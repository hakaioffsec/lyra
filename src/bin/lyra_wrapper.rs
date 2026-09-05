// Copyright (c) 2025 Hakai Offensive Security.
// Licensed under GNU GPL version 3 or later (see LICENSE).
// Generated-output permission: see LICENSE-EXCEPTION.md.
//
// lyra_wrapper - RUSTC_WRAPPER invoked by cargo once per crate compile.
//
// For each rustc invocation cargo hands us the rustc path plus all the
// flags cargo wants passed. We parse the args to find --crate-name and
// --crate-type. If the crate is in LYRA_OBFUSCATE_CRATES we intercept:
//
//   - For LIB crates (--crate-type=lib/rlib):
//       Phase A: re-run rustc with --emit extended to include llvm-ir so
//                rustc emits both the rlib AND the crate's IR.
//       Post:    call lyra __obfuscate-ir to obfuscate the IR,
//                llc+clang to compile it to a .o,
//                then rewrite the rlib in place, replacing the main
//                codegen-unit .o with our obfuscated .o.
//
//   - For BIN crates (--crate-type=bin):
//       Phase A: re-run rustc with --emit=llvm-ir only (no link) so we
//                can obtain the IR.
//       Post:    obfuscate the IR and compile to .o.
//       Phase B: re-run rustc with the original args PLUS
//                -C linker=<lyra_linker.exe>. When rustc invokes the
//                linker, the shim swaps the bin crate's .rcgu.o files for
//                our pre-built obfuscated .o and then calls the real
//                linker (LYRA_REAL_LINKER).
//
//   - For any other crate: exec real rustc as-is (pass-through).
//
// Environment variables:
//   LYRA_OBFUSCATE_CRATES   comma-separated list of crate names to intercept
//   LYRA_OBF_DIR            writable directory for obfuscated artefacts
//                           and the per-crate obj map
//   LYRA_LYRA_EXE           absolute path to the lyra binary that can run
//                           `__obfuscate-ir` in a subprocess
//   LYRA_LLC                path to llc
//   LYRA_CLANG              path to clang
//   LYRA_LINKER_SHIM        absolute path to lyra_linker (for bin)
//   LYRA_REAL_LINKER        path to the real system linker
//   LYRA_TARGET             target triple, e.g. x86_64-pc-windows-msvc,
//                           aarch64-apple-darwin, x86_64-unknown-linux-gnu
//   LYRA_OBFUSCATE_FLAGS    space-separated flags forwarded to
//                           `lyra __obfuscate-ir` (e.g. "--string-enc")
//   LYRA_DEBUG              if set, print extra diagnostics

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{exit, Command};

