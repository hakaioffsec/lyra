// Copyright (c) 2026 Hakai Offensive Security.
// SPDX-License-Identifier: GPL-3.0-or-later

fn main() {
    println!("LYRA_HYPHEN_BINARY_CANARY");
    println!("{}", hyphen_lib::message());
    println!("{}", hyphen_lib::adjusted(std::hint::black_box(11)));
}
