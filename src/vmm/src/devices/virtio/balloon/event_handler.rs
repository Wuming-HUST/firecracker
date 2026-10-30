// Copyright 2020 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

use event_manager::{EventOps, Events, MutEventSubscriber};
use vmm_sys_util::epoll::EventSet;

use super::{DEFLATE_INDEX, INFLATE_INDEX, STATS_INDEX, report_balloon_event_fail};
use crate::devices::virtio::balloon::device::Balloon;
use crate::devices::virtio::device::VirtioDevice;
use crate::logger::{error, warn};

impl Balloon {
    const PROCESS_ACTIVATE: u32 = 0;
    const PROCESS_VIRTQ_INFLATE: u32 = 1;
    const PROCESS_VIRTQ_DEFLATE: u32 = 2;
    const PROCESS_VIRTQ_STATS: u32 = 3;
    const PROCESS_STATS_TIMER: u32 = 4;
    const PROCESS_VIRTQ_REPORTING: u32 = 5;

    fn register_runtime_events(&self, ops: &mut EventOps) {
        if let Err(err) = ops.add(Events::with_data(
            &self.queue_evts[INFLATE_INDEX],
            Self::PROCESS_VIRTQ_INFLATE,
            EventSet::IN,
        )) {
            error!("Failed to register inflate queue event: {}", err);
        }
        if let Err(err) = ops.add(Events::with_data(
            &self.queue_evts[DEFLATE_INDEX],
            Self::PROCESS_VIRTQ_DEFLATE,
            EventSet::IN,
        )) {
            error!("Failed to register deflate queue event: {}", err);
        }
        if self.stats_enabled() {
            if let Err(err) = ops.add(Events::with_data(
                &self.queue_evts[STATS_INDEX],
                Self::PROCESS_VIRTQ_STATS,
                EventSet::IN,
            )) {
                error!("Failed to register stats queue event: {}", err);
            }
            if let Err(err) = ops.add(Events::with_data(
                &self.stats_timer,
                Self::PROCESS_STATS_TIMER,
                EventSet::IN,
            )) {
                error!("Failed to register stats timerfd event: {}", err);
            }
        }
        if self.free_page_reporting() {
            if let Err(err) = ops.add(Events::with_data(
                &self.queue_evts[self.free_page_reporting_idx()],
                Self::PROCESS_VIRTQ_REPORTING,
                EventSet::IN,
            )) {
                error!(
                    "Failed to register free page reporting queue event: {}",
                    err
                );
            }
        }
    }

    fn register_activate_event(&self, ops: &mut EventOps) {
        if let Err(err) = ops.add(Events::with_data(
            &self.activate_evt,
            Self::PROCESS_ACTIVATE,
            EventSet::IN,
        )) {
            error!("Failed to register activate event: {}", err);
        }
    }

    fn process_activate_event(&self, ops: &mut EventOps) {
        if let Err(err) = self.activate_evt.read() {
            error!("Failed to consume balloon activate event: {:?}", err);
        }
        self.register_runtime_events(ops);
        if let Err(err) = ops.remove(Events::with_data(
            &self.activate_evt,
            Self::PROCESS_ACTIVATE,
            EventSet::IN,
        )) {
            error!("Failed to un-register activate event: {}", err);
        }
    }
}

impl MutEventSubscriber for Balloon {
    fn process(&mut self, event: Events, ops: &mut EventOps) {
        // Keep pending eventfds intact until resume_vm kicks the devices.
        // API state save and external RAM export span several event-loop turns.
        if self.external_snapshot_frozen {
            return;
        }
        let source = event.data();
        let event_set = event.event_set();
        let supported_events = EventSet::IN;

        if !supported_events.contains(event_set) {
            warn!(
                "Received unknown event: {:?} from source: {:?}",
                event_set, source
            );
            return;
        }

        if self.is_activated() {
            match source {
                Self::PROCESS_ACTIVATE => self.process_activate_event(ops),
                Self::PROCESS_VIRTQ_INFLATE => self
                    .process_inflate_queue_event()
                    .unwrap_or_else(report_balloon_event_fail),
                Self::PROCESS_VIRTQ_DEFLATE => self
                    .process_deflate_queue_event()
                    .unwrap_or_else(report_balloon_event_fail),
                Self::PROCESS_VIRTQ_STATS => self
                    .process_stats_queue_event()
                    .unwrap_or_else(report_balloon_event_fail),
                Self::PROCESS_STATS_TIMER => self
                    .process_stats_timer_event()
                    .unwrap_or_else(report_balloon_event_fail),
                Self::PROCESS_VIRTQ_REPORTING => self
                    .process_free_page_reporting_queue_event()
                    .unwrap_or_else(report_balloon_event_fail),
                _ => {
                    warn!("Balloon: Spurious event received: {:?}", source);
                }
            };
        } else {
            warn!(
                "Balloon: The device is not yet activated. Spurious event received: {:?}",
                source
            );
        }
    }

