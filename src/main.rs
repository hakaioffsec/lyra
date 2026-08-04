// Copyright (c) 2025 Hakai Offensive Security.
//
// This program is free software: you can redistribute it and/or modify it
// under the terms of the GNU General Public License as published by the
// Free Software Foundation, version 3. See the LICENSE file for details.

mod obfuscator;

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use inkwell::context::Context as LLVMContext;
use inkwell::memory_buffer::MemoryBuffer;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The kind of artefact lyra should build and obfuscate.
///
/// `bin` and `cdylib`/`dylib` go through the **linker** (rustc emits
/// `.o` objects and then invokes `link.exe`/`ld`/`ld64` to produce an
/// `.exe` / `.dll` / `.so` / `.dylib`). Both are routed through the
/// two-phase "Phase A emit IR → obfuscate → Phase B link with shim"
/// path in `lyra_wrapper.rs`.
///
/// `staticlib` produces a `.lib` / `.a` archive directly (no link
/// step). Its main `.o` lives inside that archive and is patched the
/// same way rlibs are.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum CrateKind {
    /// Executable binary (`.exe`). Default.
    Bin,
    /// C-ABI dynamic library (`.dll` / `.so` / `.dylib`). Exports
    /// `#[no_mangle] pub extern "C"` functions for FFI consumers.
    Cdylib,
    /// Rust-native dynamic library (same file extension as cdylib
    /// but exports Rust ABI; rarely used outside rustc itself).
    Dylib,
    /// Static library archive (`.lib` / `.a`). Obfuscation patches
    /// the main `.o` inside the archive, same as an rlib.
    Staticlib,
}

impl CrateKind {
    fn cargo_build_arg(self, name: &str) -> Vec<String> {
        match self {
            CrateKind::Bin => vec!["--bin".into(), name.into()],
            // cargo `--lib` builds whichever crate-types are listed in
            // `[lib] crate-type = [...]`. The user is responsible for
            // having set it to include cdylib / dylib / staticlib.
            CrateKind::Cdylib | CrateKind::Dylib | CrateKind::Staticlib => {
                vec!["--lib".into()]
            }
        }
    }

    fn label(self) -> &'static str {
        match self {
            CrateKind::Bin => "bin",
            CrateKind::Cdylib => "cdylib",
            CrateKind::Dylib => "dylib",
            CrateKind::Staticlib => "staticlib",
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "lyra",
    about = "LLVM-based code obfuscator for Rust",
    long_about = "Lyra obfuscates Rust binaries at the LLVM IR level, making them resistant to reverse engineering."
)]
struct Args {
    /// Path to the Rust project directory to obfuscate
    #[arg(short, long, value_name = "DIR")]
    project: PathBuf,

    /// Output path for the obfuscated artefact (`.exe`, `.dll`, `.so`,
    /// `.dylib`, or `.lib`/`.a` depending on `--crate-type`).
    #[arg(short, long, value_name = "FILE")]
    output: PathBuf,

    /// Target triple. Defaults to the host triple if not specified.
    ///
    /// Supported targets:
    ///   Windows:  x86_64-pc-windows-msvc, x86_64-pc-windows-gnu,
    ///             x86_64-pc-windows-gnullvm
    ///   macOS:    aarch64-apple-darwin, x86_64-apple-darwin
    ///   Linux:    x86_64-unknown-linux-gnu, aarch64-unknown-linux-gnu
    #[arg(long)]
    target: Option<String>,

    /// What kind of artefact to build. Default `bin`. Use `cdylib` to
    /// obfuscate a Rust DLL with C-ABI exports.
    #[arg(long = "crate-type", value_enum, default_value_t = CrateKind::Bin)]
    crate_type: CrateKind,

    /// Shorthand for `--crate-type cdylib`. Conflicts with `--bin`
    /// and `--crate-type`.
    #[arg(long, conflicts_with_all = ["bin", "crate_type"])]
    lib: bool,

    /// Enable string encryption (per-byte random XOR + global-ctor
    /// decrypt stub). Currently the only shipped obfuscation pass.
    #[arg(long = "string-enc")]
    string_enc: bool,

    /// Enable basic-block shuffling. Permutes textual block order within
    /// each function (entry block stays first). Breaks byte-level
    /// signatures and block-layout-based BinDiff matching; no effect on
    /// CFG semantics.
    #[arg(long = "shuffle-blocks")]
    shuffle_blocks: bool,