fn main() {
    let all: Vec<String> = env::args().skip(1).collect();
    if all.is_empty() {
        eprintln!("lyra_wrapper: no arguments");
        exit(1);
    }

    let rustc = all[0].clone();
    let rustc_args: Vec<String> = all[1..].to_vec();
    let debug = env::var("LYRA_DEBUG").is_ok();

    // Very early pass-through: cargo calls `rustc -vV` and `rustc --version`
    // and other probing commands. We should not attempt to intercept those.
    // Rule of thumb: if the args don't include --crate-name, pass through.
    let crate_name = find_opt_value(&rustc_args, "--crate-name");
    let crate_name = match crate_name {
        Some(n) => n,
        None => {
            let st = Command::new(&rustc).args(&rustc_args).status();
            exit(match st {
                Ok(s) => s.code().unwrap_or(1),
                Err(e) => {
                    eprintln!("lyra_wrapper: failed to exec rustc: {e}");
                    1
                }
            });
        }
    };

    let targets: Vec<String> = env::var("LYRA_OBFUSCATE_CRATES")
        .map(|s| s.split(',').map(|s| s.trim().to_string()).collect())
        .unwrap_or_default();

    let should_obfuscate = targets.iter().any(|t| *t == crate_name);

    if !should_obfuscate {
        let st = Command::new(&rustc).args(&rustc_args).status();
        exit(match st {
            Ok(s) => s.code().unwrap_or(1),
            Err(e) => {
                eprintln!("lyra_wrapper: exec rustc failed: {e}");
                1
            }
        });
    }

    // Determine crate types.
    //
    // Routing:
    //   * `bin`, `cdylib`, `dylib` → handle_bin_crate (two-phase, uses
    //     linker shim). They all produce a linked artefact (.exe/.dll/
    //     .so/.dylib) via link.exe/ld, which the shim can intercept
    //     regardless of whether the artefact is an executable or a
    //     shared library.
    //   * `lib`, `rlib` → handle_lib_crate (emit IR, obfuscate, patch
    //     the main .o inside the resulting rlib archive).
    //   * `staticlib` → currently falls through to pass-through. A
    //     staticlib produces a `.lib`/`.a` archive without invoking
    //     the linker; a proper staticlib handler that patches the
    //     archive in place is a follow-up.
    let crate_types = find_crate_types(&rustc_args);
    let is_linked_output = crate_types
        .iter()
        .any(|t| t == "bin" || t == "cdylib" || t == "dylib");
    let is_rlib_output = crate_types.iter().any(|t| t == "lib" || t == "rlib");

    if debug {
        eprintln!(
            "[lyra_wrapper] crate='{crate_name}' types={crate_types:?} \
             is_linked_output={is_linked_output} is_rlib_output={is_rlib_output}"
        );
    }

    let obf_dir =
        PathBuf::from(env::var("LYRA_OBF_DIR").expect("lyra_wrapper: LYRA_OBF_DIR not set"));
    fs::create_dir_all(&obf_dir).ok();

    let lyra_exe = env::var("LYRA_LYRA_EXE").expect("lyra_wrapper: LYRA_LYRA_EXE not set");
    let llc = env::var("LYRA_LLC").expect("lyra_wrapper: LYRA_LLC not set");
    let clang = env::var("LYRA_CLANG").expect("lyra_wrapper: LYRA_CLANG not set");
    let target = env::var("LYRA_TARGET").expect("lyra_wrapper: LYRA_TARGET not set");
    let obf_flags: Vec<String> = env::var("LYRA_OBFUSCATE_FLAGS")
        .unwrap_or_default()
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();

    let rc = if is_linked_output {
        handle_bin_crate(
            &rustc,
            &rustc_args,
            &crate_name,
            &obf_dir,
            &lyra_exe,
            &llc,
            &clang,
            &target,
            &obf_flags,
            debug,
        )
    } else if is_rlib_output {
        handle_lib_crate(
            &rustc,
            &rustc_args,
            &crate_name,
            &obf_dir,
            &lyra_exe,
            &llc,
            &clang,
            &target,
            &obf_flags,
            debug,
        )
    } else {
        // staticlib or unknown crate type — pass through untouched for
        // now. A staticlib-specific handler that patches the .lib/.a
        // archive is planned but not implemented.
        Command::new(&rustc)
            .args(&rustc_args)
            .status()
            .map(|s| s.code().unwrap_or(1))
            .unwrap_or(1)
    };
    exit(rc);
}

// ---------------------------------------------------------------------------
// LIB crate handling
// ---------------------------------------------------------------------------

fn handle_lib_crate(
    rustc: &str,
    rustc_args: &[String],
    crate_name: &str,
    obf_dir: &Path,
    lyra_exe: &str,
    llc: &str,
    clang: &str,
    target: &str,
    obf_flags: &[String],
    debug: bool,
) -> i32 {
    let out_dir = find_opt_value(rustc_args, "--out-dir").unwrap_or_default();

    // Phase A: run rustc with --emit extended to include llvm-ir. We let
    // rustc write the .ll to the default location (out-dir/<crate>-<hash>...)
    // rather than forcing an explicit path, because some environments don't
    // let rustc write directly to a path outside out-dir.
    let modified = ensure_emit_has_llvm_ir(rustc_args);
    if debug {
        eprintln!("[lyra_wrapper] LIB phase A: rustc {}", modified.join(" "));
    }
    let status = match Command::new(rustc).args(&modified).status() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[lyra_wrapper] failed to exec rustc: {e}");
            return 1;
        }
    };
    if !status.success() {
        return status.code().unwrap_or(1);
    }

    // Find the .ll emitted by rustc in the out-dir. Filename pattern depends
    // on --extra-filename but is deterministic: <crate><extra>.<crate>.<cgu_hash>-cgu.N.rcgu.ll
    // We match "<normalized-crate><extra>" at the start.
    let ll_path = match find_emitted_ll(Path::new(&out_dir), crate_name, rustc_args) {
        Some(p) => p,
        None => {
            eprintln!(
                "[lyra_wrapper] could not find emitted .ll for crate '{crate_name}' in '{out_dir}'"
            );
            return 1;
        }
    };
    if debug {
        eprintln!("[lyra_wrapper] found IR at {}", ll_path.display());
    }

    // Obfuscate IR via lyra subprocess
    let obf_ll = obf_dir.join(format!("{crate_name}.obf.ll"));
    if !run_obfuscate_subprocess(lyra_exe, &ll_path, &obf_ll, obf_flags, debug) {
        return 1;
    }

    // llc + clang -> .o
    let obf_o = obf_dir.join(format!("{crate_name}.obf.o"));
    let reloc = reloc_model_for(rustc_args);
    if !compile_ir_to_obj(llc, clang, target, &reloc, &obf_ll, &obf_o, debug) {
        return 1;
    }

    // Patch the rlib
    let rlib_path = find_rlib(&PathBuf::from(&out_dir), crate_name, rustc_args);
    let rlib_path = match rlib_path {
        Some(p) => p,
        None => {
            eprintln!("[lyra_wrapper] could not locate rlib for '{crate_name}' in '{out_dir}'");
            return 1;
        }
    };

    if debug {
        eprintln!(
            "[lyra_wrapper] LIB patching rlib '{}' replacing main .o with '{}'",
            rlib_path.display(),
            obf_o.display()
        );
    }

    if let Err(e) = patch_rlib_replace_main_o(&rlib_path, crate_name, &obf_o) {
        eprintln!("[lyra_wrapper] rlib patch failed: {e}");
        return 1;
    }

    0
}

