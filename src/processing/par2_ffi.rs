use std::ffi::CString;
use std::path::Path;
use crate::error::{DlNzbError, PostProcessingError};

type Result<T> = std::result::Result<T, DlNzbError>;

// Manual FFI declarations following Rust Nomicon approach
#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Par2Result {
    Success = 0,
    RepairPossible = 1,
    RepairNotPossible = 2,
    InvalidArguments = 3,
    InsufficientData = 4,
    RepairFailed = 5,
    FileIOError = 6,
    LogicError = 7,
    MemoryError = 8,
}

// External C function declaration
extern "C" {
    fn par2_repair_sync(
        parfilename: *const std::os::raw::c_char,
        do_repair: bool,
    ) -> Par2Result;
}

/// Rust wrapper for PAR2 repair functionality
pub struct Par2Repairer {
    par2_file: String,
}

impl Par2Repairer {
    /// Create a new PAR2 repairer for the given PAR2 file
    pub fn new(par2_file: &Path) -> Result<Self> {
        Ok(Self {
            par2_file: par2_file.to_string_lossy().to_string(),
        })
    }

    /// Perform PAR2 repair or verification (synchronous, single-threaded)
    ///
    /// # Arguments
    /// * `do_repair` - If true, perform repair; if false, only verify
    ///
    /// # Returns
    /// * `Ok(())` - Files were correct or successfully repaired
    /// * `Err(DlNzbError)` - Repair failed or not possible
    pub fn repair(&self, do_repair: bool) -> Result<()> {
        // Convert path to C string
        let par2_cstr = CString::new(self.par2_file.as_str())
            .map_err(|e| PostProcessingError::Par2Failed(
                format!("Invalid PAR2 file path: {}", e)
            ))?;

        // Call C API (all work happens synchronously on this thread)
        let result = unsafe {
            par2_repair_sync(
                par2_cstr.as_ptr(),
                do_repair,
            )
        };

        // Convert result
        match result {
            Par2Result::Success => Ok(()),
            Par2Result::RepairPossible => {
                if do_repair {
                    Err(PostProcessingError::Par2Failed(
                        "PAR2 repair possible but not completed".to_string()
                    ).into())
                } else {
                    Ok(()) // Verification passed, repair is possible if needed
                }
            }
            Par2Result::RepairNotPossible => Err(PostProcessingError::Par2Failed(
                "PAR2 repair not possible: insufficient recovery data".to_string()
            ).into()),
            Par2Result::InvalidArguments => Err(PostProcessingError::Par2Failed(
                "Invalid arguments".to_string()
            ).into()),
            Par2Result::InsufficientData => Err(PostProcessingError::Par2Failed(
                "Insufficient critical data in PAR2 files".to_string()
            ).into()),
            Par2Result::RepairFailed => Err(PostProcessingError::Par2Failed(
                "PAR2 repair failed".to_string()
            ).into()),
            Par2Result::FileIOError => Err(PostProcessingError::Par2Failed(
                "File I/O error".to_string()
            ).into()),
            Par2Result::LogicError => Err(PostProcessingError::Par2Failed(
                "Internal logic error".to_string()
            ).into()),
            Par2Result::MemoryError => Err(PostProcessingError::Par2Failed(
                "Out of memory".to_string()
            ).into()),
        }
    }
}