    fn init(&mut self, ops: &mut EventOps) {
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
}

#[cfg(test)]
pub mod tests {
    use std::sync::{Arc, Mutex};

    use event_manager::{EventManager, SubscriberOps};

    use super::*;
    use crate::devices::virtio::balloon::test_utils::set_request;
    use crate::devices::virtio::test_utils::{VirtQueue, default_mem};
    use crate::vstate::memory::{Bytes, GuestAddress};

    #[test]
    fn test_external_snapshot_freeze_preserves_reporting_event_and_ram() {
        let page_size = crate::utils::get_page_size().unwrap();
        let mem = crate::test_utils::single_region_mem(4 * page_size);
        let mut balloon = Balloon::new(0, true, 0, true, false).unwrap();
        let index = balloon.free_page_reporting_idx();
        let queue = VirtQueue::new(GuestAddress(0), &mem, 16);
        balloon.set_queue(index, queue.create_queue());
        balloon.activate(mem.clone()).unwrap();
        let page = GuestAddress(page_size as u64);
        let original = vec![0xa5; page_size];
        mem.write_slice(&original, page).unwrap();
        set_request(&queue, 0, page_size as u64, page_size as u32, 0);
        balloon.external_snapshot_frozen = true;
        balloon.queue_evts[index].write(1).unwrap();
        let balloon = Arc::new(Mutex::new(balloon));
        let mut manager = EventManager::new().unwrap();
        manager.add_subscriber(balloon.clone());

        // A level-triggered event must remain pending over successive event-loop
        // turns while external RAM export is in progress. In particular FPR may
        // neither zero the reported page nor advance the used ring.
        for _ in 0..2 {
            assert_eq!(manager.run_with_timeout(0).unwrap(), 1);
            assert_eq!(queue.used.idx.get(), 0);
            let mut observed = vec![0; page_size];
            mem.read_slice(&mut observed, page).unwrap();
            assert_eq!(observed, original);
        }

        balloon.lock().unwrap().external_snapshot_frozen = false;
        // No eventfd rewrite: successful processing proves the frozen handler
        // preserved the original event as well as the descriptor and RAM.
        assert_eq!(manager.run_with_timeout(50).unwrap(), 1);
        assert_eq!(queue.used.idx.get(), 1);
        let mut observed = vec![0xff; page_size];
        mem.read_slice(&mut observed, page).unwrap();
        assert_eq!(observed, vec![0; page_size]);
        assert_eq!(manager.run_with_timeout(0).unwrap(), 0);
    }

    #[test]
    fn test_event_handler() {
        let mut event_manager = EventManager::new().unwrap();
        let mut balloon = Balloon::new(0, true, 10, false, false).unwrap();
        let mem = default_mem();
        let infq = VirtQueue::new(GuestAddress(0), &mem, 16);
        balloon.set_queue(INFLATE_INDEX, infq.create_queue());

        let balloon = Arc::new(Mutex::new(balloon));
        let _id = event_manager.add_subscriber(balloon.clone());

        // Push a queue event, use the inflate queue in this test.
        {
            let addr = 0x100;
            set_request(&infq, 0, addr, 4, 0);
            balloon.lock().unwrap().queue_evts[INFLATE_INDEX]
                .write(1)
                .unwrap();
        }

        // EventManager should report no events since balloon has only registered
        // its activation event so far (even though there is also a queue event pending).
        let ev_count = event_manager.run_with_timeout(50).unwrap();
        assert_eq!(ev_count, 0);

        // Manually force a queue event and check it's ignored pre-activation.
        {
            let b = balloon.lock().unwrap();
            // Artificially push event.
            b.queue_evts[INFLATE_INDEX].write(1).unwrap();
            // Process the pushed event.
            let ev_count = event_manager.run_with_timeout(50).unwrap();
            // Validate there was no queue operation.
            assert_eq!(ev_count, 0);
            assert_eq!(infq.used.idx.get(), 0);
        }

        // Now activate the device.
        balloon.lock().unwrap().activate(mem.clone()).unwrap();
        // Process the activate event.
        let ev_count = event_manager.run_with_timeout(50).unwrap();
        assert_eq!(ev_count, 1);

        // Handle the previously pushed queue event through EventManager.
        event_manager
            .run_with_timeout(100)
            .expect("Metrics event timeout or error.");
        // Make sure the data queue advanced.
        assert_eq!(infq.used.idx.get(), 1);
    }
}
