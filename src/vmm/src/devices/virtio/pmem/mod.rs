// SPDX-License-Identifier: Apache-2.0
//! Read-only virtio-pmem (Linux virtio_pmem ABI, device ID 27).
mod device;
mod event_handler;
/// Immutable pmem metrics.
pub mod metrics;
/// Immutable pmem persist.
pub mod persist;
pub use device::{Pmem, PmemError, VerifiedPmemBacking};
/// Virtio specification device ID for persistent memory.
pub const TYPE_PMEM: u32 = 27;

#[cfg(test)]
mod tests;
