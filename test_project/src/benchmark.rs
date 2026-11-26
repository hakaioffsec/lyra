// Copyright (c) 2025 Hakai Offensive Security.
// SPDX-License-Identifier: GPL-3.0-or-later

//! Runtime-supplied, deterministic workloads; the independent semantic oracle
//! and public input descriptions live in scripts/benchmark_workloads.py.
//! Timed scope: one workload call, including loop control, input preparation,
//! checksum consumption, and black-box barriers. Argument parsing, JSON output,
//! and independent correctness verification are excluded. FFT timing includes
//! stack-array initialization, resetting inputs, the FFT, and output hashing.
//! Zero-valued floats are canonicalized to positive zero before bit hashing;
//! all nonzero float bits are preserved exactly (there is no tolerance check).

use std::hint::black_box;
use std::time::Instant;

const INITIAL: u64 = 0x6a09_e667_f3bc_c909;
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
const STARTUP_TAG: u64 = 0x5354_4152_5455_5031;
const FFT_LEN: usize = 1024;

// Fixed, public ASCII input, including uppercase, digits, punctuation, and
// whitespace. This is deliberately a global string for encrypted-global use.
const CORPUS: &str = concat!(
    "Lyra public benchmark corpus: deterministic inputs, not secret data.\n",
    "The quick brown fox jumps over 13 lazy dogs; PACK MY BOX WITH 5 JUGS.\n",
    "0123456789 abcdefghijklmnopqrstuvwxyz ABCDEFGHIJKLMNOPQRSTUVWXYZ\n",
    "Memory scans visit every byte; branches classify letters, digits, and spaces.\n",
    "Tabs\tand newlines\njoin punctuation: !?.,;:-_+=/()[]{}<>@#$%^&*|~\n",
    "Linux baseline measurements retain exact checksums and inspection symbols.\n",
);

/// Scalar ABI and runtime arguments expose integer expressions to the existing
/// MBA pass without changing eligibility, pass order, or pass identities.
#[no_mangle]
#[inline(never)]
pub extern "C" fn bench_integer_step(state: u64, input: u64) -> u64 {
    let mixed = state.wrapping_add(input) ^ input.rotate_left(17);
    let next = if mixed & 1 == 0 {
        mixed.wrapping_mul(0x9e37_79b1_85eb_ca87) ^ state.rotate_left(23)
    } else {
        mixed.wrapping_sub(0xd1b5_4a32_d192_ed03) ^ state.rotate_right(11)
    };
    next.wrapping_add(input ^ 0xa076_1d64_78bd_642f)
}

#[inline(never)]
extern "C" fn integer(iterations: u64, seed: u64) -> u64 {
    let mut state = seed ^ INITIAL;
    for round in 0..iterations {
        state = bench_integer_step(black_box(state), seed.wrapping_add(round));
    }
    state
}

#[no_mangle]
#[inline(never)]
pub extern "C" fn bench_text(iterations: u64, seed: u64) -> u64 {
    let corpus = black_box(CORPUS.as_bytes());
    let mut state = FNV_OFFSET ^ seed;
    for round in 0..iterations {
        state ^= seed.wrapping_add(round);
        for &byte in corpus {
            let token = if byte.is_ascii_uppercase() {
                (byte + 32) as u64
            } else if byte.is_ascii_digit() {
                (byte - b'0') as u64 * 17 + 256
            } else if byte.is_ascii_whitespace() {
                32
            } else {
                byte as u64
            };
            state = (state ^ token).wrapping_mul(FNV_PRIME);
        }
        state = black_box(state);
    }
    state
}

#[inline]
fn float_word(value: f64) -> u64 {
    // FFT operations on zero can produce either sign. Both mean exact zero;
    // make this explicit rather than depending on the platform's zero sign.
    if value == 0.0 {
        0
    } else {
        value.to_bits()
    }
}

#[no_mangle]
#[inline(never)]
pub extern "C" fn bench_fft(iterations: u64, seed: u64) -> u64 {
    let mut re = [0.0; FFT_LEN];
    let mut im = [0.0; FFT_LEN];
    let mut state = FNV_OFFSET ^ seed;
    for round in 0..iterations {
        let input = seed.wrapping_add(round);
        let constant = ((input & 15) + 1) as f64;
        let impulse = (((input >> 4) & 31) + 1) as f64;
        re.fill(constant);
        im.fill(0.0);
        re[0] += impulse;
        super::fft(black_box(&mut re), black_box(&mut im));
        // The mathematical DFT is [1024*constant + impulse, impulse, ...],
        // with imaginary part zero. Only k=0 carries a nonzero butterfly's
        // second operand, so nontrivial twiddles multiply exact zeros. These
        // small positive integers are exactly representable in binary64.
        for index in 0..FFT_LEN {
            state = (state ^ float_word(re[index])).wrapping_mul(FNV_PRIME);
            state = (state ^ float_word(im[index])).wrapping_mul(FNV_PRIME);
        }
        state = black_box(state);
    }
    state
}

fn invalid(message: &str) -> ! {
    eprintln!("benchmark error: {message}\nusage: --bench <startup|integer|text|fft> <iterations:u64> <seed:u64>");
    std::process::exit(2)
}

fn decimal(value: &std::ffi::OsStr) -> u64 {
    let text = value
        .to_str()
        .unwrap_or_else(|| invalid("expected an unsigned decimal integer"));
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        invalid("expected an unsigned decimal integer");
    }
    text.parse()
        .unwrap_or_else(|_| invalid("integer exceeds u64 range"))
}

/// Return false without output for the existing non-benchmark invocation path.
/// A misplaced --bench flag is still a benchmark request and is rejected.
pub fn run_cli() -> bool {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if !arguments.iter().any(|argument| argument == "--bench") {
        return false;
    }
    if arguments.len() != 4 || arguments[0] != "--bench" {
        invalid("expected exactly four arguments with --bench first");
    }
    let workload = arguments[1]
        .to_str()
        .unwrap_or_else(|| invalid("invalid workload name"));
    let iterations = decimal(&arguments[2]);
    let seed = decimal(&arguments[3]);
    if workload == "startup" {
        if iterations != 0 {
            invalid("startup requires zero iterations");
        }
        let checksum = seed.rotate_left(17) ^ STARTUP_TAG;
        println!("{{\"workload\":\"startup\",\"iterations\":0,\"seed\":{seed},\"checksum\":\"{checksum:016x}\"}}");
        return true;
    }
    let run: extern "C" fn(u64, u64) -> u64 = match workload {
        "text" => bench_text,
        "fft" => bench_fft,
        "integer" => integer,
        _ => invalid("unknown workload"),
    };
    if iterations == 0 {
        invalid("non-startup workloads require positive iterations");
    }
    let started = Instant::now();
    let checksum = black_box(run(black_box(iterations), black_box(seed)));
    let work_ns = started.elapsed().as_nanos();
    println!("{{\"workload\":\"{workload}\",\"iterations\":{iterations},\"seed\":{seed},\"checksum\":\"{checksum:016x}\",\"work_ns\":{work_ns}}}");
    true
}
