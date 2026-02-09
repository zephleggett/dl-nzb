//! Post-processing functionality
//!
//! This module handles PAR2 verification/repair, RAR extraction, and file deobfuscation.

mod deobfuscate;
mod file_extension;
mod par2;
mod post_processor;
mod rar;

pub use par2::Par2Status;
pub use post_processor::{PostProcessingOutcome, PostProcessor};
