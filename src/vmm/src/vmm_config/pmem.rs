// SPDX-License-Identifier: Apache-2.0
//! Immutable virtio-pmem identity/layout and node-local binding.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Section-aligned windows support full struct-page filesystem DAX.
pub const PMEM_ALIGNMENT: u64 = 128 << 20;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Preboot or restore binding to an immutable local file.
pub struct PmemConfig {
    /// Pmem id.
    pub pmem_id: String,
    /// Path on host.
    pub path_on_host: PathBuf,
    /// Content id.
    pub content_id: String,
    /// Guest address.
    pub guest_address: u64,
    /// Size.
    pub size: u64,
    #[serde(default)]
    /// File offset.
    pub file_offset: u64,
    /// Read only.
    pub read_only: bool,
}

/// Only this identity/layout is persisted. Paths are supplied anew on restore.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
/// Persistent content identity and GPA layout, excluding host paths.
pub struct PmemLayout {
    /// Pmem id.
    pub pmem_id: String,
    /// Content id.
    pub content_id: String,
    /// Guest address.
    pub guest_address: u64,
    /// Size.
    pub size: u64,
    /// File offset.
    pub file_offset: u64,
}

impl PmemConfig {
    /// Immutable pmem layout.
    pub fn layout(&self) -> PmemLayout {
        PmemLayout {
            pmem_id: self.pmem_id.clone(),
            content_id: self.content_id.clone(),
            guest_address: self.guest_address,
            size: self.size,
            file_offset: self.file_offset,
        }
    }

    /// Immutable pmem validate.
    pub fn validate(&self) -> Result<(), String> {
        if !self.read_only
            || self.pmem_id.is_empty()
            || self.pmem_id.len() > 64
            || !self
                .pmem_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            || self.content_id.len() != 64
            || !self
                .content_id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || self.path_on_host.as_os_str().is_empty()
            || self.size == 0
            || self.size % PMEM_ALIGNMENT != 0
            || self.guest_address % PMEM_ALIGNMENT != 0
            || self.guest_address < (1 << 32)
            || self.file_offset % 4096 != 0
        {
            return Err("pmem requires an immutable SHA256 binding, readonly=true and a section-aligned device GPA above 4GiB".into());
        }
        if self
            .guest_address
            .checked_add(self.size)
            .filter(|end| *end <= (1 << 40))
            .is_none()
            || self
                .file_offset
                .checked_add(self.size)
                .filter(|end| *end <= i64::MAX as u64)
                .is_none()
        {
            return Err("pmem address/offset exceeds supported 40-bit layout or file range".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> PmemConfig {
        PmemConfig {
            pmem_id: "lower".into(),
            path_on_host: "/lower.raw".into(),
            content_id: "a".repeat(64),
            guest_address: 1 << 38,
            size: PMEM_ALIGNMENT,
            file_offset: 0,
            read_only: true,
        }
    }
    #[test]
    fn rejects_writable_overflow_unaligned_and_low_windows() {
        let c = config();
        assert!(c.validate().is_ok());
        let mut bad = c.clone();
        bad.read_only = false;
        assert!(bad.validate().is_err());
        let mut bad = c.clone();
        bad.guest_address = u64::MAX;
        assert!(bad.validate().is_err());
        let mut bad = c.clone();
        bad.guest_address = 0x40000000;
        assert!(bad.validate().is_err());
        let mut bad = c.clone();
        bad.size = 0;
        assert!(bad.validate().is_err());
        let mut bad = c.clone();
        bad.size += 4096;
        assert!(bad.validate().is_err());
        let mut bad = c.clone();
        bad.file_offset = u64::MAX;
        assert!(bad.validate().is_err());
        let mut bad = c.clone();
        bad.content_id = "/path".into();
        assert!(bad.validate().is_err());
    }
}
