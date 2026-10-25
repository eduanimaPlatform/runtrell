// SPDX-License-Identifier: Apache-2.0
//! Tool results, failures, feedback and declared footprints. No executor.
pub mod feedback;
mod footprint;
mod output;
mod result;
pub use footprint::*;
pub use output::*;
pub use result::*;

pub mod validate;

mod schema;
pub use schema::args_schema;