    /// Enable indirect-branch obfuscation. Rewrites direct `br` terminators
    /// to go through a per-function block-address dispatch table, so the
    /// real destination is only known at runtime. Breaks CFG reconstruction
    /// in decompilers.
    #[arg(long = "indirect-branch")]
    indirect_branch: bool,

    /// Enable Mixed Boolean-Arithmetic substitution. Replaces integer
    /// add/sub/xor/and/or and float fadd/fsub/fmul with equivalent MBA
    /// expressions (2 rounds). Combined with --indirect-branch, makes
    /// dispatch-table index computation opaque to decompilers.
    #[arg(long = "mba")]
    mba: bool,

    /// Keep intermediate files for debugging
    #[arg(long)]
    keep_temps: bool,

    /// Name of the binary target to obfuscate (required when the project
    /// has multiple bin targets). If unset, the project's default bin is
    /// used. Only meaningful when `--crate-type bin` (default).
    #[arg(long, value_name = "NAME")]
    bin: Option<String>,

    /// Obfuscate the named crate in addition to the bin. May be passed
    /// multiple times. Lib-style crates have their rlib patched in place;
    /// the bin/cdylib target is handled via a linker shim.
    /// Example: --obfuscate-crate quasar --obfuscate-crate protean
    #[arg(long = "obfuscate-crate", value_name = "CRATE")]
    obfuscate_crate: Vec<String>,

    /// Enable verbose diagnostics from the lyra_linker shim (logs to
    /// `<temp>/obf/lyra_linker.log` regardless of this flag; enabling it
    /// additionally mirrors those lines to stderr).
    #[arg(long)]
    verbose_link: bool,

    /// Master RNG seed for all randomised passes. If set, every pass
    /// derives its per-module RNG deterministically from this seed, so
    /// the same seed + same source produces the same binary. Without
    /// this flag, every build uses fresh OS entropy and two consecutive
    /// builds of the same source produce different binaries (the
    /// signature-breaking default).
    #[arg(long, value_name = "U64")]
    seed: Option<u64>,
}

impl Args {
    /// Resolve the effective `CrateKind` after considering the `--lib`
    /// shorthand.
    fn effective_kind(&self) -> CrateKind {
        if self.lib {
            CrateKind::Cdylib
        } else {
            self.crate_type
        }
    }
}

fn main() -> Result<()> {
    // Internal mode dispatch: the lyra_wrapper binary calls us as
    //     lyra __obfuscate-ir <in.ll> <out.ll> [--string-enc]
    // This bypasses clap so we can keep the public CLI clean.
    {
        let raw: Vec<String> = std::env::args().collect();
        if raw.len() >= 4 && raw[1] == "__obfuscate-ir" {
            return run_internal_obfuscate(&raw[2..]);
        }
    }

    let mut args = Args::parse();

    if args.target.is_none() {
        args.target = Some(detect_host_triple()?);
    }

    if !args.project.exists() {
        anyhow::bail!(
            "Project directory does not exist: {}",
            args.project.display()
        );
    }
    if !args.project.join("Cargo.toml").exists() {
        anyhow::bail!(
            "Not a valid Rust project (missing Cargo.toml): {}",
            args.project.display()
        );
    }

    println!("\n╔═══════════════════════════════════════╗");
    println!("║      Lyra LLVM Obfuscator v0.1.0     ║");
    println!("╚═══════════════════════════════════════╝\n");

    println!("[*] Project: {}", args.project.display());
    let target = args.target.as_deref().unwrap();
    println!("[*] Target : {}", target);
    println!("[*] Output : {}", args.output.display());
    match args.seed {
        Some(s) => println!("[*] Seed   : {} (deterministic)", s),
        None => println!("[*] Seed   : (OS entropy, non-deterministic)"),
    }

    let mut passes: Vec<&str> = Vec::new();
    if args.string_enc {
        passes.push("String Encryption");
    }
    if args.shuffle_blocks {
        passes.push("Shuffle Blocks");
    }
    if args.indirect_branch {
        passes.push("Indirect Branch");
    }
    if args.mba {
        passes.push("MBA");
    }
    if passes.is_empty() {
        println!("[*] Passes : none (use --string-enc, --shuffle-blocks, --indirect-branch, --mba)");
    } else {
        println!("[*] Passes : {}", passes.join(", "));
    }
    println!();

    let temp_dir = std::env::temp_dir().join(format!("lyra_{}", std::process::id()));
    fs::create_dir_all(&temp_dir).context("create temp directory")?;

    run_native_flow(&args, &temp_dir)?;

    if !args.keep_temps {
        let _ = fs::remove_dir_all(&temp_dir);
    } else {
        println!("\n[*] Intermediate files kept in: {}", temp_dir.display());
    }

    println!("\n╔═══════════════════════════════════════╗");
    println!("║         BUILD SUCCESSFUL! ✓           ║");
    println!("╚═══════════════════════════════════════╝");
    println!("\nOutput: {}\n", args.output.display());

    Ok(())
}

