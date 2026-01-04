//! Build script for dl-nzb
//!
//! Compiles par2cmdline-turbo from vendored source during cargo build.

fn main() {
    // Only build par2cmdline-turbo on Linux/macOS (requires autotools)
    // Windows users should use pre-built binaries or Visual Studio
    #[cfg(unix)]
    build_par2();

    #[cfg(windows)]
    build_par2_windows();
}

#[cfg(unix)]
fn build_par2() {
    use std::env;
    use std::path::PathBuf;
    use std::process::Command;

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let par2_src = manifest_dir.join("vendor").join("par2cmdline-turbo");

    // Check if par2 source exists
    if !par2_src.exists() {
        println!(
            "cargo:warning=par2cmdline-turbo source not found at {:?}",
            par2_src
        );
        println!("cargo:warning=PAR2 support will require manual installation of par2");
        return;
    }

    // Check if already built
    let par2_binary = out_dir.join("par2");
    if par2_binary.exists() {
        println!("cargo:warning=par2 already built, skipping");
        copy_par2_to_target(&par2_binary);
        return;
    }

    println!("cargo:warning=Building par2cmdline-turbo from source...");

    // Run automake.sh if configure doesn't exist
    let configure_script = par2_src.join("configure");
    if !configure_script.exists() {
        println!("cargo:warning=Running automake.sh...");
        let status = Command::new("sh")
            .arg("automake.sh")
            .current_dir(&par2_src)
            .status();

        if let Err(e) = status {
            println!("cargo:warning=Failed to run automake.sh: {}", e);
            println!("cargo:warning=Install autoconf, automake, libtool and try again");
            return;
        }
    }

    // Run configure
    println!("cargo:warning=Running configure...");
    let status = Command::new("sh")
        .arg("configure")
        .arg(format!("--prefix={}", out_dir.display()))
        .current_dir(&par2_src)
        .status();

    if let Err(e) = status {
        println!("cargo:warning=Failed to run configure: {}", e);
        return;
    }

    // Run make
    println!("cargo:warning=Running make...");
    let num_cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);

    let status = Command::new("make")
        .arg(format!("-j{}", num_cpus))
        .current_dir(&par2_src)
        .status();

    match status {
        Ok(s) if s.success() => {
            // Copy the built binary
            let built_par2 = par2_src.join("par2");
            if built_par2.exists() {
                if let Err(e) = std::fs::copy(&built_par2, &par2_binary) {
                    println!("cargo:warning=Failed to copy par2 binary: {}", e);
                } else {
                    println!("cargo:warning=par2cmdline-turbo built successfully!");
                    copy_par2_to_target(&par2_binary);
                }
            }
        }
        Ok(_) => {
            println!("cargo:warning=make failed");
        }
        Err(e) => {
            println!("cargo:warning=Failed to run make: {}", e);
        }
    }
}

#[cfg(unix)]
fn copy_par2_to_target(par2_binary: &std::path::PathBuf) {
    use std::env;
    use std::path::PathBuf;

    // Copy to target directory so it's alongside the final binary
    if let Ok(profile) = env::var("PROFILE") {
        let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
        let target_dir = manifest_dir.join("target").join(&profile);

        if target_dir.exists() {
            let dest = target_dir.join("par2");
            if let Err(e) = std::fs::copy(par2_binary, &dest) {
                println!("cargo:warning=Failed to copy par2 to target: {}", e);
            } else {
                println!("cargo:warning=Copied par2 to {:?}", dest);
            }
        }
    }
}

#[cfg(windows)]
fn build_par2_windows() {
    // On Windows, we don't auto-build (requires Visual Studio or MSYS2)
    // Users should either:
    // 1. Download pre-built binary from releases
    // 2. Build manually with Visual Studio
    // 3. Use MSYS2 to build
    println!("cargo:warning=par2cmdline-turbo must be built separately on Windows");
    println!("cargo:warning=See vendor/BUILD.md for instructions");
}
