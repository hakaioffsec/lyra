// Copyright (c) 2026 Hakai Offensive Security.
// SPDX-License-Identifier: GPL-3.0-or-later

#[inline(never)]
pub fn message() -> &'static str {
    "LYRA_HYPHEN_LIBRARY_CANARY"
}

#[inline(never)]
pub fn adjusted(value: u32) -> u32 {
    value.wrapping_mul(3).wrapping_add(7)
}