// ===========================================================================
// Main pipeline
// ===========================================================================
//
// 1. Launch `cargo build --bin <bin> --release --target <target>` with:
//      - RUSTC_WRAPPER        = path to lyra_wrapper.exe
//      - LYRA_OBFUSCATE_CRATES = comma-separated crate names
//      - LYRA_OBF_DIR          = temp dir for obfuscated artefacts
//      - LYRA_LYRA_EXE         = this executable (for `lyra __obfuscate-ir`)
//      - LYRA_LLC / LYRA_CLANG / LYRA_LLVM_AR = absolute paths to tools
//      - LYRA_LINKER_SHIM / LYRA_REAL_LINKER  = linker-shim wiring
//      - LYRA_TARGET / LYRA_OBFUSCATE_FLAGS   = pass-through config
//      - CARGO_PROFILE_RELEASE_LTO=off        = force-disable LTO so rlibs
//        contain plain machine-code .o files we can swap.
//
// 2. For each rustc invocation cargo makes, lyra_wrapper.exe inspects
//    --crate-name. If it's in LYRA_OBFUSCATE_CRATES:
//      - LIB crate: re-runs rustc with `--emit=llvm-ir` appended,
//        obfuscates the resulting IR (via `lyra __obfuscate-ir`), compiles
//        it to a .o, then patches the freshly-produced rlib in place using
//        llvm-ar (swapping the main codegen-unit .o with the obfuscated
//        one).
//      - BIN crate: Phase A emits IR without linking; Phase B re-runs
//        rustc with `-C linker=<lyra_linker.exe>`. The linker shim
//        intercepts the MSVC linker invocation (including @response
//        files), substitutes the bin's .rcgu.o with the obfuscated .o
//        while preserving the allocator shim, then invokes link.exe.
//
// 3. Copy the cargo-produced executable to --output.
// ===========================================================================

