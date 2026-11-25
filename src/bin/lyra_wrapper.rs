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
//       Run rustc once with --emit extended to include llvm-ir so it
//       emits both the rlib AND the crate's IR.
//       Post:    call lyra __obfuscate-ir to obfuscate the IR,
//                llc to compile it directly to a .o,
//                then rewrite the rlib in place, replacing the main
//                codegen-unit .o with our obfuscated .o.
//
//   - For linked outputs (bin/cdylib/dylib):
//       Run rustc once with link + llvm-ir emission and our linker shim.
//       The IR exists before linking; the shim calls our __link-codegen
//       mode to produce the transformed object, substitutes it, and relays
//       the original linker. Codegen output is replayed after rustc returns.
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
//   LYRA_LINKER_SHIM        absolute path to lyra_linker (for bin)
//   LYRA_REAL_LINKER        fallback when rustc has no explicit -C linker
//   LYRA_TARGET             target triple, e.g. x86_64-pc-windows-msvc,
//                           aarch64-apple-darwin, x86_64-unknown-linux-gnu
//   LYRA_OBFUSCATE_FLAGS    space-separated flags forwarded to
//                           `lyra __obfuscate-ir` (e.g. "--string-enc")
//   LYRA_DEBUG              if set, print extra diagnostics

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{exit, Command};

#[path = "../audit.rs"]
mod audit;

