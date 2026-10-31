// SPDX-License-Identifier: Apache-2.0
use super::{Pmem, PmemError, TYPE_PMEM, VerifiedPmemBacking};
use crate::devices::virtio::device::DeviceState;
use crate::devices::virtio::persist::VirtioDeviceState;
use crate::devices::virtio::queue::FIRECRACKER_MAX_QUEUE_SIZE;
use crate::snapshot::Persist;
use crate::vmm_config::pmem::PmemLayout;
use crate::vstate::memory::GuestMemoryMmap;
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Serialize, Deserialize)]
/// Persisted virtio-pmem queue state and immutable layout.
pub struct PmemState {
    /// Layout.
    pub layout: PmemLayout,
    /// Virtio state.
    pub virtio_state: VirtioDeviceState,
}
/// Validated node-local binding and RAM used when restoring.
#[derive(Debug)]
pub struct PmemConstructorArgs {
    /// Ordinary RAM with the registered readonly device window.
    pub mem: GuestMemoryMmap,
    /// The exact previously verified mapping used for KVM registration.
    pub backing: VerifiedPmemBacking,
}
impl Persist<'_> for Pmem {
    type State = PmemState;
    type ConstructorArgs = PmemConstructorArgs;
    type Error = PmemError;
    fn save(&self) -> PmemState {
        PmemState {
            layout: self.config.layout(),
            virtio_state: VirtioDeviceState::from_device(self),
        }
    }
    fn restore(args: PmemConstructorArgs, state: &PmemState) -> Result<Self, PmemError> {
        if args.backing.config.layout() != state.layout {
            return Err(PmemError::Invalid(
                "snapshot lower identity/layout differs from binding".into(),
            ));
        }
        let queues = state.virtio_state.build_queues_checked(
            &args.mem,
            TYPE_PMEM,
            1,
            FIRECRACKER_MAX_QUEUE_SIZE,
        )?;
        let mut dev = Self::from_backing(args.backing)?;
        dev.queues = queues;
        dev.avail_features = state.virtio_state.avail_features;
        if dev.avail_features
            != 1 << crate::devices::virtio::generated::virtio_config::VIRTIO_F_VERSION_1
            || state.virtio_state.acked_features & !dev.avail_features != 0
        {
            return Err(PmemError::Invalid(
                "unsupported pmem snapshot features".into(),
            ));
        }
        dev.acked_features = state.virtio_state.acked_features;
        dev.irq_trigger.irq_status.store(
            state.virtio_state.interrupt_status,
            std::sync::atomic::Ordering::SeqCst,
        );
        if state.virtio_state.activated {
            dev.device_state = DeviceState::Activated(args.mem);
        }
        Ok(dev)
    }
}