fn run_native_flow(args: &Args, temp_dir: &Path) -> Result<()> {
    let kind = args.effective_kind();
    let target = args.target.as_deref().unwrap();

    // Resolve the target name (bin name or lib name) from CLI + cargo metadata.
    let target_name = match kind {
        CrateKind::Bin => args
            .bin
            .clone()
            .or_else(|| detect_default_target(&args.project, kind).ok())
            .context("Could not determine bin crate name; pass --bin <NAME>")?,
        CrateKind::Cdylib | CrateKind::Dylib | CrateKind::Staticlib => {
            if args.bin.is_some() {
                anyhow::bail!(
                    "--bin is only valid with --crate-type bin; got --crate-type {}",
                    kind.label()
                );
            }
            detect_default_target(&args.project, kind)
                .context("Could not determine lib crate name from Cargo.toml")?
        }
    };

    // Which crates to obfuscate. If the user didn't pass --obfuscate-crate,
    // default to obfuscating just the main target crate.
    let mut crates_to_obfuscate: Vec<String> = args.obfuscate_crate.clone();
    if crates_to_obfuscate.is_empty() {
        crates_to_obfuscate.push(target_name.clone());
    } else if !crates_to_obfuscate.contains(&target_name) {
        eprintln!(
            "[lyra] note: --obfuscate-crate list does not include target '{}'. \
             Lib crates will be obfuscated inside their rlibs, but the main \
             target's own source strings won't be. Add --obfuscate-crate {} \
             to include them.",
            target_name, target_name
        );
    }

    println!(
        "[ 1/3 ] Building {} ({}) with RUSTC_WRAPPER + obfuscation of {:?} ...",
        target_name,
        kind.label(),
        crates_to_obfuscate
    );

    let real_linker = find_real_linker(target)?;
    let shim_path = find_self_bin("lyra_linker")?;
    let wrapper_path = find_self_bin("lyra_wrapper")?;
    let lyra_exe = std::env::current_exe().context("current_exe")?;

    let llvm_bin = find_llvm_project_bin().context(
        "Could not locate llvm-project/build/bin. Expected alongside or two dirs above the lyra binary.",
    )?;
    let llc = llvm_bin.join(exe_name("llc"));
    let clang = llvm_bin.join(exe_name("clang"));
    let llvm_ar = llvm_bin.join(exe_name("llvm-ar"));

    let llc_abs = absolutize(&llc)?;
    let clang_abs = absolutize(&clang)?;
    let llvm_ar_abs = absolutize(&llvm_ar)?;

    let obf_dir = temp_dir.join("obf");
    fs::create_dir_all(&obf_dir).ok();

    println!("        real linker : {}", real_linker.display());
    println!("        shim        : {}", shim_path.display());
    println!("        wrapper     : {}", wrapper_path.display());
    println!("        obf dir     : {}", obf_dir.display());

    let mut obf_flags: Vec<&str> = Vec::new();
    if args.string_enc {
        obf_flags.push("--string-enc");
    }
    if args.shuffle_blocks {
        obf_flags.push("--shuffle-blocks");
    }
    if args.indirect_branch {
        obf_flags.push("--indirect-branch");
    }
    if args.mba {
        obf_flags.push("--mba");
    }

    // `cargo clean` ensures the wrapper is invoked for every crate we care
    // about. Without a clean, cargo would skip anything it considers fresh.
    println!(
        "[ 2/3 ] cargo clean + cargo build {} ...",
        kind.cargo_build_arg(&target_name).join(" ")
    );
    let status = Command::new("cargo")
        .current_dir(&args.project)
        .arg("clean")
        .status()
        .context("cargo clean")?;
    if !status.success() {
        anyhow::bail!("cargo clean failed");
    }

    let mut cmd = Command::new("cargo");
    cmd.current_dir(&args.project);
    cmd.env("RUSTC_WRAPPER", &wrapper_path);
    cmd.env("LYRA_OBFUSCATE_CRATES", crates_to_obfuscate.join(","));
    cmd.env("LYRA_OBF_DIR", &obf_dir);
    cmd.env("LYRA_LYRA_EXE", &lyra_exe);
    cmd.env("LYRA_LLC", &llc_abs);
    cmd.env("LYRA_CLANG", &clang_abs);
    cmd.env("LYRA_LLVM_AR", &llvm_ar_abs);
    cmd.env("LYRA_TARGET", target);
    cmd.env("LYRA_OBFUSCATE_FLAGS", obf_flags.join(" "));
    cmd.env("LYRA_LINKER_SHIM", &shim_path);
    cmd.env("LYRA_REAL_LINKER", &real_linker);
    // Force LTO off across the whole build. Our obfuscated .o files are
    // produced by llc + clang without embedded bitcode and so cannot
    // participate in thin/fat LTO. Keeping rlibs + bin objects as plain
    // machine code lets link.exe consume them uniformly and overrides any
    // `[profile.release]` lto setting in the user's project.
    cmd.env("CARGO_PROFILE_RELEASE_LTO", "off");
    cmd.env("CARGO_PROFILE_RELEASE_CODEGEN_UNITS", "1");
    if args.verbose_link {
        cmd.env("LYRA_DEBUG", "1");
    }
    if let Some(s) = args.seed {
        // Propagated through cargo -> rustc wrapper -> lyra __obfuscate-ir
        // subprocess. Every pass honours this via `obfuscator::seed`.
        cmd.env("LYRA_SEED", s.to_string());
    }
    cmd.args(["build", "--release", "--target", target]);
    for a in kind.cargo_build_arg(&target_name) {
        cmd.arg(a);
    }

    let status = cmd.status().context("cargo build via RUSTC_WRAPPER")?;
    if !status.success() {
        anyhow::bail!("cargo build via RUSTC_WRAPPER failed");
    }

    println!("[ 3/3 ] Copying final artefact to output...");
    let cargo_out = cargo_target_output_path(&args.project, target, &target_name, kind);
    if !cargo_out.exists() {
        anyhow::bail!(
            "expected cargo output at {} but file does not exist",
            cargo_out.display()
        );
    }
    if let Some(parent) = args.output.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).ok();
        }
    }
    fs::copy(&cargo_out, &args.output).with_context(|| {
        format!(
            "copying {} -> {}",
            cargo_out.display(),
            args.output.display()
        )
    })?;
    let sz = fs::metadata(&args.output)?.len();
    println!(
        "        ✓ Artefact: {} bytes ({:.2} MB)",
        sz,
        sz as f64 / 1024.0 / 1024.0
    );

    // Windows targets produce an import library alongside the DLL that
    // C/C++ consumers need to link against. Copy it next to `--output`.
    //
    // MSVC toolchain:  `<name>.dll.lib`
    // GNU toolchain:   `lib<name>.dll.a`
    if (kind == CrateKind::Cdylib || kind == CrateKind::Dylib)
        && target.contains("windows")
    {
        let is_gnu = target.contains("gnu");
        let candidates: &[String] = &[
            // MSVC
            format!("{}.dll.lib", target_name),
            // GNU (MinGW)
            format!("lib{}.dll.a", target_name),
        ];
        let release_dir = cargo_out.parent();
        let import_src = release_dir.and_then(|dir| {
            candidates.iter().find_map(|name| {
                let p = dir.join(name);
                if p.exists() { Some(p) } else { None }
            })
        });
        if let Some(src) = import_src {
            let ext = if is_gnu { "dll.a" } else { "dll.lib" };
            let dst = args.output.with_extension(ext);
            if let Err(e) = fs::copy(&src, &dst) {
                eprintln!(
                    "[lyra] warning: could not copy import lib {} -> {}: {}",
                    src.display(),
                    dst.display(),
                    e
                );
            } else {
                println!("        ✓ Import lib: {}", dst.display());
            }
        }
    }

    Ok(())
}

