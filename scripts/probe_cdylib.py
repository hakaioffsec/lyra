#!/usr/bin/env python3
# Copyright (c) 2026 Hakai Offensive Security.
# SPDX-License-Identifier: GPL-3.0-or-later
"""Load a native sample library in a disposable process and check its C ABI."""

import argparse
import ctypes
import math
from pathlib import Path
import struct


GREETING = b"lyra-dll: secret message that must be decrypted at DLL_PROCESS_ATTACH time\0"


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def check_float_arithmetic(library):
    infinity, nan = float("inf"), float("nan")
    formats = [
        ("f32", ctypes.c_float, "<f", float.fromhex("0x1p-149"),
         float.fromhex("0x1p-126"), float.fromhex("0x1p-24"), ctypes.c_float(3e38).value),
        ("f64", ctypes.c_double, "<d", float.fromhex("0x1p-1074"),
         float.fromhex("0x1p-1022"), float.fromhex("0x1p-53"), 1e308),
    ]
    for suffix, ctype, encoding, tiny, normal, half_ulp, large in formats:
        # Explicit round-to-nearest-even results, not a tolerance or a second
        # evaluation of the operation under test. Powers of two are exact.
        cases = {
            "add": [
                (large, -large, 0.0), (large, large, infinity),
                (-0.0, -0.0, -0.0), (0.0, -0.0, 0.0), (-0.0, 0.0, 0.0),
                (1.0, half_ulp, 1.0), (1.0 + 2 * half_ulp, -1.0, 2 * half_ulp),
                (tiny, tiny, 2 * tiny), (normal, -tiny, normal - tiny),
                (infinity, 1.0, infinity), (1.0, infinity, infinity),
                (infinity, -infinity, nan), (infinity, infinity, infinity),
                (nan, 1.0, nan), (1.0, nan, nan),
            ],
            "sub": [
                (large, large, 0.0), (large, -large, infinity),
                (-0.0, 0.0, -0.0), (0.0, -0.0, 0.0), (-0.0, -0.0, 0.0),
                (1.0, -half_ulp, 1.0), (1.0 + 2 * half_ulp, 1.0, 2 * half_ulp),
                (tiny, 2 * tiny, -tiny), (normal, tiny, normal - tiny),
                (infinity, 1.0, infinity), (1.0, infinity, -infinity),
                (infinity, infinity, nan), (-infinity, -infinity, nan),
                (nan, 1.0, nan), (1.0, nan, nan),
            ],
            "mul": [
                (large, 1.0, large), (1.0, large, large), (large, 2.0, infinity),
                (tiny, 1.0, tiny), (1.0, tiny, tiny), (tiny, 2.0, 2 * tiny),
                (normal, 0.5, normal / 2), (tiny, 0.5, 0.0), (-tiny, 0.5, -0.0),
                (0.0, -1.0, -0.0), (-0.0, -1.0, 0.0),
                (infinity, 0.0, nan), (0.0, infinity, nan),
                (infinity, -1.0, -infinity), (-infinity, -1.0, infinity),
                (nan, 1.0, nan), (1.0, nan, nan),
            ],
        }
        for operation, operands in cases.items():
            name = f"float_{operation}_{suffix}"
            function = getattr(library, name)
            function.argtypes = [ctype, ctype]
            function.restype = ctype
            for a, b, expected in operands:
                actual = function(a, b)
                # NaN payload/sign are not part of this oracle. All other
                # results, including signed zero and subnormals, must match bits.
                matches = math.isnan(actual) if math.isnan(expected) else (
                    struct.pack(encoding, actual) == struct.pack(encoding, expected))
                require(matches, f"{name}({a.hex()}, {b.hex()}): "
                        f"expected {expected.hex()}, got {actual.hex()}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("library", type=Path)
    args = parser.parse_args()
    library = ctypes.CDLL(str(args.library.resolve()))
    library.greeting.argtypes = []
    library.greeting.restype = ctypes.POINTER(ctypes.c_ubyte)
    library.greeting_len.argtypes = []
    library.greeting_len.restype = ctypes.c_size_t

    def check_greeting():
        pointer = library.greeting()
        require(bool(pointer), "greeting returned a null pointer")
        require(library.greeting_len() == 75, "greeting_len must include the NUL (75)")
        require(ctypes.string_at(pointer, 75) == GREETING, "greeting bytes were not decrypted")
        require(ctypes.string_at(pointer) == GREETING[:-1], "greeting has an incorrect terminator")

    # Check loader-time decryption before invoking any arithmetic exports.
    check_greeting()
    library.add.argtypes = [ctypes.c_int32, ctypes.c_int32]
    library.add.restype = ctypes.c_int32
    library.magic.argtypes = []
    library.magic.restype = ctypes.c_uint32
    operands = [(0, 0), (3, 4), (100, 0), (100, 1), (-100, 0), (-100, -1),
                (200, 50), (-200, -50), (2**31 - 1, 1), (-2**31, -1),
                (2**31 - 1, 2**31 - 1), (-2**31, -2**31)]
    for _ in range(3):
        require(library.magic() == 0xDEADBEEF, "magic returned an incorrect value")
        for a, b in operands:
            expected = (a + b + 2**31) % 2**32 - 2**31
            expected += -100 if expected > 100 else 100 if expected < -100 else 0
            actual = library.add(a, b)
            require(actual == expected, f"add({a}, {b}): expected {expected}, got {actual}")
        check_greeting()
    check_float_arithmetic(library)
    print("cdylib ABI and loader-time greeting passed")


if __name__ == "__main__":
    main()
