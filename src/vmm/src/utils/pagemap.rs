//! Utilities for reading /proc/self/pagemap to track dirty pages.

#![allow(clippy::cast_possible_wrap)]

use std::fs::File;
use std::os::unix::io::AsRawFd;

use crate::arch::host_page_size;

const PAGEMAP_ENTRY_SIZE: usize = 8;

/// Errors related to pagemap operations
#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum PagemapError {
    /// Failed to open /proc/self/pagemap: {0}
    OpenPagemap(#[source] std::io::Error),
    /// Failed to read pagemap entry: {0}
    ReadEntry(#[source] std::io::Error),
    /// Failed to open /proc/self/clear_refs: {0}
    OpenClearRefs(#[source] std::io::Error),
    /// Failed to clear soft-dirty bits: {0}
    ClearSoftDirty(#[source] std::io::Error),
}

/// Represents a single entry in /proc/pid/pagemap.
///
/// Each virtual page has an 8-byte entry with the following layout:
/// - Bits 0-54:  Page frame number (PFN) if present
/// - Bit 55:     Page is soft-dirty (written to since last clear)
/// - Bit 56:     Page is exclusively mapped
/// - Bit 57:     Page is write-protected via userfaultfd
/// - Bit 58:     Unused
/// - Bit 59-60:  Unused
/// - Bit 61:     Page is file-page or shared-anon
/// - Bit 62:     Page is swapped
/// - Bit 63:     Page is present in RAM
#[derive(Debug, Clone, Copy)]
pub struct PagemapEntry {
    raw: u64,
}

impl PagemapEntry {
    /// Create a PagemapEntry from bytes (little-endian)
    pub fn from_bytes(bytes: [u8; 8]) -> Self {
        Self {
            raw: u64::from_ne_bytes(bytes),
        }
    }

    /// Check if page is write-protected via userfaultfd
    pub fn is_write_protected(&self) -> bool {
        (self.raw & (1u64 << 57)) != 0
    }

    /// Check if page is present in RAM (bit 63)
    pub fn is_present(&self) -> bool {
        (self.raw & (1u64 << 63)) != 0
    }
}

/// Reader for /proc/self/pagemap
#[derive(Debug)]
pub struct PagemapReader {
    pagemap_fd: File,
}

impl PagemapReader {
    /// Create a new PagemapReader
    pub fn new(_page_size: usize) -> Result<Self, PagemapError> {
        let pagemap_fd = File::open("/proc/self/pagemap").map_err(PagemapError::OpenPagemap)?;

        Ok(Self { pagemap_fd })
    }

    /// Scan in 4096-entry / 32 KiB chunks. The active mask/output words and scan
    /// buffer fit in a 64 KiB L1D. Skip wholly absent chunks.
    pub fn scan_batched(
        &self,
        base: usize,
        pages: usize,
        page_size: usize,
        resident: &[u64],
    ) -> Result<(Vec<u64>, u64, u64), PagemapError> {
        use std::os::unix::fs::FileExt;
        let mut out = vec![0; pages.div_ceil(64)];
        let mut buffer = [0u8; 4096 * PAGEMAP_ENTRY_SIZE];
        let mut reads = 0;
        let mut bytes = 0;
        for first in (0..pages).step_by(4096) {
            let count = (pages - first).min(4096);
            if resident[first / 64..(first + count).div_ceil(64)]
                .iter()
                .all(|x| *x == 0)
            {
                continue;
            }
            // Only 4KiB batching is supported: entries must be consecutive host pages.
            if page_size != host_page_size() {
                return Err(PagemapError::ReadEntry(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "batched scan requires host page size",
                )));
            }
            let n = count * PAGEMAP_ENTRY_SIZE;
            self.pagemap_fd
                .read_exact_at(
                    &mut buffer[..n],
                    ((base / page_size + first) * PAGEMAP_ENTRY_SIZE) as u64,
                )
                .map_err(PagemapError::ReadEntry)?;
            reads += 1;
            bytes += n as u64;
            for (j, entry) in buffer[..n].chunks_exact(8).enumerate() {
                let page = first + j;
                let raw = u64::from_ne_bytes(entry.try_into().unwrap());
                // Require present, resident, and WP clear. Preserve baseline semantics.
                let dirty = ((raw >> 63) & 1)
                    & (1 ^ ((raw >> 57) & 1))
                    & ((resident[page / 64] >> (page % 64)) & 1);
                out[page / 64] |= dirty << (page % 64);
            }
        }
        Ok((out, reads, bytes))
    }

    /// Check if a single page is dirty (write-protected bit cleared).
    ///
    /// Checks the first host page (4K) of the guest page at the given address.
    /// For huge pages, all host pages within the huge page typically have the same
    /// dirty status, so sampling the first is sufficient.
    ///
    /// # Arguments
    /// * `virt_addr` - Virtual address of the page to check
    ///
    /// # Returns
    /// True if the page is present and write-protected bit is cleared (dirty).
    pub fn is_page_dirty(&self, virt_addr: usize) -> Result<bool, PagemapError> {
        // Pagemap always uses host (4K) page size
        let host_page_size = host_page_size();

        // Calculate offset for this virtual page (using host page size)
        let host_vpn = virt_addr / host_page_size;
        let offset = (host_vpn * PAGEMAP_ENTRY_SIZE) as i64;

        let mut entry_bytes = [0u8; 8];

        // SAFETY: pread is safe as long as the fd is valid and the buffer is properly sized
        let ret = unsafe {
            libc::pread(
                self.pagemap_fd.as_raw_fd(),
                entry_bytes.as_mut_ptr().cast(),
                PAGEMAP_ENTRY_SIZE,
                offset,
            )
        };

        if ret != PAGEMAP_ENTRY_SIZE as isize {
            return Err(PagemapError::ReadEntry(std::io::Error::last_os_error()));
        }

        let entry = PagemapEntry::from_bytes(entry_bytes);

        // Page must be present and the write_protected bit cleared (indicating it was written to)
        Ok(entry.is_present() && !entry.is_write_protected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    #[test]
    fn batch_crosses_chunks_and_preserves_wp_present_semantics() {
        let f = vmm_sys_util::tempfile::TempFile::new().unwrap();
        let mut writer = f.as_file();
        let pages: usize = 4201;
        for i in 0..pages {
            let raw: u64 = match i % 3 {
                0 => 1u64 << 63,
                1 => (1u64 << 63) | (1u64 << 57),
                _ => 0,
            };
            writer.write_all(&raw.to_ne_bytes()).unwrap();
        }
        let reader = PagemapReader {
            pagemap_fd: f.as_file().try_clone().unwrap(),
        };
        let mut resident = vec![u64::MAX; pages.div_ceil(64)];
        resident[0] &= !1;
        let (bits, reads, bytes) = reader
            .scan_batched(0, pages, host_page_size(), &resident)
            .unwrap();
        assert_eq!(reads, 2);
        assert_eq!(bytes, pages as u64 * 8);
        for i in 0..pages {
            assert_eq!(
                (bits[i / 64] >> (i % 64)) & 1,
                u64::from(i != 0 && i % 3 == 0)
            );
        }
        assert_eq!(
            reader
                .scan_batched(0, pages, host_page_size(), &vec![0; resident.len()])
                .unwrap()
                .1,
            0
        );
    }
}
