// Copyright (c) 2025 Hakai Offensive Security.
//
// This program is free software: you can redistribute it and/or modify it
// under the terms of the GNU General Public License as published by the
// Free Software Foundation, version 3. See the LICENSE file for details.
//
// Per-build seed management.
//
// Every pass that uses randomness goes through `PassRng::for_pass(...)`.
// Behaviour:
//
//   * If the `LYRA_SEED` env var is set to a valid u64, each pass derives
//     its RNG seed deterministically from `(LYRA_SEED, pass_name,
//     module_name, module_source)` via FNV-1a 64. Re-running the build
//     with the same seed on the same inputs reproduces the same binary.
//     Useful for CI diffs and bisecting pass-introduced regressions.
//
//   * If `LYRA_SEED` is unset, each pass pulls entropy from the OS via
//     `rand::rng()`. Two consecutive builds produce different binaries
//     even from identical source, which is the signature-breaking mode
//     we want by default.
//
// The CLI flag `--seed <u64>` sets `LYRA_SEED` on the cargo invocation;
// the wrapper inherits it, and `lyra __obfuscate-ir` subprocesses read
// it directly via this helper.

use rand::rngs::StdRng;
use rand::SeedableRng;

/// Returns a seeded [`StdRng`] appropriate for the given pass on the
/// given module.
///
/// `pass_name` is a short identifier for the pass (e.g. `"shuffle_blocks"`).
/// `module_name` / `module_source` should come from the module being
/// transformed so different modules under the same seed get independent
/// streams.
pub fn for_pass(pass_name: &str, module_name: &str, module_source: &str) -> StdRng {
    if let Ok(raw) = std::env::var("LYRA_SEED") {
        if let Ok(master) = raw.parse::<u64>() {
            let input = format!("{}::{}::{}::{}", master, pass_name, module_name, module_source);
            return StdRng::seed_from_u64(fnv1a64(&input));
        }
    }
    // Fall through to non-deterministic entropy.
    let mut os = rand::rng();
    StdRng::from_rng(&mut os)
}

fn fnv1a64(input: &str) -> u64 {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;
    let mut h = OFFSET;
    for &b in input.as_bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(PRIME);
    }
    h
}
