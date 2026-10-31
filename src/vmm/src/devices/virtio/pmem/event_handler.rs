// SPDX-License-Identifier: Apache-2.0
use super::{Pmem, metrics::METRICS};
use crate::devices::virtio::device::VirtioDevice;
use crate::logger::{IncMetric, error};
use event_manager::{EventOps, Events, MutEventSubscriber};
use vmm_sys_util::epoll::EventSet;
impl MutEventSubscriber for Pmem {
    fn init(&mut self, ops: &mut EventOps) {
        // Always watch activation and queue events, including after reset.
        for (fd, key) in [(&self.activate_event, 0), (&self.queue_events[0], 1)] {
            if let Err(e) = ops.add(Events::with_data(fd, key, EventSet::IN)) {
                error!("pmem event registration: {e}");
                METRICS.event_errors.inc();
            }
        }
    }
    fn process(&mut self, event: Events, _ops: &mut EventOps) {
        if self.external_snapshot_frozen {
            return;
        }
        if event.event_set() != EventSet::IN {
            METRICS.event_errors.inc();
            return;
        }
        match event.data() {
            0 => {
                if self.activate_event.read().is_err() {
                    METRICS.event_errors.inc();
                }
            }
            1 => {
                if self.queue_events[0].read().is_err() {
                    METRICS.event_errors.inc();
                } else if self.is_activated() {
                    if let Err(e) = self.process_virtio_queues() {
                        error!("pmem completion: {e}");
                    }
                }
            }
            _ => {
                METRICS.event_errors.inc();
            }
        }
    }
}