// ===========================================================================
// Path / tool resolution helpers
// ===========================================================================

fn detect_host_triple() -> Result<String> {
    let out = Command::new("rustc")
        .args(["-vV"])
        .output()
        .context("failed to run `rustc -vV` to detect host triple")?;
    if !out.status.success() {
        anyhow::bail!("`rustc -vV` exited with {}", out.status);
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    for line in stdout.lines() {
        if let Some(triple) = line.strip_prefix("host: ") {
            return Ok(triple.trim().to_string());
        }
    }
    anyhow::bail!("`rustc -vV` output did not contain a `host:` line")
}

fn exe_name(stem: &str) -> String {
    if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_string()
    }
}

fn absolutize(p: &Path) -> Result<PathBuf> {
    if p.is_absolute() {
        Ok(p.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(p))
    }
}

/// Locate the LLVM bin directory containing `llc`, `clang`, and `llvm-ar`.
///
/// Resolution order (first match wins):
///   1. `$LYRA_LLVM_BIN` env var (explicit override)
///   2. `$LLVM_SYS_211_PREFIX/bin` (set for llvm-sys / Homebrew builds)
///   3. In-tree build layouts relative to the lyra binary
///   4. System-installed LLVM on PATH
fn find_llvm_project_bin() -> Result<PathBuf> {
    if let Ok(v) = std::env::var("LYRA_LLVM_BIN") {
        let p = PathBuf::from(v);
        if p.exists() {
            return Ok(p);
        }
    }

    for var in ["LLVM_SYS_221_PREFIX", "LLVM_SYS_211_PREFIX"] {
        if let Ok(v) = std::env::var(var) {
            let p = PathBuf::from(v).join("bin");
            if has_llc(&p) {
                return Ok(p.canonicalize().unwrap_or(p));
            }
        }
    }

    let exe = std::env::current_exe().context("current_exe")?;
    let exe_dir = exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("exe has no parent"))?;

    let mut candidates = vec![
        // In-tree build: <repo>/target/release/lyra -> <repo>/llvm-project-22/build/bin
        exe_dir
            .join("..")
            .join("..")
            .join("llvm-project-22")
            .join("build")
            .join("bin"),
        exe_dir
            .join("..")
            .join("..")
            .join("llvm-project")
            .join("build")
            .join("bin"),
        // Side-by-side install layouts.
        exe_dir
            .join("llvm-project-22")
            .join("build")
            .join("bin"),
        exe_dir.join("llvm-project").join("build").join("bin"),
        // CWD fallbacks.
        PathBuf::from("llvm-project-22/build/bin"),
        PathBuf::from("llvm-project/build/bin"),
    ];

    // Homebrew (macOS): /opt/homebrew/opt/llvm@22/bin or /usr/local/opt/llvm@22/bin
    for prefix in ["/opt/homebrew/opt", "/usr/local/opt"] {
        candidates.push(PathBuf::from(prefix).join("llvm@22").join("bin"));
        candidates.push(PathBuf::from(prefix).join("llvm").join("bin"));
    }

    for c in &candidates {
        if has_llc(c) {
            return Ok(c.canonicalize().unwrap_or(c.clone()));
        }
    }

    anyhow::bail!(
        "could not find LLVM bin directory with `llc`. Set $LYRA_LLVM_BIN or \
         $LLVM_SYS_211_PREFIX, or install LLVM 22 (tried: {})",
        candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
}

