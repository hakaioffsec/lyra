// Copyright (c) 2025 Hakai Offensive Security.
// Licensed under GNU GPL version 3 or later (see LICENSE).
// Generated-output permission: see LICENSE-EXCEPTION.md.
//
// lyra_linker - linker shim invoked by rustc.
//
// rustc calls us with its standard linker command line. We identify any
// codegen-unit object files belonging to crates in LYRA_OBFUSCATE_CRATES
// and replace them with the pre-built obfuscated objects at
// $LYRA_OBF_DIR/<crate>.obf.o (or the paths given in LYRA_OBFUSCATED_OBJS).
// Then we invoke the real linker ($LYRA_REAL_LINKER).
//
// Environment variables:
//   LYRA_REAL_LINKER         absolute path to the real linker (e.g. link.exe)
//   LYRA_OBFUSCATE_CRATES    comma-separated list of crate names
//   LYRA_OBF_DIR             directory containing <crate>.obf.o artefacts
//   LYRA_OBFUSCATED_OBJS     optional pipe-separated "crate=objpath" list
//                            (legacy / single-crate mode)
//   LYRA_DEBUG               if set, print what we're doing

use std::collections::HashMap;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{exit, Command};

fn main() {
    let raw_args: Vec<String> = env::args().skip(1).collect();
    let debug = env::var("LYRA_DEBUG").is_ok();

    let real_linker = env::var("LYRA_REAL_LINKER").expect("lyra_linker: LYRA_REAL_LINKER not set");

    let crates: Vec<String> = env::var("LYRA_OBFUSCATE_CRATES")
        .map(|s| s.split(',').map(|s| s.trim().to_string()).collect())
        .unwrap_or_default();

    // Build the per-crate obj map
    let mut obj_map: HashMap<String, PathBuf> = HashMap::new();
    if let Ok(obf_dir) = env::var("LYRA_OBF_DIR") {
        let d = PathBuf::from(obf_dir);
        for c in &crates {
            let p = d.join(format!("{c}.obf.o"));
            if p.exists() {
                obj_map.insert(c.clone(), p);
            }
        }
    }
    if let Ok(s) = env::var("LYRA_OBFUSCATED_OBJS") {
        for entry in s.split('|') {
            let mut parts = entry.splitn(2, '=');
            if let (Some(k), Some(v)) = (parts.next(), parts.next()) {
                obj_map.insert(k.trim().to_string(), PathBuf::from(v.trim()));
            }
        }
    }

    // Always write a log (regardless of LYRA_DEBUG) so we can diagnose
    // whether the shim actually matched anything and what was substituted.
    let log_path = env::var("LYRA_OBF_DIR")
        .ok()
        .map(|d| PathBuf::from(d).join("lyra_linker.log"));
    let mut log = log_path
        .as_ref()
        .and_then(|p| OpenOptions::new().create(true).append(true).open(p).ok());

    macro_rules! logln {
        ($($arg:tt)*) => {{
            let msg = format!($($arg)*);
            if debug { eprintln!("[lyra-linker] {msg}"); }
            if let Some(ref mut f) = log { let _ = writeln!(f, "{msg}"); }
        }};
    }

    logln!("=== lyra_linker invoked ===");
    logln!("real_linker={real_linker}");
    logln!("crates={:?}", crates);
    logln!("obj_map={:?}", obj_map);

    // MSVC link.exe (and LLVM lld-link) accept `@responsefile` to read
    // command-line arguments from a file, and rustc uses this routinely on
    // Windows because its full linker command is usually > 32kB.
    // We need to expand these so we can see (and potentially substitute)
    // the .rcgu.o paths inside.
    let expanded = expand_response_files(&raw_args, &mut log, debug);

    logln!("expanded argv ({} tokens):", expanded.len());
    for (i, a) in expanded.iter().enumerate() {
        logln!("  [{i}] {a}");
    }

    let mut new_args: Vec<String> = Vec::with_capacity(expanded.len());
    let mut substituted: HashMap<String, bool> = HashMap::new();
    let mut total_replaced = 0usize;
    let mut total_dropped = 0usize;

    for arg in &expanded {
        if let Some(crate_name) = match_workspace_crate_object(arg, &crates) {
            if let Some(obf_path) = obj_map.get(&crate_name) {
                if !substituted.contains_key(&crate_name) {
                    new_args.push(obf_path.display().to_string());
                    substituted.insert(crate_name.clone(), true);
                    total_replaced += 1;
                    logln!(
                        "replace '{arg}' -> '{}' (crate={crate_name})",
                        obf_path.display()
                    );
                } else {
                    total_dropped += 1;
                    logln!("drop extra CGU '{arg}' (crate={crate_name})");
                }
                continue;
            } else {
                logln!(
                    "MATCHED '{arg}' as crate={crate_name} but no obj in map; leaving unchanged"
                );
            }
        }
        new_args.push(arg.clone());
    }

    logln!("summary: replaced {total_replaced} object(s), dropped {total_dropped} extra CGU(s)");

    eprintln!(
        "[lyra-linker] replaced {total_replaced} crate object(s), dropped {total_dropped} extra CGU(s)"
    );

    // The expanded command line can be tens of thousands of characters on
    // Windows, which exceeds CreateProcess's limit (~32k). Write a new
    // response file and pass `@file` to the real linker.
    let resp_file = env::var("LYRA_OBF_DIR")
        .map(|d| PathBuf::from(d).join("lyra_linker_args.rsp"))
        .unwrap_or_else(|_| std::env::temp_dir().join("lyra_linker_args.rsp"));
    if let Err(e) = write_response_file(&resp_file, &new_args) {
        eprintln!(
            "[lyra-linker] failed to write response file {}: {e}",
            resp_file.display()
        );
        exit(1);
    }
    logln!("wrote response file: {}", resp_file.display());

    let status = Command::new(&real_linker)
        .arg(format!("@{}", resp_file.display()))
        .status()
        .unwrap_or_else(|e| panic!("lyra_linker: exec '{real_linker}' failed: {e}"));

    logln!("real linker exit={:?}", status.code());
    exit(status.code().unwrap_or(1));
}

