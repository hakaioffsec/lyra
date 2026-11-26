// Copyright (c) 2025 Hakai Offensive Security.
// SPDX-License-Identifier: GPL-3.0-or-later

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn llvm_config(config: &Path, option: &str) -> String {
    let output = Command::new(config)
        .arg(option)
        .output()
        .unwrap_or_else(|error| panic!("cannot run {} {option}: {error}", config.display()));
    assert!(
        output.status.success(),
        "{} {option} failed: {}",
        config.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("llvm-config output is not UTF-8")
        .trim()
        .to_owned()
}

fn main() {
    // llvm-sys declares links="llvm-22". Its metadata pins headers/flags to
    // the library already selected for the Rust bindings, not another PATH LLVM.
    println!("cargo:rerun-if-env-changed=DEP_LLVM_22_CONFIG_PATH");
    let config = PathBuf::from(env::var_os("DEP_LLVM_22_CONFIG_PATH").expect(
        "llvm-sys must export DEP_LLVM_22_CONFIG_PATH; LLVM 22 development files are required",
    ));
    let version = llvm_config(&config, "--version");
    assert!(
        version.starts_with("22."),
        "outlining requires LLVM 22, got {version}"
    );
    let include = PathBuf::from(llvm_config(&config, "--includedir"));
    for header in [
        "llvm/Transforms/Utils/CodeExtractor.h",
        "llvm/Config/abi-breaking.h",
    ] {
        let path = include.join(header);
        assert!(
            path.is_file(),
            "LLVM C++ development header missing: {}",
            path.display()
        );
        println!("cargo:rerun-if-changed={}", path.display());
    }
    println!("cargo:rerun-if-changed={}", config.display());
    println!("cargo:rerun-if-changed=src/obfuscator/block_outline.cpp");

    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .include(&include)
        .file("src/obfuscator/block_outline.cpp");
    let msvc = build.get_compiler().is_like_msvc();
    if msvc {
        // The supported Windows LLVM bundle uses the static CRT (/MT).
        // cc otherwise follows Rust's default /MD and mixes two C++ runtimes
        // when this shim is linked with LLVM's static libraries.
        build.static_crt(true);
    }
    let flags = llvm_config(&config, "--cxxflags");
    // Windows llvm-config can emit quoted native paths. Forward slashes retain
    // their meaning to MSVC/clang-cl without POSIX shell unescaping backslashes.
    let flags = if cfg!(windows) {
        flags.replace('\\', "/")
    } else {
        flags
    };
    for flag in shlex::split(&flags).expect("invalid quoting in llvm-config --cxxflags") {
        if flag.starts_with("-std=") || flag.starts_with("/std:") {
            continue; // cc chooses the correct driver spelling for C++17.
        }
        match flag.as_str() {
            "-fno-exceptions" if msvc => {
                build.flag("/EHs-c-");
            }
            "-fno-rtti" if msvc => {
                build.flag("/GR-");
            }
            "-stdlib=libc++" => {
                build.cpp_set_stdlib("c++");
            }
            "-stdlib=libstdc++" => {
                build.cpp_set_stdlib("stdc++");
            }
            _ => {
                build.flag(&flag);
            }
        }
    }
    build.compile("lyra_block_outline");
}
