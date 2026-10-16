// SPDX-License-Identifier: Apache-2.0
//! openEuler HIP12 HDBSS. Explicit opt-in; no silent fallback for treatment runs.
use crate::vstate::{
    kvm::Kvm,
    vm::{Vm, VmError},
};

static SOFTWARE_BITMAP: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
static ORDER: std::sync::OnceLock<u32> = std::sync::OnceLock::new();

/// Set immutable process policy before starting threads.
pub fn set_order(value: Option<&str>, software_bitmap: bool) -> Result<(), VmError> {
    let order = parse_order(value)?;
    SOFTWARE_BITMAP.set(software_bitmap).map_err(|_| {
        VmError::InvalidMemoryConfiguration("bitmap policy already initialized".into())
    })?;
    ORDER
        .set(order)
        .map_err(|_| VmError::InvalidMemoryConfiguration("HDBSS policy already initialized".into()))
}

/// Parse a bounded host buffer order; zero disables HDBSS.
pub fn parse_order(value: Option<&str>) -> Result<u32, VmError> {
    let value = value.unwrap_or("0");
    let order = value.parse::<u32>().map_err(|_| {
        VmError::InvalidMemoryConfiguration("--hdbss-order must be an integer in 0..=9".into())
    })?;
    if order > 9 {
        return Err(VmError::InvalidMemoryConfiguration(
            "HDBSS order exceeds 9".into(),
        ));
    }
    Ok(order)
}

/// Called exactly once, after every vCPU is created, before guest execution.
pub fn configure(kvm: &Kvm, vm: &mut Vm) -> Result<(), VmError> {
    let order = *ORDER.get().unwrap_or(&0);
    vm.common.track_dirty_export = order != 0 || *SOFTWARE_BITMAP.get().unwrap_or(&false);
    if vm.common.track_dirty_export && crate::arch::host_page_size() != 4096 {
        return Err(VmError::InvalidMemoryConfiguration(
            "dirty export requires 4KiB host pages".into(),
        ));
    }
    if order == 0 {
        return Ok(());
    }
    enable(kvm, vm, order)
}

#[cfg(target_arch = "aarch64")]
fn enable(kvm: &Kvm, vm: &mut Vm, order: u32) -> Result<(), VmError> {
    const CAP: u32 = 502; // openEuler KVM_CAP_ARM_HW_DIRTY_STATE_TRACK
    if crate::arch::host_page_size() != 4096 || kvm.fd.check_extension_raw(CAP.into()) != 1 {
        return Err(VmError::InvalidMemoryConfiguration(
            "HDBSS requires 4KiB host pages and openEuler capability 502".into(),
        ));
    }
    let mut cap = kvm_bindings::kvm_enable_cap {
        cap: CAP,
        ..Default::default()
    };
    cap.args[0] = order.into();
    use std::os::fd::AsRawFd;
    // kvm-ioctls exposes VmFd::enable_cap only on x86 in this pinned version.
    // SAFETY: KVM_ENABLE_CAP takes a valid kvm_enable_cap pointer; fd is a VM fd.
    let result = unsafe { libc::ioctl(vm.fd().as_raw_fd(), 0x4068_aea3, &cap) };
    if result < 0 {
        return Err(VmError::InvalidMemoryConfiguration(format!(
            "HDBSS enable failed: {}",
            std::io::Error::last_os_error()
        )));
    }
    vm.common.hdbss_order = order;
    vm.common.track_dirty_export = true;
    crate::logger::info!(
        "HDBSS enabled: order={}, bytes_per_vcpu={}; bitmap mode",
        order,
        4096u64 << order
    );
    Ok(())
}

#[cfg(not(target_arch = "aarch64"))]
fn enable(_kvm: &Kvm, _vm: &mut Vm, _order: u32) -> Result<(), VmError> {
    Err(VmError::InvalidMemoryConfiguration(
        "HDBSS requires aarch64".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_arch = "aarch64")]
    #[test]
    #[ignore = "requires openEuler capability 502 and /dev/kvm access"]
    fn hdbss_vm_initialization() {
        let kvm = Kvm::new(vec![]).unwrap();
        let mut vm = Vm::new(&kvm).unwrap();
        let (_vcpus, _event) = vm.create_vcpus(1).unwrap();
        enable(&kvm, &mut vm, 1).unwrap();
        assert_eq!(vm.common.hdbss_order, 1);
        use crate::vmm_config::machine_config::HugePageConfig;
        use crate::vstate::memory::{self, Bytes, GuestAddress, GuestMemoryExtension};
        let untracked = memory::anonymous(
            [(GuestAddress(0x4000_0000), 0x4000)].into_iter(),
            false,
            HugePageConfig::None,
        )
        .unwrap();
        assert!(vm.register_memory_regions(untracked).is_err());
        let tracked = memory::anonymous(
            [(GuestAddress(0x4000_0000), 0x4000)].into_iter(),
            true,
            HugePageConfig::None,
        )
        .unwrap();
        vm.register_memory_regions(tracked).unwrap();
        // Device-model writes must survive repeated kernel-log harvests and
        // userspace bitmap reset, independently of guest hardware writes.
        vm.guest_memory()
            .write_slice(&[0x55], GuestAddress(0x4000_1000))
            .unwrap();
        assert_eq!(vm.get_dirty_bitmap().unwrap()[&0][0] & 2, 2);
        vm.guest_memory().reset_dirty();
        assert_eq!(vm.get_dirty_bitmap().unwrap()[&0][0] & 2, 2);
    }

    #[test]
    fn rejects_invalid_orders() {
        for s in ["", "-1", "10", "4294967296", "true", "1;exit"] {
            assert!(parse_order(Some(s)).is_err());
        }
        assert_eq!(parse_order(None).unwrap(), 0);
        assert_eq!(parse_order(Some("1")).unwrap(), 1);
        assert_eq!(parse_order(Some("9")).unwrap(), 9);
    }
}
