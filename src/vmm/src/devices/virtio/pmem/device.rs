// SPDX-License-Identifier: Apache-2.0
use super::{TYPE_PMEM, metrics::METRICS};
use crate::devices::virtio::ActivateError;
use crate::devices::virtio::device::{DeviceState, IrqTrigger, IrqType, VirtioDevice};
use crate::devices::virtio::generated::virtio_config::VIRTIO_F_VERSION_1;
use crate::devices::virtio::queue::{DescriptorChain, FIRECRACKER_MAX_QUEUE_SIZE, Queue};
use crate::logger::IncMetric;
use crate::vmm_config::pmem::PmemConfig;
use crate::vstate::memory::{
    Bytes, FileOffset, GuestAddress, GuestMemoryMmap, GuestRegionMmap, MmapRegionBuilder,
};
use aws_lc_rs::digest::{Context, SHA256};
use std::fs::File;
use std::io::{self, Read};
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, atomic::Ordering};
use vmm_sys_util::eventfd::EventFd;

#[derive(Debug, thiserror::Error)]
/// Failures creating or completing an immutable pmem device.
pub enum PmemError {
    #[error("invalid pmem binding: {0}")]
    Invalid(String),
    #[error("pmem I/O: {0}")]
    Io(#[from] io::Error),
    #[error("pmem mmap: {0}")]
    Mmap(#[from] vm_memory::mmap::MmapRegionError),
    #[error("pmem memory: {0}")]
    Memory(#[from] vm_memory::Error),
    #[error("pmem queue completion failed: {0}")]
    Completion(String),
    #[error("pmem virtio state: {0}")]
    Virtio(#[from] crate::devices::virtio::persist::PersistError),
}

/// A verified immutable file mapping; fields cannot be forged by callers.
#[derive(Clone, Debug)]
pub struct VerifiedPmemBacking {
    pub(super) config: PmemConfig,
    pub(super) region: Arc<GuestRegionMmap>,
}

#[derive(Debug)]
/// Immutable virtio-pmem device with a readonly device window.
pub struct Pmem {
    pub(crate) config: PmemConfig,
    pub(crate) region: Arc<GuestRegionMmap>,
    pub(crate) avail_features: u64,
    pub(crate) acked_features: u64,
    pub(crate) activate_event: EventFd,
    pub(crate) queue_events: Vec<EventFd>,
    pub(crate) queues: Vec<Queue>,
    pub(crate) irq_trigger: IrqTrigger,
    pub(crate) device_state: DeviceState,
    pub(crate) external_snapshot_frozen: bool,
}

impl Pmem {
    /// Immutable pmem new.
    pub fn new(config: PmemConfig) -> Result<Self, PmemError> {
        config.validate().map_err(PmemError::Invalid)?;
        let mut file = File::open(&config.path_on_host)?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.permissions().mode() & 0o222 != 0
            || config
                .file_offset
                .checked_add(config.size)
                .filter(|end| *end <= metadata.len())
                .is_none()
        {
            return Err(PmemError::Invalid(
                "backing must be a complete regular file containing the full window".into(),
            ));
        }
        // Verify the entire immutable object using the same opened fd used by mmap.
        let mut hash = Context::new(&SHA256);
        let mut buffer = [0u8; 256 * 1024];
        loop {
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            hash.update(&buffer[..n]);
        }
        let digest = hash
            .finish()
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        if digest != config.content_id {
            return Err(PmemError::Invalid("content SHA256 mismatch".into()));
        }
        let region = MmapRegionBuilder::new_with_bitmap(config.size as usize, None)
            .with_mmap_prot(libc::PROT_READ)
            .with_mmap_flags(libc::MAP_SHARED)
            .with_file_offset(FileOffset::new(file, config.file_offset))
            .build()?;
        let region = Arc::new(GuestRegionMmap::new(
            region,
            GuestAddress(config.guest_address),
        )?);
        Self::from_backing(VerifiedPmemBacking { config, region })
    }

    pub(super) fn from_backing(backing: VerifiedPmemBacking) -> Result<Self, PmemError> {
        let VerifiedPmemBacking { config, region } = backing;
        Ok(Self {
            config,
            region,
            avail_features: 1 << VIRTIO_F_VERSION_1,
            acked_features: 0,
            activate_event: EventFd::new(libc::EFD_NONBLOCK)?,
            queue_events: vec![EventFd::new(libc::EFD_NONBLOCK)?],
            queues: vec![Queue::new(FIRECRACKER_MAX_QUEUE_SIZE)],
            irq_trigger: IrqTrigger::new()?,
            device_state: DeviceState::Inactive,
            external_snapshot_frozen: false,
        })
    }
    /// Immutable pmem config.
    pub fn config(&self) -> &PmemConfig {
        &self.config
    }
    /// Immutable pmem region.
    pub(crate) fn region(&self) -> Arc<GuestRegionMmap> {
        self.region.clone()
    }

    /// Reuse the exact verified mmap/slot when restoring the virtio state.
    pub fn backing(&self) -> VerifiedPmemBacking {
        VerifiedPmemBacking {
            config: self.config.clone(),
            region: self.region.clone(),
        }
    }

    fn complete_request(mem: &GuestMemoryMmap, head: DescriptorChain) -> Result<u32, ()> {
        // Linux submits one le32 request followed by one writable le32 response.
        if head.is_write_only() || head.len != 4 || !head.has_next() {
            return Err(());
        }
        let response = head.next_descriptor().ok_or(())?;
        if !response.is_write_only() || response.len != 4 || response.has_next() {
            return Err(());
        }
        let request: u32 = mem.read_obj(head.addr).map_err(|_| ())?;
        // Immutable mappings never accept persistence writes. A flush is a barrier
        // before acknowledgement, not a claim to persist private COW contents.
        std::sync::atomic::fence(Ordering::SeqCst);
        let ret = if u32::from_le(request) == 0 {
            METRICS.flushes.inc();
            0u32
        } else {
            METRICS.malformed_requests.inc();
            1u32
        };
        mem.write_obj(ret.to_le(), response.addr).map_err(|_| ())?;
        Ok(4)
    }

    /// Immutable pmem process virtio queues.
    pub fn process_virtio_queues(&mut self) -> Result<(), PmemError> {
        if self.external_snapshot_frozen {
            return Ok(());
        }
        let DeviceState::Activated(ref mem) = self.device_state else {
            return Ok(());
        };
        let mut used = false;
        // The queue parser bounds the chain walk by queue size. Malformed chains
        // are completed with length zero, without touching their data buffer.
        while let Some(head) = self.queues[0].pop() {
            let len = Self::complete_request(mem, head).unwrap_or_else(|()| {
                METRICS.malformed_requests.inc();
                0
            });
            self.queues[0].add_used(head.index, len).map_err(|err| {
                METRICS.event_errors.inc();
                PmemError::Completion(err.to_string())
            })?;
            used = true;
        }
        if used {
            self.queues[0].advance_used_ring_idx();
            self.irq_trigger
                .trigger_irq(IrqType::Vring)
                .map_err(|err| {
                    METRICS.event_errors.inc();
                    PmemError::Completion(err.to_string())
                })?;
        }
        Ok(())
    }

    pub(crate) fn prepare_save(&mut self) -> Result<(), PmemError> {
        let frozen = self.external_snapshot_frozen;
        self.external_snapshot_frozen = false;
        let result = self.process_virtio_queues();
        self.external_snapshot_frozen = frozen;
        result
    }
}

impl VirtioDevice for Pmem {
    fn device_type(&self) -> u32 {
        TYPE_PMEM
    }
    fn queues(&self) -> &[Queue] {
        &self.queues
    }
    fn queues_mut(&mut self) -> &mut [Queue] {
        &mut self.queues
    }
    fn queue_events(&self) -> &[EventFd] {
        &self.queue_events
    }
    fn interrupt_trigger(&self) -> &IrqTrigger {
        &self.irq_trigger
    }
    fn avail_features(&self) -> u64 {
        self.avail_features
    }
    fn acked_features(&self) -> u64 {
        self.acked_features
    }
    fn set_acked_features(&mut self, bits: u64) {
        self.acked_features = bits;
    }
    fn read_config(&self, offset: u64, data: &mut [u8]) {
        let mut bytes = [0u8; 16];
        bytes[..8].copy_from_slice(&self.config.guest_address.to_le_bytes());
        bytes[8..].copy_from_slice(&self.config.size.to_le_bytes());
        for (i, value) in data.iter_mut().enumerate() {
            *value = offset
                .checked_add(i as u64)
                .and_then(|at| bytes.get(at as usize))
                .copied()
                .unwrap_or(0);
        }
    }
    fn write_config(&mut self, _offset: u64, _data: &[u8]) {}
    fn is_activated(&self) -> bool {
        self.device_state.is_activated()
    }
    fn activate(&mut self, mem: GuestMemoryMmap) -> Result<(), ActivateError> {
        for queue in &mut self.queues {
            queue
                .initialize(&mem)
                .map_err(ActivateError::QueueMemoryError)?;
        }
        self.activate_event
            .write(1)
            .map_err(|_| ActivateError::EventFd)?;
        self.device_state = DeviceState::Activated(mem);
        Ok(())
    }
    fn reset(&mut self) -> Option<(EventFd, Vec<EventFd>)> {
        // Keep registered eventfds alive; the transport resets the queue pointers.
        let irq = self.irq_trigger.irq_evt.try_clone().ok()?;
        let queue = self.queue_events[0].try_clone().ok()?;
        self.device_state = DeviceState::Inactive;
        self.acked_features = 0;
        self.irq_trigger.irq_status.store(0, Ordering::SeqCst);
        Some((irq, vec![queue]))
    }
}
