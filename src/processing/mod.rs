//! Post-processing functionality
//!
//! This module handles PAR2 verification/repair, RAR extraction, and file deobfuscation.

mod deobfuscate;
mod file_extension;
mod post_process;

pub use post_process::PostProcessor;
