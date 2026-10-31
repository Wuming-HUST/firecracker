// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::devices::virtio::block::virtio::io::sync_io::SyncFileEngine;
use crate::devices::virtio::device::VirtioDevice;
use crate::devices::virtio::test_utils::{VirtQueue, default_mem};
use crate::snapshot::Persist;
use crate::vmm_config::pmem::{PMEM_ALIGNMENT, PmemConfig};
use crate::vstate::memory::{Bytes, GuestAddress, GuestMemory, GuestMemoryExtension};
use aws_lc_rs::digest::{Context, SHA256};
use std::io::Read;
use std::os::unix::fs::{FileExt, PermissionsExt};
use vmm_sys_util::tempfile::TempFile;

fn backing() -> (TempFile, PmemConfig) {
    let f = TempFile::new().unwrap();
    f.as_file().set_len(PMEM_ALIGNMENT).unwrap();
    f.as_file().write_all_at(b"immutable-pmem", 0).unwrap();
    let mut hash = Context::new(&SHA256);
    let mut read = f.as_file().try_clone().unwrap();
    let mut buf = [0u8; 262144];
    loop {
        let n = read.read(&mut buf).unwrap();
        if n == 0 {
            break;
        };
        hash.update(&buf[..n]);
    }
    let digest = hash
        .finish()
        .as_ref()
        .iter()
        .map(|v| format!("{v:02x}"))
        .collect();
    f.as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o444))
        .unwrap();
    let c = PmemConfig {
        pmem_id: "rootfs-lower".into(),
        path_on_host: f.as_path().into(),
        content_id: digest,
        guest_address: 1 << 38,
        size: PMEM_ALIGNMENT,
        file_offset: 0,
        read_only: true,
    };
    (f, c)
}

#[test]
fn readonly_sources_and_mutable_destinations_are_separate() {
    let (_file, c) = backing();
    let device = Pmem::new(c.clone()).unwrap();
    let mut mem = default_mem();
    mem.add_readonly_region(device.region());
    assert_eq!(mem.num_regions(), 1);
    assert_eq!(mem.describe().regions.len(), 1);
    let addr = GuestAddress(c.guest_address);
    assert!(mem.get_host_address(addr).is_err());
    assert!(mem.get_slice(addr, 1).is_err());
    assert!(mem.write_obj(0u8, addr).is_err());
    assert!(
        mem.get_read_slice(GuestAddress(c.guest_address + c.size - 1), 2)
            .is_err()
    );
    assert!(mem.get_read_slice(GuestAddress(u64::MAX), 2).is_err());
    let output = TempFile::new().unwrap();
    let mut engine = SyncFileEngine::from_file(output.as_file().try_clone().unwrap());
    assert_eq!(engine.write(0, &mem, addr, 14).unwrap(), 14);
    assert!(engine.read(0, &mem, addr, 14).is_err());
    let mut bytes = [0u8; 14];
    output.as_file().read_exact_at(&mut bytes, 0).unwrap();
    assert_eq!(&bytes, b"immutable-pmem");
    // TX (also used by vsock) can source pmem; RX cannot get a mutable slice.
    let q = VirtQueue::new(GuestAddress(0), &mem, 8);
    q.dtable[0].set(addr.0, 14, 0, 0);
    q.avail.ring[0].set(0);
    q.avail.idx.set(1);
    let mut queue = q.create_queue();
    let head = queue.pop().unwrap();
    let iov =
        unsafe { crate::devices::virtio::iovec::IoVecBuffer::from_descriptor_chain(&mem, head) }
            .unwrap();
    let mut output = &mut bytes[..];
    iov.read_exact_volatile_at(&mut output, 0).unwrap();
    assert_eq!(&bytes, b"immutable-pmem");
}

