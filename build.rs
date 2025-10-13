use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let par2_root = PathBuf::from("par2cmdline-turbo");
    let build_dir = PathBuf::from(".build");

    // Create build directory if it doesn't exist
    std::fs::create_dir_all(&build_dir).expect("Failed to create build directory");

    // Check if we already have a built library
    let lib_path = build_dir.join("libpar2_combined.a");

    // Only tell cargo to rerun if PAR2-related files change, not on every Rust code change
    // This dramatically speeds up incremental debug builds
    println!("cargo:rerun-if-changed=src/processing/par2repairer.cpp");
    println!("cargo:rerun-if-changed=par2cmdline-turbo/configure.ac");
    println!("cargo:rerun-if-changed=.build/libpar2_combined.a");

    if !lib_path.exists() {
        eprintln!("Building par2cmdline-turbo using autotools...");

        // Run automake.sh if configure doesn't exist
        let configure_path = par2_root.join("configure");
        if !configure_path.exists() {
            let status = Command::new("sh")
                .arg("automake.sh")
                .current_dir(&par2_root)
                .status()
                .expect("Failed to run automake.sh - make sure autotools are installed");

            if !status.success() {
                panic!("automake.sh failed");
            }
        }

        // Run configure
        let status = Command::new("sh")
            .arg("configure")
            .current_dir(&par2_root)
            .status()
            .expect("Failed to run configure - make sure autotools are installed");

        if !status.success() {
            panic!("configure failed");
        }

        // Build libpar2.a
        let num_jobs = env::var("NUM_JOBS").unwrap_or_else(|_| "4".to_string());
        let status = Command::new("make")
            .arg("-j")
            .arg(&num_jobs)
            .arg("libpar2.a")
            .current_dir(&par2_root)
            .status()
            .expect("Failed to run make - make sure build tools are installed");

        if !status.success() {
            panic!("make libpar2.a failed");
        }

        // Compile the C API wrapper (our custom par2repairer.cpp)
        let wrapper_path = PathBuf::from("src/processing/par2repairer.cpp")
            .canonicalize()
            .expect("Failed to find src/processing/par2repairer.cpp");

        let wrapper_obj = build_dir.join("par2repairer_wrapper.o");
        let status = Command::new("g++")
            .args([
                "-std=c++14",
                "-DHAVE_CONFIG_H",
                "-Wall",
                "-DNDEBUG",
                "-DPARPAR_ENABLE_HASHER_MD5CRC",
                "-DPARPAR_INVERT_SUPPORT",
                "-DPARPAR_SLIM_GF16",
                "-g",
                "-O2",
                "-c",
                "-o",
            ])
            .arg(&wrapper_obj)
            .arg("-I")
            .arg(&par2_root)
            .arg(&wrapper_path)
            .status()
            .expect("Failed to compile C API wrapper");

        if !status.success() {
            panic!("Failed to compile par2repairer.cpp wrapper");
        }

        // Combine libraries in build directory
        let temp_dir = build_dir.join("par2_objs");
        std::fs::create_dir_all(&temp_dir).unwrap();

        // Extract libpar2.a
        let libpar2_path = par2_root
            .join("libpar2.a")
            .canonicalize()
            .expect("Failed to get absolute path to libpar2.a");
        Command::new("ar")
            .arg("x")
            .arg(&libpar2_path)
            .current_dir(&temp_dir)
            .status()
            .expect("Failed to extract libpar2.a");

        // Copy par2repairer_wrapper.o
        std::fs::copy(&wrapper_obj, temp_dir.join("par2repairer_wrapper.o")).unwrap();

        // Create combined library in build directory
        let combined_lib_path = build_dir
            .join("libpar2_combined.a")
            .canonicalize()
            .unwrap_or_else(|_| {
                std::env::current_dir()
                    .unwrap()
                    .join(&build_dir)
                    .join("libpar2_combined.a")
            });

        let status = Command::new("sh")
            .arg("-c")
            .arg(format!("ar rcs {} *.o", combined_lib_path.display()))
            .current_dir(&temp_dir)
            .status()
            .expect("Failed to create combined library");

        if !status.success() {
            panic!("Failed to create libpar2_combined.a");
        }

        // Clean up temp dir
        std::fs::remove_dir_all(&temp_dir).ok();

        eprintln!("par2cmdline-turbo built successfully!");
    }

    // Tell cargo to link the combined library from build directory
    let combined_lib = build_dir
        .join("libpar2_combined.a")
        .canonicalize()
        .expect("Failed to find libpar2_combined.a in build directory");
    println!(
        "cargo:rustc-link-search=native={}",
        build_dir.canonicalize().unwrap().display()
    );
    println!("cargo:rustc-link-lib=static=par2_combined");

    // Also add the full path as a direct link argument
    println!("cargo:rustc-link-arg={}", combined_lib.display());

    // Link C++ standard library and pthread
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();

    match target_os.as_str() {
        "macos" => {
            println!("cargo:rustc-link-lib=dylib=c++");
        }
        "linux" => {
            println!("cargo:rustc-link-lib=dylib=stdc++");
        }
        "windows" => {
            // MSVC links C++ automatically
        }
        _ => {}
    }

    if target_os != "windows" {
        println!("cargo:rustc-link-lib=dylib=pthread");
    }
}
