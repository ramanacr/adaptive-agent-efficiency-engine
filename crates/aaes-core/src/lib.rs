//! AAES core: pure, synchronous building blocks with no I/O.
//!
//! - [`command`]: shell command → command family (shared with `aaes mine`).
//! - [`outline`]: deterministic structural outlines with exact line ranges (ADR-022).
//! - [`shape`]: large-file read shaping and search-result shaping (FUN-025, FUN-026).

pub mod command;
pub mod outline;
pub mod shape;