fn has_llc(dir: &Path) -> bool {
    dir.join("llc").exists() || dir.join("llc.exe").exists()
}

/// Compute the path cargo will write the final artefact to given the
/// crate kind and target triple.
///
/// The naming conventions implemented here:
///
/// | kind      | Windows (MSVC/gnu)    | Linux              | macOS                 |
/// |-----------|-----------------------|--------------------|-----------------------|
/// | bin       | `<name>.exe`          | `<name>`           | `<name>`              |
/// | cdylib    | `<name>.dll`          | `lib<name>.so`     | `lib<name>.dylib`     |
/// | dylib     | `<name>.dll`          | `lib<name>.so`     | `lib<name>.dylib`     |
/// | staticlib | `<name>.lib`          | `lib<name>.a`      | `lib<name>.a`         |
///
/// The `lib` prefix on Unix is added by cargo/rustc for linkable
/// artefacts (matches standard Unix library naming).
fn cargo_target_output_path(
    project_dir: &Path,
    target: &str,
    name: &str,
    kind: CrateKind,
) -> PathBuf {
    let release_dir = project_dir.join("target").join(target).join("release");
    let is_windows = target.contains("windows");
    let is_macos = target.contains("darwin") || target.contains("apple");

    let filename = match kind {
        CrateKind::Bin => {
            if is_windows {
                format!("{name}.exe")
            } else {
                name.to_string()
            }
        }
        CrateKind::Cdylib | CrateKind::Dylib => {
            if is_windows {
                format!("{name}.dll")
            } else if is_macos {
                format!("lib{name}.dylib")
            } else {
                format!("lib{name}.so")
            }
        }
        CrateKind::Staticlib => {
            if is_windows {
                format!("{name}.lib")
            } else {
                format!("lib{name}.a")
            }
        }
    };
    release_dir.join(filename)
}

/// Detect the default target in the project via `cargo metadata`.
/// For `CrateKind::Bin` this picks the unique `[[bin]]` target
/// (bailing if there are multiple). For lib-ish kinds it picks the
/// `[lib]` target name, requiring the project to have one.
fn detect_default_target(project_dir: &Path, kind: CrateKind) -> Result<String> {
    let out = Command::new("cargo")
        .current_dir(project_dir)
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .output()
        .context("cargo metadata")?;
    if !out.status.success() {
        anyhow::bail!("cargo metadata failed");
    }
    let stdout = String::from_utf8_lossy(&out.stdout);

    // `cargo metadata` JSON has per-package `targets: [{kind:[...], crate_types:[...], name:"...", ...}, ...]`.
    // For each `"kind":[...]` array we find, we parse its bracketed contents
    // and check whether any wanted crate-type token is present, then walk
    // *forward* to the next `"name":"..."` (which, in cargo metadata's output
    // ordering, is that target's own name). Walking backward would land on
    // the previous `"name":"..."` field — typically a dependency entry like
    // `{"name":"toml", "kind":null, ...}` — and produce a bogus target name.
    //
    // For lib-ish kinds we accept any of the five lib crate-types so that a
    // `crate-type = ["cdylib", "rlib"]` lib still resolves correctly; cargo
    // itself enforces that the requested crate-type is actually present.
    let wanted: &[&str] = match kind {
        CrateKind::Bin => &["bin"],
        CrateKind::Cdylib | CrateKind::Dylib | CrateKind::Staticlib => {
            &["lib", "rlib", "cdylib", "dylib", "staticlib"]
        }
    };

    let mut found: Option<String> = None;
    let mut cursor = 0;
    while let Some(rel) = stdout[cursor..].find("\"kind\":[") {
        let arr_start = cursor + rel + "\"kind\":[".len();
        let arr_end = match stdout[arr_start..].find(']') {
            Some(e) => arr_start + e,
            None => break,
        };
        let arr = &stdout[arr_start..arr_end];
        cursor = arr_end + 1;

        let matches_wanted = wanted.iter().any(|w| {
            let quoted = format!("\"{w}\"");
            arr.split(',').any(|tok| tok.trim() == quoted)
        });
        if !matches_wanted {
            continue;
        }

        let name_marker = "\"name\":\"";
        if let Some(nrel) = stdout[cursor..].find(name_marker) {
            let nstart = cursor + nrel + name_marker.len();
            if let Some(end_rel) = stdout[nstart..].find('"') {
                let name = &stdout[nstart..nstart + end_rel];
                match (&found, kind) {
                    (Some(existing), CrateKind::Bin) if existing != name => {
                        anyhow::bail!(
                            "project has multiple bin targets; please pass --bin <NAME>"
                        );
                    }
                    _ => {
                        found = Some(name.to_string());
                    }
                }
            }
        }
    }
    found.ok_or_else(|| {
        anyhow::anyhow!(
            "no {} target found in project",
            match kind {
                CrateKind::Bin => "bin",
                _ => "lib",
            }
        )
    })
}

