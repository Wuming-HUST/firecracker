// Copyright 2022 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

use event_manager::{EventOps, Events, MutEventSubscriber};
use vmm_sys_util::epoll::EventSet;

use super::{Entropy, RNG_QUEUE};
use crate::devices::virtio::device::VirtioDevice;
use crate::logger::{error, warn};

impl Entropy {
    const PROCESS_ACTIVATE: u32 = 0;
    const PROCESS_ENTROPY_QUEUE: u32 = 1;
    const PROCESS_RATE_LIMITER: u32 = 2;

    fn register_runtime_events(&self, ops: &mut EventOps) {
        if let Err(err) = ops.add(Events::with_data(
            &self.queue_events()[RNG_QUEUE],
            Self::PROCESS_ENTROPY_QUEUE,
            EventSet::IN,
        )) {
            error!("entropy: Failed to register queue event: {err}");
        }
        if let Err(err) = ops.add(Events::with_data(
            self.rate_limiter(),
            Self::PROCESS_RATE_LIMITER,
            EventSet::IN,
        )) {
            error!("entropy: Failed to register rate-limiter event: {err}");
        }
    }

    fn register_activate_event(&self, ops: &mut EventOps) {
        if let Err(err) = ops.add(Events::with_data(
            self.activate_event(),
            Self::PROCESS_ACTIVATE,
            EventSet::IN,
        )) {
            error!("entropy: Failed to register activate event: {err}");
        }
    }

    fn process_activate_event(&self, ops: &mut EventOps) {
        if let Err(err) = self.activate_event().read() {
            error!("entropy: Failed to consume activate event: {err}");
        }

        // Register runtime events
        self.register_runtime_events(ops);

        // Remove activate event
        if let Err(err) = ops.remove(Events::with_data(
            self.activate_event(),
            Self::PROCESS_ACTIVATE,
            EventSet::IN,
        )) {
            error!("entropy: Failed to un-register activate event: {err}");
        }
    }
}

impl MutEventSubscriber for Entropy {
    fn init(&mut self, ops: &mut event_manager::EventOps) {
        // This function can be called during different points in the device lifetime:
        //  - shortly after device creation,
        //  - on device activation (is-activated already true at this point),
        //  - on device restore from snapshot.
        if self.is_activated() {
            self.register_runtime_events(ops);
        } else {
            self.register_activate_event(ops);
        }
    }

    fn process(&mut self, events: event_manager::Events, ops: &mut event_manager::EventOps) {
        // Keep pending eventfds intact until resume_vm kicks the devices.
        // API state save and external RAM export span several event-loop turns.
        if self.external_snapshot_frozen {
            return;
        }
        let event_set = events.event_set();
        let source = events.data();

        if !event_set.contains(EventSet::IN) {
            warn!("entropy: Received unknown event: {event_set:?} from source {source}");
            return;
        }

        if !self.is_activated() {
            warn!("entropy: The device is not activated yet. Spurious event received: {source}");
            return;
        }

        match source {
            Self::PROCESS_ACTIVATE => self.process_activate_event(ops),
            Self::PROCESS_ENTROPY_QUEUE => self.process_entropy_queue_event(),
            Self::PROCESS_RATE_LIMITER => self.process_rate_limiter_event(),
            _ => {
                warn!("entropy: Unknown event received: {source}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use event_manager::{EventManager, SubscriberOps};

    use super::*;
    use crate::devices::virtio::queue::VIRTQ_DESC_F_WRITE;
    use crate::devices::virtio::test_utils::{VirtQueue, default_mem};
    use crate::rate_limiter::RateLimiter;
    use crate::vstate::memory::{Bytes, GuestAddress};

    #[test]
    fn test_external_snapshot_freeze_preserves_entropy_event_and_ram() {
        let mem = default_mem();
        let queue = VirtQueue::new(GuestAddress(0), &mem, 16);
        let mut entropy = Entropy::new(RateLimiter::default()).unwrap();
        entropy.queues_mut()[RNG_QUEUE] = queue.create_queue();
        entropy.activate(mem.clone()).unwrap();
        let payload_addr = GuestAddress(0x4000);
        let original = [0xa5; 64];
        mem.write_slice(&original, payload_addr).unwrap();
        queue.dtable[0].set(0x4000, original.len() as u32, VIRTQ_DESC_F_WRITE, 0);
        queue.avail.ring[0].set(0);
        queue.avail.idx.set(1);
        entropy.external_snapshot_frozen = true;
        entropy.queue_events()[RNG_QUEUE].write(1).unwrap();
        let entropy = Arc::new(Mutex::new(entropy));
        let mut manager = EventManager::new().unwrap();
        manager.add_subscriber(entropy.clone());

        for _ in 0..2 {
            assert_eq!(manager.run_with_timeout(0).unwrap(), 1);
            assert_eq!(queue.used.idx.get(), 0);
            let mut observed = [0; 64];
            mem.read_slice(&mut observed, payload_addr).unwrap();
            assert_eq!(observed, original);
        }

        entropy.lock().unwrap().external_snapshot_frozen = false;
        assert_eq!(manager.run_with_timeout(50).unwrap(), 1);
        assert_eq!(queue.used.idx.get(), 1);
        // Check completed length rather than random byte values: successful
        // entropy completion must account for all requested guest writes.
        assert_eq!(queue.used.ring[0].get().len, original.len() as u32);
        assert_eq!(manager.run_with_timeout(0).unwrap(), 0);
    }
}