fn main() {
    let all: Vec<String> = env::args().skip(1).collect();
    if all.is_empty() {
        eprintln!("lyra_wrapper: no arguments");
        exit(1);
    }
    if all[0] == "__link-codegen" {
        exit(prepare_link_object());
    }

    let rustc = &all[0];
    let rustc_args = &all[1..];
    let debug = env::var("LYRA_DEBUG").is_ok();

    // Very early pass-through: cargo calls `rustc -vV` and `rustc --version`
    // and other probing commands. We should not attempt to intercept those.
    // Rule of thumb: if the args don't include --crate-name, pass through.
    let crate_name = find_opt_value(rustc_args, "--crate-name");
    let crate_name = match crate_name {
        Some(n) => n,
        None => {
            let st = Command::new(rustc).args(rustc_args).status();
            exit(match st {
                Ok(s) => s.code().unwrap_or(1),
                Err(e) => {
                    eprintln!("lyra_wrapper: failed to exec rustc: {e}");
                    1
                }
            });
        }
    };

    let targets = env::var("LYRA_OBFUSCATE_CRATES").unwrap_or_default();

    let should_obfuscate = targets.split(',').any(|target| target.trim() == crate_name)
        && find_opt_value(rustc_args, "--target")
            .is_some_and(|target| env::var("LYRA_TARGET").is_ok_and(|expected| target == expected));
    // Cargo compiles build dependencies for the host without --target. Never
    // lower those with the deployment target, even when the package is selected.

    if !should_obfuscate {
        let st = Command::new(rustc).args(rustc_args).status();
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
    //   * `bin`, `cdylib`, `dylib` → handle_bin_crate (one rustc invocation,
    //     transformation at the linker boundary). They produce a linked artefact
    //     (.exe/.dll/.so/.dylib), which the shim can intercept
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

    let rc = if is_linked_output {
        handle_bin_crate(
            rustc,
            rustc_args,
            &crate_name,
            &obf_dir,
            is_rlib_output,
            debug,
        )
    } else if is_rlib_output {
        handle_lib_crate(rustc, rustc_args, &crate_name, &obf_dir, debug)
    } else {
        // staticlib or unknown crate type — pass through untouched for
        // now. A staticlib-specific handler that patches the .lib/.a
        // archive is planned but not implemented.
        Command::new(rustc)
            .args(rustc_args)
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
    debug: bool,
) -> i32 {
    let out_dir = find_opt_value(rustc_args, "--out-dir").unwrap_or_default();

    // Run rustc with --emit extended to include llvm-ir. We let
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
    let extra = find_c_value(rustc_args, "extra-filename").unwrap_or_default();
    let ll_path = match find_emitted_ll(Path::new(&out_dir), crate_name, &extra) {
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

    let reloc = reloc_model_for(rustc_args);
    let Some(obf_o) = generate_object(crate_name, &ll_path, obf_dir, &reloc, debug) else {
        return 1;
    };

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
    also_rlib: bool,
    debug: bool,
) -> i32 {
    let out_dir = find_opt_value(rustc_args, "--out-dir").unwrap_or_default();
    let extra = find_c_value(rustc_args, "extra-filename").unwrap_or_default();
    let shim = env::var("LYRA_LINKER_SHIM").expect("lyra_wrapper: LYRA_LINKER_SHIM not set");
    let output = obf_dir.join(format!("{crate_name}.link-codegen.stdout"));
    match fs::remove_file(&output) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            eprintln!("lyra_wrapper: cannot clear prior codegen output: {error}");
            return 1;
        }
    }
    let mut modified = ensure_emit_has_llvm_ir(rustc_args);
    modified.extend([
        "-C".to_string(),
        format!("linker={shim}"),
        "--allow".to_string(),
        "linker_messages".to_string(),
    ]);
    if debug {
        eprintln!("[lyra_wrapper] linked rustc: {}", modified.join(" "));
    }
    let mut command = Command::new(rustc);
    command
        .args(&modified)
        .env(
            "LYRA_LINK_CODEGEN",
            env::current_exe().expect("lyra_wrapper: current executable"),
        )
        .env("LYRA_LINK_CRATE", crate_name)
        .env("LYRA_LINK_OUT_DIR", &out_dir)
        .env("LYRA_LINK_EXTRA", extra)
        .env("LYRA_LINK_RELOC", reloc_model_for(rustc_args))
        .env("LYRA_LINK_STDOUT", &output);
    if let Some(linker) = find_c_value(rustc_args, "linker") {
        command.env("LYRA_REAL_LINKER", linker);
    }
    let status = match command.status() {
        Ok(status) => status,
        Err(error) => {
            eprintln!("lyra_wrapper: rustc failed to start: {error}");
            return 1;
        }
    };
    // rustc captures linker output. Replay the callback's exact stream here so
    // coverage remains machine-readable to the outer Cargo/release controller.
    match fs::File::open(&output) {
        Ok(mut input) => {
            if let Err(error) = std::io::copy(&mut input, &mut std::io::stdout().lock()) {
                eprintln!("lyra_wrapper: cannot relay codegen output: {error}");
                return 1;
            }
        }
        Err(error) if status.success() => {
            eprintln!("lyra_wrapper: linked compilation produced no codegen output: {error}");
            return 1;
        }
        Err(_) => {}
    }
    if status.success() && also_rlib {
        // Mixed cdylib+rlib crates also contribute their archive to dependants.
        // Updating only their shared library would leave those consumers plain.
        let Some(archive) = find_rlib(Path::new(&out_dir), crate_name, rustc_args) else {
            eprintln!("lyra_wrapper: missing archive for mixed-output crate {crate_name}");
            return 1;
        };
        if let Err(error) = patch_rlib_replace_main_o(
            &archive,
            crate_name,
            &obf_dir.join(format!("{crate_name}.obf.o")),
        ) {
            eprintln!("lyra_wrapper: mixed-output archive patch failed: {error}");
            return 1;
        }
    }
    status.code().unwrap_or(1)
}

fn prepare_link_object() -> i32 {
    let crate_name = env::var("LYRA_LINK_CRATE").expect("missing linked crate");
    let out_dir = env::var("LYRA_LINK_OUT_DIR").expect("missing linked output directory");
    let extra = env::var("LYRA_LINK_EXTRA").expect("missing linked filename suffix");
    let reloc = env::var("LYRA_LINK_RELOC").expect("missing linked relocation model");
    let obf_dir = PathBuf::from(env::var("LYRA_OBF_DIR").expect("missing obfuscation directory"));
    let Some(input) = find_emitted_ll(Path::new(&out_dir), &crate_name, &extra) else {
        eprintln!("lyra_wrapper: no unique LLVM IR for {crate_name}{extra} before linking");
        return 1;
    };
    let debug = env::var_os("LYRA_DEBUG").is_some();
    if generate_object(&crate_name, &input, &obf_dir, &reloc, debug).is_some() {
        0
    } else {
        1
    }
}

fn generate_object(
    crate_name: &str,
    input: &Path,
    obf_dir: &Path,
    reloc: &str,
    debug: bool,
) -> Option<PathBuf> {
    let lyra = env::var("LYRA_LYRA_EXE").expect("missing LYRA_LYRA_EXE");
    let llc = env::var("LYRA_LLC").expect("missing LYRA_LLC");
    let target = env::var("LYRA_TARGET").expect("missing LYRA_TARGET");
    let flags: Vec<String> = env::var("LYRA_OBFUSCATE_FLAGS")
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    let ir = obf_dir.join(format!("{crate_name}.obf.ll"));
    if !run_obfuscate_subprocess(&lyra, input, &ir, &flags, debug) {
        return None;
    }
    let object = obf_dir.join(format!("{crate_name}.obf.o"));
    compile_ir_to_obj(&llc, &target, reloc, &ir, &object, debug).then_some(object)
}

/// Require this invocation's exact crate/suffix and its single codegen unit.
fn find_emitted_ll(out_dir: &Path, crate_name: &str, extra: &str) -> Option<PathBuf> {
    let prefix = format!("{crate_name}{extra}.");
    let mut found = None;
    for entry in fs::read_dir(out_dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("ll") {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if name.starts_with(&prefix) {
            if found.is_some() {
                return None;
            }
            found = Some(path);
        }
    }
    found
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

    let archive_name = rlib_path.to_string_lossy();
    audit::run("archive-save", [crate_name, archive_name.as_ref()])?;

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
    audit::run("archive-finish", [crate_name, archive_name.as_ref()])?;

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
    target: &str,
    reloc: &str,
    ll: &Path,
    out_o: &Path,
    debug: bool,
) -> bool {
    // Rust's gnullvm suffix selects the MinGW linker toolchain, not a distinct
    // LLVM ABI. Match the GNU triple that rustc writes into its IR.
    let target = match target {
        "x86_64-pc-windows-gnullvm" => "x86_64-pc-windows-gnu",
        other => other,
    };
    // Emit objects directly. The assembly round trip loses COFF imported-global
    // section spelling (LLVM's \\01 symbol prefix can escape into a directive).
    // Relocation and section granularity belong to this code-generation step.
    if debug {
        eprintln!(
            "[lyra_wrapper] llc -mtriple={target} --relocation-model={reloc} --function-sections --data-sections -filetype=obj {} -o {}",
            ll.display(),
            out_o.display()
        );
    }
    match Command::new(llc)
        .args([
            "-mtriple",
            target,
            "--relocation-model",
            reloc,
            // Preserve the same full local names as Lyra's IR reader.
            "--non-global-value-max-name-size=-1",
            "--function-sections",
            "--data-sections",
            "-filetype=obj",
        ])
        .arg(ll)
        .arg("-o")
        .arg(out_o)
        .status()
    {
        Ok(status) if status.success() => true,
        Ok(status) => {
            eprintln!(
                "[lyra_wrapper] llc exit={:?}: {} -> {} (target={target}, relocation={reloc})",
                status.code(),
                ll.display(),
                out_o.display()
            );
            false
        }
        Err(error) => {
            eprintln!("[lyra_wrapper] failed to spawn llc '{llc}': {error}");
            false
        }
    }
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

/// Find `-C KEY=VAL` or `-CKEY=VAL`, with rustc's last-option precedence.
fn find_c_value(args: &[String], key: &str) -> Option<String> {
    args.iter().enumerate().rev().find_map(|(index, argument)| {
        let option = if argument == "-C" {
            args.get(index + 1)?.as_str()
        } else {
            argument.strip_prefix("-C")?
        };
        option
            .strip_prefix(key)?
            .strip_prefix('=')
            .map(str::to_owned)
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codegen_options_preserve_last_explicit_linker_and_relocation() {
        let args: Vec<String> = [
            "-Clinker=host-cc",
            "-Crelocation-model=pic",
            "-C",
            "linker=/target tools/zigcc",
            "-C",
            "relocation-model=static",
            "-Clinker-flavor=gcc",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        assert_eq!(
            find_c_value(&args, "linker").as_deref(),
            Some("/target tools/zigcc")
        );
        assert_eq!(reloc_model_for(&args), "static");
    }

    #[test]
    fn ir_selection_rejects_stale_metadata_and_multiple_codegen_units() {
        let directory = env::temp_dir().join(format!(
            "lyra-ir-selection-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        fs::create_dir(&directory).unwrap();
        let wanted = directory.join("example-current.ll");
        fs::write(&wanted, b"current").unwrap();
        fs::write(directory.join("example-stale.ll"), b"stale").unwrap();
        fs::write(directory.join("example-currently.ll"), b"wrong suffix").unwrap();
        assert_eq!(
            find_emitted_ll(&directory, "example", "-current"),
            Some(wanted.clone())
        );
        fs::remove_file(wanted).unwrap();
        assert!(find_emitted_ll(&directory, "example", "-current").is_none());
        fs::write(directory.join("example-current.0.rcgu.ll"), b"unit zero").unwrap();
        fs::write(directory.join("example-current.1.rcgu.ll"), b"unit one").unwrap();
        assert!(find_emitted_ll(&directory, "example", "-current").is_none());
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linked_executable_discards_unreachable_code_and_data() {
        let llvm_bin = env::var_os("LYRA_LLVM_BIN")
            .map(PathBuf::from)
            .or_else(|| env::var_os("LLVM_SYS_221_PREFIX").map(|p| PathBuf::from(p).join("bin")))
            .unwrap_or_default();
        let llc = llvm_bin.join(format!("llc{}", env::consts::EXE_SUFFIX));
        let clang = llvm_bin.join(format!("clang{}", env::consts::EXE_SUFFIX));
        let rustc = Command::new(env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
            .arg("-vV")
            .output()
            .expect("read native target");
        assert!(rustc.status.success());
        let version = String::from_utf8(rustc.stdout).unwrap();
        let target = version
            .lines()
            .find_map(|line| line.strip_prefix("host: "))
            .unwrap();
        let directory = env::temp_dir().join(format!(
            "lyra-sections-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        fs::create_dir(&directory).unwrap();
        let ir = directory.join("sections.ll");
        fs::write(&ir, include_str!("../../tests/dead_sections.ir")).unwrap();
        let object = directory.join("sections.o");
        assert!(compile_ir_to_obj(
            llc.to_str().unwrap(),
            target,
            "pic",
            &ir,
            &object,
            false,
        ));
        let executable = directory.join("sections");
        let linked = Command::new(clang)
            .arg(format!("--target={target}"))
            .arg("-Wl,--gc-sections")
            .arg(object)
            .arg("-o")
            .arg(&executable)
            .status()
            .unwrap();
        assert!(linked.success());
        assert!(Command::new(&executable).status().unwrap().success());
        let bytes = fs::read(executable).unwrap();
        assert!(
            !bytes
                .windows(b"LYRA_DEAD_DATA".len())
                .any(|s| s == b"LYRA_DEAD_DATA"),
            "unreachable release content survived section garbage collection"
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn rust_gnullvm_target_produces_a_windows_amd64_object() {
        let llvm_bin = env::var_os("LYRA_LLVM_BIN")
            .map(PathBuf::from)
            .or_else(|| env::var_os("LLVM_SYS_221_PREFIX").map(|p| PathBuf::from(p).join("bin")))
            .unwrap_or_default();
        let llc = llvm_bin.join(format!("llc{}", env::consts::EXE_SUFFIX));
        let directory = env::temp_dir().join(format!(
            "lyra-gnullvm-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        fs::create_dir(&directory).unwrap();
        let ir = directory.join("probe.ll");
        fs::write(&ir, include_str!("../../tests/coff_import.ir")).unwrap();
        let object = directory.join("probe.o");
        assert!(compile_ir_to_obj(
            llc.to_str().unwrap(),
            "x86_64-pc-windows-gnullvm",
            "pic",
            &ir,
            &object,
            false,
        ));
        let bytes = fs::read(&object).unwrap();
        assert_eq!(u16::from_le_bytes([bytes[0], bytes[1]]), 0x8664);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn long_local_names_preserve_native_branch_results() {
        let llvm_bin = env::var_os("LYRA_LLVM_BIN")
            .map(PathBuf::from)
            .or_else(|| env::var_os("LLVM_SYS_221_PREFIX").map(|p| PathBuf::from(p).join("bin")))
            .unwrap_or_default();
        let llc = llvm_bin.join(format!("llc{}", env::consts::EXE_SUFFIX));
        let clang = llvm_bin.join(format!("clang{}", env::consts::EXE_SUFFIX));
        let rustc = Command::new(env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
            .arg("-vV")
            .output()
            .expect("run rustc for the native target");
        assert!(rustc.status.success());
        let version = String::from_utf8(rustc.stdout).unwrap();
        let target = version
            .lines()
            .find_map(|line| line.strip_prefix("host: "))
            .expect("rustc host triple");
        let directory = env::temp_dir().join(format!(
            "lyra-long-names-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        fs::create_dir(&directory).unwrap();
        let ir = directory.join("long-names.ll");
        fs::write(
            &ir,
            include_str!("../../tests/long_names.ir.in")
                .replace("__LONG_LABEL__", &"L".repeat(1024)),
        )
        .unwrap();
        let object = directory.join("long-names.o");
        assert!(compile_ir_to_obj(
            llc.to_str().unwrap(),
            target,
            "pic",
            &ir,
            &object,
            true,
        ));
        let executable = directory.join(format!("long-names{}", env::consts::EXE_SUFFIX));
        let linked = Command::new(&clang)
            .arg(format!("--target={target}"))
            .arg(&object)
            .arg("-o")
            .arg(&executable)
            .status()
            .expect("link native name-boundary fixture");
        assert!(linked.success());
        // The IR main returns zero only when choose(1) == 12 and choose(0) == 22.
        assert!(
            Command::new(&executable).status().unwrap().success(),
            "long-prefix block names changed the two branch results"
        );
        fs::remove_dir_all(directory).unwrap();
    }
}
