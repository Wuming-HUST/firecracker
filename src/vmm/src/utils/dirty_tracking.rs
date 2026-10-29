// SPDX-License-Identifier: Apache-2.0
//! Bounded, sequential bitmap operations for dirty collection experiments.
use serde::Serialize;

/// Phase times exclude API JSON serialization. Counts are not unique physical pages.
#[derive(Debug, Default, Serialize)]
#[allow(missing_docs)]
pub struct DirtyStats {
    pub policy: String,
    pub hdbss_order: u32,
    pub total_ns: u64,
    pub kvm_collect_ns: u64,
    pub kvm_ioctls: u64,
    pub vmm_collect_ns: u64,
    pub vmm_words: u64,
    pub pagemap_ns: u64,
    pub pagemap_reads: u64,
    pub pagemap_bytes: u64,
    pub pages: u64,
    pub selected_pages: u64,
    pub kernel_vmm_pages: u64,
    pub pagemap_only_pages: u64,
}

/// Merge contiguous words; amenable to LLVM vectorization, no ISA requirement.
#[inline]
pub fn or_words(dst: &mut [u64], src: &[u64]) {
    assert_eq!(dst.len(), src.len(), "dirty bitmap geometry mismatch");
    for (d, s) in dst.iter_mut().zip(src) {
        *d |= *s;
    }
}

/// Remove padding bits from a region (regions are flattened in mapping order).
pub fn mask_tail(words: &mut [u64], pages: usize) {
    if pages % 64 != 0 {
        if let Some(last) = words.last_mut() {
            *last &= (1u64 << (pages % 64)) - 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn merge_preserves_rewrites_and_tail() {
        let mut a = vec![1, 2, u64::MAX];
        or_words(&mut a, &[5, 0, 3]);
        mask_tail(&mut a, 130);
        assert_eq!(a, vec![5, 2, 3]);
        or_words(&mut a, &[1, 2, 0]);
        assert_eq!(a, vec![5, 2, 3]);
    }

    #[test]
    fn word_snapshot_does_not_clear_live_device_writes() {
        use vm_memory::bitmap::{AtomicBitmap, Bitmap};
        use vm_memory::mmap::NewBitmap;
        let live = AtomicBitmap::with_len(130 * 4096);
        live.mark_dirty(4096, 1);
        live.mark_dirty(129 * 4096, 1);
        let copy = live.clone().get_and_reset();
        assert_eq!(copy, vec![2, 0, 2]);
        assert!(live.dirty_at(4096));
        assert!(live.dirty_at(129 * 4096));
        // A retry observes both the earlier write and a later device write.
        live.mark_dirty(65 * 4096, 1);
        assert_eq!(live.clone().get_and_reset(), vec![2, 2, 2]);
    }
}