/// Resolve the path of the real system linker for `target`.
///
/// Resolution order (first match wins):
///   1. `$LYRA_REAL_LINKER` env var — explicit override for any target.
///   2. MSVC: `cc::windows_registry::find_tool` → `link.exe`.
///   3. windows-gnullvm: `clang` (uses LLVM's lld internally).
///   4. windows-gnu (MinGW): arch-specific cross-gcc, then plain `gcc`.
///   5. macOS/darwin: `cc`.
///   6. Everything else (linux-gnu, linux-musl, etc.): `cc`.
fn find_real_linker(target: &str) -> Result<PathBuf> {
    if let Ok(v) = std::env::var("LYRA_REAL_LINKER") {
        return Ok(PathBuf::from(v));
    }

    if target.contains("msvc") {
        if let Some(tool) = cc::windows_registry::find_tool(target, "link.exe") {
            return Ok(tool.path().to_path_buf());
        }
        Ok(PathBuf::from("link.exe"))
    } else if target.contains("windows") && target.contains("gnullvm") {
        // gnullvm targets use clang as the linker driver (clang invokes
        // lld internally with the right MinGW sysroot).
        Ok(PathBuf::from("clang"))
    } else if target.contains("windows") && target.contains("gnu") {
        let arch_prefix = if target.starts_with("x86_64") {
            "x86_64-w64-mingw32"
        } else if target.starts_with("i686") || target.starts_with("i586") {
            "i686-w64-mingw32"
        } else if target.starts_with("aarch64") {
            "aarch64-w64-mingw32"
        } else {
            "x86_64-w64-mingw32"
        };
        let cross = format!("{}-gcc", arch_prefix);
        if probe_tool(&cross) {
            return Ok(PathBuf::from(cross));
        }
        Ok(PathBuf::from("gcc"))
    } else if target.contains("darwin") || target.contains("apple") {
        Ok(PathBuf::from("cc"))
    } else {
        Ok(PathBuf::from("cc"))
    }
}

