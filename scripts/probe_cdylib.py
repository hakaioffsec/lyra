#!/usr/bin/env python3
# Copyright (c) 2026 Hakai Offensive Security.
# SPDX-License-Identifier: GPL-3.0-or-later
"""Load a native sample library in a disposable process and check its C ABI."""

import argparse
import ctypes
from pathlib import Path


GREETING = b"lyra-dll: secret message that must be decrypted at DLL_PROCESS_ATTACH time\0"


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


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
    print("cdylib ABI and loader-time greeting passed")


if __name__ == "__main__":
    main()
