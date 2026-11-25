# Copyright (c) 2025 Hakai Offensive Security.
# SPDX-License-Identifier: GPL-3.0-or-later

"""Public inputs and independent semantic checksums for the native benchmarks.

No native execution or baseline output is used here. Integer arithmetic is
explicitly modulo 2**64. The FFT oracle uses the exact mathematical DFT of a
constant plus an origin impulse, not an FFT implementation. Floating-point zero
is canonicalized to positive zero before hashing, including negative zero; every
nonzero binary64 bit is significant. Linux execution is exercised by the runner;
these definitions alone are not evidence of cross-platform native correctness.

Native work_ns covers one workload call (loop control, input preparation,
checksum consumption, and black-box barriers). It excludes CLI parsing, JSON
printing, and this oracle. Startup has no work_ns; process startup is timed by
the runner externally, not confused with an internal workload duration.
"""

import struct

_MASK = (1 << 64) - 1
_INITIAL = 0x6A09E667F3BCC909
_FNV_OFFSET = 0xCBF29CE484222325
_FNV_PRIME = 0x100000001B3
_STARTUP_TAG = 0x5354415254555031
_SEED = 0x123456789ABCDEF0
_FFT_LEN = 1024

CORPUS = (
    b"Lyra public benchmark corpus: deterministic inputs, not secret data.\n"
    b"The quick brown fox jumps over 13 lazy dogs; PACK MY BOX WITH 5 JUGS.\n"
    b"0123456789 abcdefghijklmnopqrstuvwxyz ABCDEFGHIJKLMNOPQRSTUVWXYZ\n"
    b"Memory scans visit every byte; branches classify letters, digits, and spaces.\n"
    b"Tabs\tand newlines\njoin punctuation: !?.,;:-_+=/()[]{}<>@#$%^&*|~\n"
    b"Linux baseline measurements retain exact checksums and inspection symbols.\n"
)

WORKLOADS = [
    {
        "name": "startup",
        "iterations": 0,
        "seed": _SEED,
        "description": "Parse CLI and emit rotate_left(seed, 17) XOR 0x5354415254555031.",
        "timed_scope": "External process wall time only; no internal work_ns field.",
    },
    {
        "name": "integer",
        "iterations": 2_000_000,
        "seed": _SEED,
        "description": (
            "Seeded dependent wrapping-u64 arithmetic, rotations, and a parity branch; "
            "each round calls the scalar C-ABI bench_integer_step(state, seed + round)."
        ),
        "timed_scope": (
            "One integer workload call: state initialization, loop control, scalar "
            "steps, and black-box barriers; excludes parsing, printing, and oracle."
        ),
    },
    {
        "name": "text",
        "iterations": 4_096,
        "seed": _SEED,
        "description": (
            "Repeated scans of the public ASCII CORPUS: lowercase uppercase letters, "
            "map digits to 256 + digit*17, normalize ASCII whitespace to space, "
            "and accumulate an FNV-style wrapping checksum."
        ),
        "timed_scope": (
            "One bench_text call: checksum initialization, full corpus scans, byte "
            "classification, hashing, loop control, and black-box barriers; excludes "
            "parsing, printing, and oracle."
        ),
    },
    {
        "name": "fft",
        "iterations": 128,
        "seed": _SEED,
        "description": (
            "Existing length-1024 FFT; round input is constant c plus impulse d at "
            "index zero, imaginary input zero. For s=(seed+round) mod 2**64, "
            "c=(s&15)+1 and d=((s>>4)&31)+1. Exact output: DC=1024*c+d, "
            "all other real bins=d, all imaginary bins=zero (zero sign normalized)."
        ),
        "timed_scope": (
            "One bench_fft call: stack-array initialization, per-round array reset, "
            "existing FFT, exact-bit output hashing, loop control, and black-box "
            "barriers; excludes parsing, printing, and mathematical oracle."
        ),
    },
]


def _rotate_left(value: int, shift: int) -> int:
    return ((value << shift) | (value >> (64 - shift))) & _MASK


def _integer(iterations: int, seed: int) -> int:
    state = seed ^ _INITIAL
    for round_index in range(iterations):
        operand = (seed + round_index) & _MASK
        mixed = ((state + operand) & _MASK) ^ _rotate_left(operand, 17)
        if mixed & 1:
            next_state = ((mixed - 0xD1B54A32D192ED03) & _MASK) ^ _rotate_left(state, 53)
        else:
            next_state = ((mixed * 0x9E3779B185EBCA87) & _MASK) ^ _rotate_left(state, 23)
        state = (next_state + (operand ^ 0xA0761D6478BD642F)) & _MASK
    return state


def _text(iterations: int, seed: int) -> int:
    # Derive tokens once: the oracle need not repeat the native memory workload.
    tokens = []
    for byte in CORPUS:
        if 65 <= byte <= 90:
            token = byte + 32
        elif 48 <= byte <= 57:
            token = (byte - 48) * 17 + 256
        elif byte in (9, 10, 12, 13, 32):  # Rust u8::is_ascii_whitespace
            token = 32
        else:
            token = byte
        tokens.append(token)
    state = _FNV_OFFSET ^ seed
    for round_index in range(iterations):
        state ^= (seed + round_index) & _MASK
        for token in tokens:
            state = ((state ^ token) * _FNV_PRIME) & _MASK
    return state


def _float_word(value: float) -> int:
    if value == 0.0:
        return 0
    return struct.unpack("<Q", struct.pack("<d", value))[0]


def _fft(iterations: int, seed: int) -> int:
    state = _FNV_OFFSET ^ seed
    for round_index in range(iterations):
        value = (seed + round_index) & _MASK
        constant = (value & 15) + 1
        impulse = ((value >> 4) & 31) + 1
        dc_word = _float_word(float(_FFT_LEN * constant + impulse))
        other_word = _float_word(float(impulse))
        # Mathematical DFT values, not a second numerical FFT implementation.
        # Hash each real bin followed by an exactly-zero imaginary bin.
        state = ((state ^ dc_word) * _FNV_PRIME * _FNV_PRIME) & _MASK
        for _ in range(1, _FFT_LEN):
            state = ((state ^ other_word) * _FNV_PRIME * _FNV_PRIME) & _MASK
    return state


def expected_checksum(name: str, iterations: int, seed: int) -> str:
    """Return 16 lowercase hex digits; reject inputs the native CLI rejects."""
    if type(iterations) is not int or not 0 <= iterations <= _MASK:
        raise ValueError("iterations must be an unsigned 64-bit integer")
    if type(seed) is not int or not 0 <= seed <= _MASK:
        raise ValueError("seed must be an unsigned 64-bit integer")
    if name == "startup":
        if iterations != 0:
            raise ValueError("startup requires zero iterations")
        checksum = _rotate_left(seed, 17) ^ _STARTUP_TAG
    else:
        functions = {"integer": _integer, "text": _text, "fft": _fft}
        if name not in functions:
            raise ValueError(f"unknown workload: {name}")
        if iterations == 0:
            raise ValueError("non-startup workloads require positive iterations")
        checksum = functions[name](iterations, seed)
    return f"{checksum:016x}"
