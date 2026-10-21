//! Compatibility re-export.
//!
//! The command runner was promoted to [`crate::runner`] once virtualization
//! needed the same seam; these aliases keep every existing `networking::runner`
//! path working unchanged.

pub use crate::runner::{Runner, SystemRunner};

#[cfg(test)]
pub use crate::runner::fake;
