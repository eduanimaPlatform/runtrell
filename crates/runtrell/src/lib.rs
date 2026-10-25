// SPDX-License-Identifier: Apache-2.0
#![forbid(unsafe_code)]
#![allow(async_fn_in_trait)]
//! Host-independent execution contracts. In-memory adapters are not durable storage.
mod api;
pub use api::*;
mod driver;
pub mod memory;
pub use driver::Runtime;
pub mod schema;
pub use schema::{schema_diagnostics, validate_schema};
pub mod data;
pub use data::{
    ArtifactRef, ArtifactStore, FileVersion, Namespace, WorkspaceSnapshot, WorkspaceStore,
};
pub mod execution;
pub mod file_tools;
pub use execution::{ExecutionBackend, ExecutionLimits, ExecutionRef};

pub mod budget;
pub use budget::{AccountPolicy, ReservationPolicy, SchedulingPolicy};

pub mod assembly;