/// Recursively expand MSVC-style `@responsefile` arguments by reading each
/// response file and tokenising its contents. Non-`@` args are passed
/// through unchanged.
fn expand_response_files(
    args: &[String],
    log: &mut Option<std::fs::File>,
    debug: bool,
) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    for a in args {
        if let Some(path) = a.strip_prefix('@') {
            let mut diag = |msg: &str| {
                if debug {
                    eprintln!("[lyra-linker] {msg}");
                }
                if let Some(f) = log.as_mut() {
                    let _ = writeln!(f, "{msg}");
                }
            };
            diag(&format!("expanding response file '{path}'"));
            match fs::read(path) {
                Ok(bytes) => {
                    // rustc's response files on Windows may be written as
                    // UTF-16 LE with a BOM. Detect and decode.
                    let body = decode_response_bytes(&bytes);
                    diag(&format!(
                        "  read {} bytes, decoded to {} chars",
                        bytes.len(),
                        body.len()
                    ));
                    let tokens = tokenize_response_file(&body);
                    diag(&format!("  tokenised into {} tokens", tokens.len()));
                    let expanded = expand_response_files(&tokens, log, debug);
                    out.extend(expanded);
                }
                Err(e) => {
                    diag(&format!(
                        "  WARNING: cannot read response file '{path}': {e}"
                    ));
                    out.push(a.clone());
                }
            }
        } else {
            out.push(a.clone());
        }
    }
    out
}

