//! Host instrumentation: everything that reads the machine itself.
//!
//! Deliberately dependency-light. The deployment plan calls for direct `/proc`
//! and `/sys` parsing instead of the heavyweight `sysinfo`-style crates, because
//! we only ever need a handful of fields and the binary should stay small.

pub mod metrics;
