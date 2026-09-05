// Copyright (c) 2025 Hakai Offensive Security. Licensed under GNU GPL version 3 or later (see LICENSE).
//
// A tiny cdylib used by the harness in `tools/dll_loader.rs` to verify
// that lyra's obfuscation passes produce a DLL that:
//
//   1. Loads successfully via LoadLibraryW.
//   2. Exports the functions below, resolvable via GetProcAddress.
//   3. Returns correct values when called across the DLL boundary.
//   4. Has its string literals encrypted on disk (verified by `strings`)
//      and correctly decrypted at runtime (observed via `greeting`).

/// Simple math export. Exercise basic control flow across an exported
/// function, with some branches to give `shuffle-blocks` and
/// `indirect-branch` something to do.
#[no_mangle]
pub extern "C" fn add(a: i32, b: i32) -> i32 {
    let r = a.wrapping_add(b);
    if r > 100 {
        r.wrapping_sub(100)
    } else if r < -100 {
        r.wrapping_add(100)
    } else {
        r
    }
}

/// Magic-number style export. A value unlikely to appear by chance in
/// `std::fmt` or panic machinery, used as a sanity ground truth by the
/// harness.
#[no_mangle]
pub extern "C" fn magic() -> u32 {
    0xDEADBEEF
}

/// Returns a pointer to a NUL-terminated UTF-8 string. The harness
/// copies bytes until the NUL and checks they match the plaintext.
/// If string-encryption ran and our ctor-based decryptor works inside
/// `DllMainCRTStartup`, this should return the plaintext on the other
/// side; the literal must NOT appear in the DLL's on-disk bytes.
#[no_mangle]
pub extern "C" fn greeting() -> *const u8 {
    // The trailing NUL is explicit so the C-side caller can use it as
    // a regular C string.
    static MSG: &[u8] =
        b"lyra-dll: secret message that must be decrypted at DLL_PROCESS_ATTACH time\0";
    MSG.as_ptr()
}

/// Report the length of the greeting as seen from Rust. If our global
/// ctor ran, the byte count should match the plaintext length (74 +
/// trailing NUL = 75, but strlen excludes NUL so Rust's len on the slice
/// is 75 including NUL). The harness compares against its own strlen.
#[no_mangle]
pub extern "C" fn greeting_len() -> usize {
    // Matches the literal in `greeting()` above, kept as a separate
    // constant so the compiler doesn't fold them into one shared global
    // that confuses the encryption pass's bookkeeping.
    static LEN: usize = 75;
    LEN
}