/// Decode a linker response file's byte content. Many Windows tools
/// (including rustc) emit UTF-16 LE with a BOM; fall back to UTF-8
/// otherwise.
fn decode_response_bytes(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE {
        // UTF-16 LE with BOM
        let body = &bytes[2..];
        let u16s: Vec<u16> = body
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16_lossy(&u16s);
    }
    if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        // UTF-16 BE with BOM
        let body = &bytes[2..];
        let u16s: Vec<u16> = body
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16_lossy(&u16s);
    }
    if bytes.len() >= 3 && bytes[0] == 0xEF && bytes[1] == 0xBB && bytes[2] == 0xBF {
        // UTF-8 with BOM
        return String::from_utf8_lossy(&bytes[3..]).into_owned();
    }
    String::from_utf8_lossy(bytes).into_owned()
}

/// Tokenise an MSVC link.exe response-file body into individual arguments.
/// Supports double-quoted strings and `\"` escapes inside quotes; separators
/// are whitespace (space, tab, CR, LF).
fn tokenize_response_file(body: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    let mut chars = body.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '"' => {
                // toggle quote mode
                in_quote = !in_quote;
            }
            '\\' if in_quote => {
                // Backslash escaping is NOT universal in MSVC response files,
                // but rustc writes `\\` for path backslashes only when
                // emitting JSON-like strings. For .rsp files it normally
                // uses literal `\`. We treat `\<anything>` as literal `\<anything>`.
                cur.push('\\');
            }
            ch if ch.is_whitespace() && !in_quote => {
                if !cur.is_empty() {
                    tokens.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    tokens
}

/// Write a response file accepted by both MSVC `link.exe` and GCC/MinGW.
///
/// GCC response files treat `\` as an escape character, so Windows paths
/// like `C:\Users\...` arrive at `ld` as `C:Users...` with all backslashes
/// stripped. Both `link.exe` and GCC/MinGW accept forward slashes in paths
/// on Windows, so we normalise `\` → `/` unconditionally.
fn write_response_file(path: &Path, args: &[String]) -> std::io::Result<()> {
    let mut f = std::fs::File::create(path)?;
    for a in args {
        // Normalise path separators so GCC does not interpret `\` as escape.
        let a = a.replace('\\', "/");
        let needs_quote = a.chars().any(|c| c == ' ' || c == '\t');
        if needs_quote {
            // Escape embedded quotes by doubling them; sufficient for both
            // link.exe and GCC @file parsers.
            let escaped = a.replace('"', "\"\"");
            writeln!(f, "\"{escaped}\"")?;
        } else {
            writeln!(f, "{a}")?;
        }
    }
    Ok(())
}

/// Determine whether `arg` looks like a codegen-unit object that belongs
/// to one of the listed crates.
///
/// rustc emits multiple `.rcgu.o` files per bin compile:
///   - the main CGU(s) from the crate's own source, named
///     `<crate>.<crate>.<cgu_hash>-cgu.N.rcgu.o` for bin crates or
///     `<crate>-<extra>.<crate>.<cgu_hash>-cgu.N.rcgu.o` for lib crates
///   - the allocator shim, named `<crate>.<random>.rcgu.o` with NO
///     `-cgu.N` segment. This shim defines `__rust_alloc`/`__rg_alloc`
///     and friends; it MUST NOT be dropped, or linking fails with
///     unresolved symbols.
///
/// We therefore require the filename to contain `-cgu.` before we
/// treat it as substitutable.
fn match_workspace_crate_object(arg: &str, crates: &[String]) -> Option<String> {
    let path = Path::new(arg);
    let filename = path.file_name().and_then(|n| n.to_str())?;

    if !filename.ends_with(".rcgu.o") && !filename.ends_with(".rcgu.obj") {
        return None;
    }

    // Only main-CGU objects have "-cgu." in their name. The allocator shim
    // (which must be preserved) does NOT.
    if !filename.contains("-cgu.") {
        return None;
    }

    for c in crates {
        let normalized = c.replace('-', "_");
        if filename.starts_with(&format!("{normalized}-"))
            || filename.starts_with(&format!("{normalized}."))
        {
            return Some(c.clone());
        }
    }
    None
}
