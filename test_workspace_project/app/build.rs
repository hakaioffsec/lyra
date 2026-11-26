// Copyright (c) 2026 Hakai Offensive Security.
// SPDX-License-Identifier: GPL-3.0-or-later

fn main() {
    assert_eq!(hyphen_lib::adjusted(11), 40);
    assert_eq!(hyphen_lib::message(), "LYRA_HYPHEN_LIBRARY_CANARY");
    println!("cargo:rerun-if-changed=build.rs");
}