// ---------------------------------------------------------------------------
// BIN crate handling
// ---------------------------------------------------------------------------

fn handle_bin_crate(
    rustc: &str,
    rustc_args: &[String],
    crate_name: &str,
    obf_dir: &Path,
    lyra_exe: &str,
    llc: &str,
    clang: &str,
    target: &str,
    obf_flags: &[String],
    debug: bool,
) -> i32 {
    let out_dir = find_opt_value(rustc_args, "--out-dir").unwrap_or_default();

    // Phase A: emit IR only (no link) by dropping all --emit args and
    // adding a single `--emit=llvm-ir` (default location in out-dir).
    let mut phase_a = strip_emit_args(rustc_args);
    phase_a.push("--emit=llvm-ir".to_string());
    if debug {
        eprintln!("[lyra_wrapper] BIN phase A: rustc {}", phase_a.join(" "));
    }
    let status = match Command::new(rustc).args(&phase_a).status() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[lyra_wrapper] rustc phase A exec failed: {e}");
            return 1;
        }
    };
    if !status.success() {
        return status.code().unwrap_or(1);
    }

    // Locate the .ll in out-dir
    let ll_path = match find_emitted_ll(Path::new(&out_dir), crate_name, rustc_args) {
        Some(p) => p,
        None => {
            eprintln!(
                "[lyra_wrapper] could not find emitted .ll for bin '{crate_name}' in '{out_dir}'"
            );
            return 1;
        }
    };

    // Obfuscate
    let obf_ll = obf_dir.join(format!("{crate_name}.obf.ll"));
    if !run_obfuscate_subprocess(lyra_exe, &ll_path, &obf_ll, obf_flags, debug) {
        return 1;
    }

    // llc+clang -> .o
    let obf_o = obf_dir.join(format!("{crate_name}.obf.o"));
    let reloc = reloc_model_for(rustc_args);
    if !compile_ir_to_obj(llc, clang, target, &reloc, &obf_ll, &obf_o, debug) {
        return 1;
    }

    // Phase B
    let shim = env::var("LYRA_LINKER_SHIM").expect("lyra_wrapper: LYRA_LINKER_SHIM not set");

    let mut phase_b: Vec<String> = rustc_args.to_vec();
    phase_b.push("-C".to_string());
    phase_b.push(format!("linker={shim}"));
    phase_b.push("--allow".to_string());
    phase_b.push("linker_messages".to_string());

    if debug {
        eprintln!("[lyra_wrapper] BIN phase B: rustc {}", phase_b.join(" "));
    }
    let status = match Command::new(rustc).args(&phase_b).status() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[lyra_wrapper] rustc phase B exec failed: {e}");
            return 1;
        }
    };
    status.code().unwrap_or(1)
}

