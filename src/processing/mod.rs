//! Post-processing functionality
//!
//! This module handles PAR2 verification/repair, RAR extraction, and file deobfuscation.

pub(crate) mod deobfuscate;
mod file_extension;
mod par2;
mod post_processor;
mod rar;
mod rar_ffi;

/// Tiny RAR5 archives for the unit tests (shared with the integration tests).
#[cfg(test)]
#[path = "../../tests/support/rar5.rs"]
mod test_rar5;

pub use par2::Par2Status;
pub use post_processor::{PostProcessingOutcome, PostProcessor};