/// Returns true if `name` is reachable on PATH by running `name --version`.
/// Spawns a process once per build so the cost is negligible.
fn probe_tool(name: &str) -> bool {
    std::process::Command::new(name)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Path to a sibling executable (e.g. `lyra_linker`) in the same directory
/// as the currently running lyra binary.
fn find_self_bin(name: &str) -> Result<PathBuf> {
    let me = std::env::current_exe().context("current_exe")?;
    let dir = me
        .parent()
        .ok_or_else(|| anyhow::anyhow!("current_exe has no parent"))?;

    let with_exe = dir.join(format!("{name}.exe"));
    if with_exe.exists() {
        return Ok(with_exe);
    }
    let no_exe = dir.join(name);
    if no_exe.exists() {
        return Ok(no_exe);
    }

    // `cargo run --bin lyra` / `cargo build --bin lyra` builds *only* that
    // bin, so the sibling shims are absent from target/{debug,release} even
    // though the tree is perfectly healthy. That is the overwhelmingly
    // common cause of this error, but it applies only to a cargo output
    // dir — in an installed layout a missing shim is a genuinely broken
    // install and deserves different advice. The cargo layout is
    // `target/{debug,release}`, or `target/<triple>/{debug,release}` when
    // lyra itself was built with an explicit --target.
    let in_cargo_target_dir = matches!(
        dir.file_name().and_then(|s| s.to_str()),
        Some("debug") | Some("release")
    ) && dir.ancestors().skip(1).take(2).any(|p| p.ends_with("target"));

    if in_cargo_target_dir {
        anyhow::bail!(
            "could not find '{name}' next to {}\n\n\
             lyra needs its sibling shims ('lyra_linker', 'lyra_wrapper') in the \
             same directory, and `cargo run --bin lyra` builds only `lyra`.\n\
             Build all three first, then re-run:\n\n    \
             cargo build --bins\n\n\
             (`cargo run` has no `--bins` flag, so the build step is separate; \
             after it, either `cargo run --bin lyra -- ...` or \
             `{}/lyra ...` works.)",
            me.display(),
            dir.display()
        );
    }

    anyhow::bail!(
        "could not find '{name}' next to {}\n\n\
         lyra expects 'lyra_linker' and 'lyra_wrapper' to sit alongside the \
         'lyra' executable. Reinstall lyra, or copy the missing shim into {}.",
        me.display(),
        dir.display()
    );
}

// ===========================================================================
// IR obfuscation
// ===========================================================================

/// Flags for a standalone IR obfuscation call (used by the internal
/// `__obfuscate-ir` subcommand invoked by `lyra_wrapper`). Only the
/// passes currently implemented appear here. New flags are added as
/// new passes land (see ROADMAP.md).
#[derive(Clone, Copy, Default)]
struct ObfuscateFlags {
    string_enc: bool,
    shuffle_blocks: bool,
    indirect_branch: bool,
    mba: bool,
}

fn obfuscate_ir_file(input_path: &Path, output_path: &Path, flags: ObfuscateFlags) -> Result<()> {
    let input_ir = fs::read_to_string(input_path).context("read IR file")?;
    let file_name = input_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("input.ll");

    let context = LLVMContext::create();
    // inkwell 0.9's `create_from_memory_range_copy` asserts the input byte
    // slice is null-terminated (panics with `input byte slice must terminate
    // with a nul byte` otherwise). `fs::read_to_string` gives us bytes ending
    // in whatever the file ends in (typically `\n`), so append a `\0`
    // explicitly here.
    let mut input_bytes = input_ir.into_bytes();
    if !input_bytes.ends_with(b"\0") {
        input_bytes.push(0);
    }
    let buffer = MemoryBuffer::create_from_memory_range_copy(&input_bytes, file_name);
    let module = context
        .create_module_from_ir(buffer)
        .map_err(|e| anyhow::anyhow!("parse IR: {}", e))?;

    // Strip debug info first so the obfuscation passes don't have to deal
    // with `!dbg` metadata and file/line maps.
    unsafe {
        use inkwell::llvm_sys::debuginfo::LLVMStripModuleDebugInfo;
        LLVMStripModuleDebugInfo(module.as_mut_ptr());
    }

    let mut obfuscator = obfuscator::Obfuscator {
        context: &context,
        module,
    };

    if flags.string_enc {
        obfuscator
            .apply_string_encryption()
            .context("String encryption failed")?;
    }
    if flags.shuffle_blocks {
        obfuscator
            .apply_shuffle_blocks()
            .context("Shuffle blocks failed")?;
    }
    if flags.indirect_branch {
        obfuscator
            .apply_indirect_branch()
            .context("Indirect branch failed")?;
    }
    if flags.mba {
        obfuscator
            .apply_mba()
            .context("MBA failed")?;
    }

    obfuscator
        .module
        .print_to_file(output_path)
        .map_err(|e| anyhow::anyhow!("write obfuscated IR: {}", e))?;
    Ok(())
}

/// Internal entry point invoked by `lyra_wrapper`:
///   lyra __obfuscate-ir <in.ll> <out.ll> [--string-enc] [--shuffle-blocks] [--indirect-branch]
fn run_internal_obfuscate(args: &[String]) -> Result<()> {
    if args.len() < 2 {
        anyhow::bail!("usage: lyra __obfuscate-ir <in.ll> <out.ll> [FLAGS]");
    }
    let input = PathBuf::from(&args[0]);
    let output = PathBuf::from(&args[1]);

    let mut flags = ObfuscateFlags::default();

    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--string-enc" => flags.string_enc = true,
            "--shuffle-blocks" => flags.shuffle_blocks = true,
            "--indirect-branch" => flags.indirect_branch = true,
            "--mba" => flags.mba = true,
            other => anyhow::bail!("unknown flag '{other}' in __obfuscate-ir"),
        }
        i += 1;
    }

    eprintln!(
        "[lyra __obfuscate-ir] {} -> {} (string_enc={}, shuffle_blocks={}, indirect_branch={}, mba={})",
        input.display(),
        output.display(),
        flags.string_enc,
        flags.shuffle_blocks,
        flags.indirect_branch,
        flags.mba,
    );
    obfuscate_ir_file(&input, &output, flags)
}