/// Find the .ll emitted by rustc for `crate_name` in `out_dir`.
/// rustc names the file `<crate><extra>.<cgu>.rcgu.ll` or simpler patterns.
/// We pick the newest matching candidate.
fn find_emitted_ll(out_dir: &Path, crate_name: &str, rustc_args: &[String]) -> Option<PathBuf> {
    let normalized = crate_name.replace('-', "_");
    let extra = find_c_value(rustc_args, "extra-filename").unwrap_or_default();

    let mut candidates: Vec<PathBuf> = Vec::new();
    let entries = fs::read_dir(out_dir).ok()?;
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().and_then(|s| s.to_str()) != Some("ll") {
            continue;
        }
        let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let prefix_with_extra = format!("{normalized}{extra}");
        if name.starts_with(&prefix_with_extra)
            || name.starts_with(&format!("{normalized}-"))
            || name.starts_with(&format!("{normalized}."))
        {
            candidates.push(p);
        }
    }

    candidates.sort_by_key(|p| fs::metadata(p).and_then(|m| m.modified()).ok());
    candidates.pop()
}

// ---------------------------------------------------------------------------
// rlib patching
// ---------------------------------------------------------------------------

/// Rewrite `rlib_path` in place, replacing the main codegen-unit object
/// with the bytes of `new_obj`.
///
/// Rust rlibs are produced by rustc's own `ar`-like code path and have
/// specific expectations (GNU variant, symbol index, in-order members
/// starting with `lib.rmeta`, ...). The Rust `ar` crate's output is
/// rejected by MSVC link.exe with `LNK4003: invalid library format`.
/// Instead we use `llvm-ar` (bundled in LLVM_BIN) via `d` (delete) +
/// `q` (quick append) so the archive keeps rustc's original structure.
///
/// Specifically:
///   1. list members with `llvm-ar t`
///   2. identify the main .o whose name starts with `<crate>`
///   3. `llvm-ar dv <rlib> <main_o_name>` to delete that member
///   4. copy our new obj to a temp file named exactly `<main_o_name>`
///   5. `llvm-ar rv <rlib> <tmp_file>` to append/replace with new contents
fn patch_rlib_replace_main_o(
    rlib_path: &Path,
    crate_name: &str,
    new_obj: &Path,
) -> std::io::Result<()> {
    let llvm_ar = env::var("LYRA_LLVM_AR").unwrap_or_else(|_| "llvm-ar".to_string());

    // 1) list members
    let out = Command::new(&llvm_ar).arg("t").arg(rlib_path).output()?;
    if !out.status.success() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!(
                "llvm-ar t failed: status={:?}, stderr={}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            ),
        ));
    }
    let listing = String::from_utf8_lossy(&out.stdout).to_string();
    let normalized = crate_name.replace('-', "_");

    let mut main_names: Vec<String> = Vec::new();
    for line in listing.lines() {
        let ln = line.trim();
        if (ln.ends_with(".o") || ln.ends_with(".obj"))
            && (ln.starts_with(&format!("{normalized}-"))
                || ln.starts_with(&format!("{normalized}.")))
        {
            main_names.push(ln.to_string());
        }
    }

    if main_names.is_empty() {
        eprintln!(
            "[lyra_wrapper] WARNING: no main .o in rlib '{}' matched crate '{crate_name}'; skipping",
            rlib_path.display()
        );
        return Ok(());
    }

    // 2) delete all main .o members
    for n in &main_names {
        let status = Command::new(&llvm_ar)
            .arg("d")
            .arg(rlib_path)
            .arg(n)
            .status()?;
        if !status.success() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("llvm-ar d {n} failed"),
            ));
        }
    }

    // 3) copy the obf .o to a temp file with the FIRST main_name so the
    // new archive member keeps the same identifier rustc expects.
    let staging = rlib_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(".lyra_stage_{}", &main_names[0]));
    fs::copy(new_obj, &staging)?;

    // 4) append (rv = insert-or-replace verbose). Because we just deleted
    // the original, this ends up appending.
    let status = Command::new(&llvm_ar)
        .arg("r")
        .arg(rlib_path)
        .arg(&staging)
        .status()?;
    let _ = fs::remove_file(&staging);
    if !status.success() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("llvm-ar r failed"),
        ));
    }

    Ok(())
}

