// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0
use std::fmt::{self, Display, Formatter};

use crate::vstate::vm::GuestMemoryRegionMapping;
use serde::{Serialize, ser};

/// Enumerates microVM runtime states.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum VmState {
    /// Vm not started (yet)
    #[default]
    NotStarted,
    /// Vm is Paused
    Paused,
    /// Vm is running
    Running,
}

impl Display for VmState {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        match *self {
            VmState::NotStarted => write!(f, "Not started"),
            VmState::Paused => write!(f, "Paused"),
            VmState::Running => write!(f, "Running"),
        }
    }
}

impl ser::Serialize for VmState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: ser::Serializer,
    {
        self.to_string().serialize(serializer)
    }
}

/// Serializable struct that contains general information about the microVM.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct InstanceInfo {
    /// The ID of the microVM.
    pub id: String,
    /// Whether the microVM is not started/running/paused.
    pub state: VmState,
    /// The version of the VMM that runs the microVM.
    pub vmm_version: String,
    /// The name of the application that runs the microVM.
    pub app_name: String,
    /// The regions of the guest memory.
    pub memory_regions: Option<Vec<GuestMemoryRegionMapping>>,
}

/// Response structure for the memory mappings endpoint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MemoryMappingsResponse {
    /// The memory region mappings.
    pub mappings: Vec<GuestMemoryRegionMapping>,
}

/// Response structure for the memory endpoint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MemoryResponse {
    /// The resident bitmap as a vector of u64 values. Each bit represents if the page is resident.
    pub resident: Vec<u64>,
    /// The empty bitmap as a vector of u64 values. Each bit represents if the page is zero (empty).
    /// This is a subset of the resident pages.
    pub empty: Vec<u64>,
}

/// Information about dirty guest memory pages
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct MemoryDirty {
    /// Bitmap for dirty pages. The bitmap is encoded as a vector of u64 values.
    /// With tracking_version 1 this accumulates changes since this VM restore.
    pub bitmap: Vec<u64>,
    /// Version 1 is cumulative KVM/VMM tracking since this restore.
    pub tracking_version: u32,
    /// Selection policy, independent of residency.
    pub tracking_mode: String,
    /// True only with logging and software bitmaps on every RAM region.
    pub tracking_complete: bool,
    /// A successful state-only snapshot has frozen CPU and device writers.
    pub capture_ready: bool,
    /// Bytes in the logical RAM stream, excluding GPA holes.
    pub logical_size: u64,
    /// Granularity of bitmap bits, in bytes.
    pub page_size: u64,
}