#[test]
fn flush_queue_malformed_response_frozen_capture_and_reset() {
    let (_file, c) = backing();
    let mut device = Pmem::new(c).unwrap();
    let mem = default_mem();
    let q = VirtQueue::new(GuestAddress(0), &mem, 8);
    q.dtable[0].set(0x4000, 4, 1, 1);
    q.dtable[1].set(0x5000, 4, 2, 0);
    mem.write_obj(0u32, GuestAddress(0x4000)).unwrap();
    mem.write_obj(99u32, GuestAddress(0x5000)).unwrap();
    q.avail.ring[0].set(0);
    q.avail.idx.set(1);
    device.queues[0] = q.create_queue();
    device.queues[0].max_size = crate::devices::virtio::queue::FIRECRACKER_MAX_QUEUE_SIZE;
    device.activate(mem.clone()).unwrap();
    device.external_snapshot_frozen = true;
    device.process_virtio_queues().unwrap();
    assert_eq!(q.used.idx.get(), 0);
    device.prepare_save().unwrap();
    assert!(device.external_snapshot_frozen);
    q.check_used_elem(0, 0, 4);
    assert_eq!(mem.read_obj::<u32>(GuestAddress(0x5000)).unwrap(), 0);
    // No duplicate completion across capture and restore.
    let saved = device.save();
    let mut restored = Pmem::restore(
        persist::PmemConstructorArgs {
            mem: mem.clone(),
            backing: device.backing(),
        },
        &saved,
    )
    .unwrap();
    restored.process_virtio_queues().unwrap();
    assert_eq!(q.used.idx.get(), 1);
    assert!(std::sync::Arc::ptr_eq(&device.region(), &restored.region()));
    // Unknown request receives le32 error, malformed response length never writes.
    mem.write_obj(9u32, GuestAddress(0x4000)).unwrap();
    q.avail.ring[1].set(0);
    q.avail.idx.set(2);
    restored.process_virtio_queues().unwrap();
    assert_eq!(mem.read_obj::<u32>(GuestAddress(0x5000)).unwrap(), 1);
    q.dtable[1].len.set(3);
    q.avail.ring[2].set(0);
    q.avail.idx.set(3);
    restored.process_virtio_queues().unwrap();
    q.check_used_elem(2, 0, 0);
    assert!(restored.reset().is_some());
    assert!(!restored.is_activated());
    assert_eq!(restored.acked_features(), 0);
    let mut cfg = [0; 20];
    restored.read_config(0, &mut cfg);
    assert_eq!(u64::from_le_bytes(cfg[..8].try_into().unwrap()), 1 << 38);
    assert_eq!(&cfg[16..], &[0; 4]);
    restored.write_config(0, &[255; 16]);
    let mut same = [0; 20];
    restored.read_config(0, &mut same);
    assert_eq!(cfg, same);
}

#[test]
fn rejects_wrong_hash_truncation_rebinding_and_host_path_is_not_saved() {
    let (file, c) = backing();
    let device = Pmem::new(c.clone()).unwrap();
    let state = device.save();
    let saved = serde_json::to_string(&state).unwrap();
    assert!(!saved.contains(file.as_path().to_str().unwrap()));
    let mut wrong = c.clone();
    wrong.guest_address += PMEM_ALIGNMENT;
    assert!(
        Pmem::restore(
            persist::PmemConstructorArgs {
                mem: default_mem(),
                backing: Pmem::new(wrong).unwrap().backing()
            },
            &state
        )
        .is_err()
    );
    let mut wrong = c.clone();
    wrong.content_id = "0".repeat(64);
    assert!(Pmem::new(wrong).is_err());
    file.as_file().set_len(4096).unwrap();
    assert!(Pmem::new(c).is_err());
}

#[test]
fn malformed_flush_chains_never_write_device_windows() {
    let (_file, c) = backing();
    let mut device = Pmem::new(c.clone()).unwrap();
    let mut mem = default_mem();
    mem.add_readonly_region(device.region());
    for (req_addr, req_len, req_flags, resp_addr, resp_len, resp_flags) in [
        (0x4000, 4, 3, 0x5000, 4, 2),
        (0x4000, 3, 1, 0x5000, 4, 2),
        (0x4000, 4, 1, 0x5000, 4, 0),
        (0x4000, 4, 1, 0x5000, 4, 3),
        (0x4000, 4, 1, u64::MAX, 4, 2),
        (0x4000, 4, 1, c.guest_address, 4, 2),
        (c.guest_address, 4, 1, 0x5000, 4, 2),
    ] {
        let q = VirtQueue::new(GuestAddress(0), &mem, 8);
        q.dtable[0].set(req_addr, req_len, req_flags, 1);
        q.dtable[1].set(resp_addr, resp_len, resp_flags, 0);
        mem.write_obj(0u32, GuestAddress(0x4000)).unwrap();
        mem.write_obj(99u32, GuestAddress(0x5000)).unwrap();
        q.avail.ring[0].set(0);
        q.avail.idx.set(1);
        device.queues[0] = q.create_queue();
        device.queues[0].max_size = crate::devices::virtio::queue::FIRECRACKER_MAX_QUEUE_SIZE;
        device.activate(mem.clone()).unwrap();
        device.process_virtio_queues().unwrap();
        q.check_used_elem(0, 0, 0);
        assert_eq!(mem.read_obj::<u32>(GuestAddress(0x5000)).unwrap(), 99);
    }
}

#[test]
fn snapshot_drain_propagates_interrupt_failure_and_keeps_frozen() {
    let (_file, c) = backing();
    let mut device = Pmem::new(c).unwrap();
    let mem = default_mem();
    let q = VirtQueue::new(GuestAddress(0), &mem, 8);
    q.dtable[0].set(0x4000, 4, 1, 1);
    q.dtable[1].set(0x5000, 4, 2, 0);
    q.avail.ring[0].set(0);
    q.avail.idx.set(1);
    device.queues[0] = q.create_queue();
    device.queues[0].max_size = crate::devices::virtio::queue::FIRECRACKER_MAX_QUEUE_SIZE;
    device.activate(mem.clone()).unwrap();
    device.external_snapshot_frozen = true;
    device.irq_trigger.irq_evt.write(u64::MAX - 1).unwrap();
    assert!(device.prepare_save().is_err());
    assert!(device.external_snapshot_frozen);
    assert_eq!(q.used.idx.get(), 1);
}