/// Locate the rlib that cargo will emit for `crate_name` in `out_dir`.
/// The filename is `lib<crate>-<metadata>.rlib` or `<crate>-<metadata>.rlib`.
fn find_rlib(out_dir: &Path, crate_name: &str, rustc_args: &[String]) -> Option<PathBuf> {
    // Prefer the explicit extra-filename if present:
    if let Some(ef) = find_c_value(rustc_args, "extra-filename") {
        let normalized = crate_name.replace('-', "_");
        for prefix in [
            format!("lib{normalized}{ef}.rlib"),
            format!("{normalized}{ef}.rlib"),
        ] {
            let p = out_dir.join(&prefix);
            if p.exists() {
                return Some(p);
            }
        }
    }

    // Otherwise scan out_dir for *<crate>*.rlib
    let normalized = crate_name.replace('-', "_");
    if let Ok(entries) = fs::read_dir(out_dir) {
        let mut candidates: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("rlib"))
            .filter(|p| {
                let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
                name.starts_with(&format!("lib{normalized}-"))
                    || name.starts_with(&format!("{normalized}-"))
                    || name.starts_with(&format!("lib{normalized}."))
                    || name.starts_with(&format!("{normalized}."))
            })
            .collect();
        candidates.sort_by_key(|p| fs::metadata(p).and_then(|m| m.modified()).ok());
        return candidates.pop();
    }
    None
}

// ---------------------------------------------------------------------------
// Subprocess helpers
// ---------------------------------------------------------------------------

fn run_obfuscate_subprocess(
    lyra_exe: &str,
    ir_in: &Path,
    ir_out: &Path,
    obf_flags: &[String],
    debug: bool,
) -> bool {
    let mut cmd = Command::new(lyra_exe);
    cmd.arg("__obfuscate-ir").arg(ir_in).arg(ir_out);
    for f in obf_flags {
        cmd.arg(f);
    }
    if debug {
        eprintln!(
            "[lyra_wrapper] obfuscate: {} __obfuscate-ir {} {} {:?}",
            lyra_exe,
            ir_in.display(),
            ir_out.display(),
            obf_flags
        );
    }
    match cmd.status() {
        Ok(s) if s.success() => true,
        Ok(s) => {
            eprintln!(
                "[lyra_wrapper] obfuscate subprocess exit {}",
                s.code().unwrap_or(-1)
            );
            false
        }
        Err(e) => {
            eprintln!("[lyra_wrapper] obfuscate subprocess failed: {e}");
            false
        }
    }
}

fn compile_ir_to_obj(
    llc: &str,
    clang: &str,
    target: &str,
    reloc: &str,
    ll: &Path,
    out_o: &Path,
    debug: bool,
) -> bool {
    let asm = out_o.with_extension("s");

    if debug {
        eprintln!(
            "[lyra_wrapper] llc -mtriple={target} --relocation-model={reloc} {} -filetype=asm -o {}",
            ll.display(),
            asm.display()
        );
    }
    match Command::new(llc)
        .args([
            &format!("-mtriple={target}"),
            &format!("--relocation-model={reloc}"),
            //"-O0",
            ll.to_str().unwrap(),
            "-filetype=asm",
            "-o",
            asm.to_str().unwrap(),
        ])
        .status()
    {
        Ok(s) if s.success() => {}
        Ok(s) => {
            eprintln!(
                "[lyra_wrapper] llc exit={:?} (cmd: {llc} -mtriple={target} --relocation-model={reloc} {} -filetype=asm -o {})",
                s.code(),
                ll.display(),
                asm.display()
            );
            return false;
        }
        Err(e) => {
            eprintln!("[lyra_wrapper] failed to spawn llc '{llc}': {e}");
            return false;
        }
    }

    // Assemble only. Deliberately no -fPIC/-fPIE here: those are front-end
    // codegen flags, and on a `.s` input clang dispatches straight to the
    // integrated assembler, which never sees them (verified: the four
    // variants produce byte-identical objects). RIP-relative vs absolute
    // addressing is already baked into the asm by llc above, so the
    // relocation model is controlled there and only there.
    if debug {
        eprintln!(
            "[lyra_wrapper] clang --target={target} -c {} -o {}",
            asm.display(),
            out_o.display()
        );
    }
    match Command::new(clang)
        .args([
            &format!("--target={target}"),
            "-c",
            asm.to_str().unwrap(),
            "-o",
            out_o.to_str().unwrap(),
        ])
        .status()
    {
        Ok(s) if s.success() => {}
        Ok(s) => {
            eprintln!(
                "[lyra_wrapper] clang exit={:?} (cmd: {clang} --target={target} -c {} -o {})",
                s.code(),
                asm.display(),
                out_o.display()
            );
            return false;
        }
        Err(e) => {
            eprintln!("[lyra_wrapper] failed to spawn clang '{clang}': {e}");
            return false;
        }
    }
    true
}

// ---------------------------------------------------------------------------
// rustc arg parsing helpers
// ---------------------------------------------------------------------------

fn find_opt_value(args: &[String], name: &str) -> Option<String> {
    // Accept both "--name VALUE" and "--name=VALUE"
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == name {
            if i + 1 < args.len() {
                return Some(args[i + 1].clone());
            }
        } else if let Some(rest) = a.strip_prefix(&format!("{name}=")) {
            return Some(rest.to_string());
        }
        i += 1;
    }
    None
}

/// Relocation model to hand `llc` so the obfuscated object matches the
/// codegen model rustc used for every *other* object in the link.
///
/// `llc`'s own default is `static`, which emits 32-bit absolute addresses
/// (`movl $sym, %eax`). rustc defaults to `RelocModel::Pic` on every target
/// lyra supports, so an unqualified `llc` run produces an object that
/// disagrees with the rest of the link:
///
///   * ELF/PIE (linux-gnu, musl): hard link error —
///     `relocation R_X86_64_32 cannot be used against local symbol`.
///   * COFF (windows-msvc/gnu/gnullvm): links, but bakes in
///     `IMAGE_REL_AMD64_ADDR32` fixups that assume the image loads below
///     4 GB — unsound under high-entropy ASLR. `pic` emits the
///     `leaq sym(%rip)` form MSVC and rustc both produce.
///   * Mach-O (apple-darwin): already position-independent; `pic` is a
///     verified no-op there.
///
/// rustc only puts `-C relocation-model` on the wire when the user asked for
/// it (RUSTFLAGS / profile), so we cannot read the default back — we
/// reproduce it. An explicit user setting still wins. LLVM has no separate
/// `pie` model: PIE is `pic` plus the `PIE Level` module flag that rustc
/// already stamps into bin-crate IR, so `pie` maps onto `pic`.
fn reloc_model_for(rustc_args: &[String]) -> String {
    match find_c_value(rustc_args, "relocation-model") {
        Some(explicit) if explicit == "pie" => "pic".to_string(),
        Some(explicit) => explicit,
        None => "pic".to_string(),
    }
}

/// Find `-C KEY=VAL` or `-C KEY val` style args, returning VAL.
fn find_c_value(args: &[String], key: &str) -> Option<String> {
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "-C" && i + 1 < args.len() {
            let kv = &args[i + 1];
            if let Some(rest) = kv.strip_prefix(&format!("{key}=")) {
                return Some(rest.to_string());
            }
        } else if let Some(rest) = a.strip_prefix(&format!("-C{key}=")) {
            return Some(rest.to_string());
        }
        i += 1;
    }
    None
}

fn find_crate_types(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--crate-type" {
            if i + 1 < args.len() {
                for t in args[i + 1].split(',') {
                    out.push(t.trim().to_string());
                }
                i += 2;
                continue;
            }
        } else if let Some(rest) = a.strip_prefix("--crate-type=") {
            for t in rest.split(',') {
                out.push(t.trim().to_string());
            }
        }
        i += 1;
    }
    out
}

/// Rewrite args so that any `--emit=...` (or `--emit ...`) includes
/// `llvm-ir`. Preserves all existing emit kinds.
fn ensure_emit_has_llvm_ir(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    let mut handled = false;
    while i < args.len() {
        let a = &args[i];
        if a == "--emit" {
            if i + 1 < args.len() {
                let existing = &args[i + 1];
                out.push(a.clone());
                out.push(merge_emit_with_llvm_ir(existing));
                i += 2;
                handled = true;
                continue;
            }
        } else if let Some(rest) = a.strip_prefix("--emit=") {
            out.push(format!("--emit={}", merge_emit_with_llvm_ir(rest)));
            handled = true;
            i += 1;
            continue;
        }
        out.push(a.clone());
        i += 1;
    }
    if !handled {
        out.push("--emit=link,dep-info,llvm-ir".to_string());
    }
    out
}

fn merge_emit_with_llvm_ir(existing: &str) -> String {
    let parts: Vec<&str> = existing
        .split(',')
        .filter(|p| !p.trim_start().starts_with("llvm-ir"))
        .collect();
    let mut s = parts.join(",");
    if !s.is_empty() {
        s.push(',');
    }
    s.push_str("llvm-ir");
    s
}

/// Remove all `--emit ...` / `--emit=...` arguments from the arg list.
fn strip_emit_args(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--emit" {
            i += 2;
            continue;
        }
        if a.starts_with("--emit=") {
            i += 1;
            continue;
        }
        out.push(a.clone());
        i += 1;
    }
    out
}
