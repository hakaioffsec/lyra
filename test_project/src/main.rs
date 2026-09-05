// Copyright (c) 2025 Hakai Offensive Security.
//
// This program is free software: you can redistribute it and/or modify it
// under the terms of the GNU General Public License as published by the
// Free Software Foundation, either version 3 or (at your option) any later
// version. See the LICENSE file for details.

use std::f64::consts::PI;
use std::hint::black_box;

#[inline(never)]
fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    if n <= 1 {
        return;
    }

    // bit-reversal permutation
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }

    // Cooley-Tukey butterfly
    let mut len = 2;
    while len <= n {
        let half = len / 2;
        let angle = -2.0 * PI / len as f64;
        let wn_re = angle.cos();
        let wn_im = angle.sin();

        for start in (0..n).step_by(len) {
            let mut w_re = 1.0;
            let mut w_im = 0.0;
            for k in 0..half {
                let u_re = re[start + k];
                let u_im = im[start + k];
                let v_re = re[start + k + half] * w_re - im[start + k + half] * w_im;
                let v_im = re[start + k + half] * w_im + im[start + k + half] * w_re;
                re[start + k] = u_re + v_re;
                im[start + k] = u_im + v_im;
                re[start + k + half] = u_re - v_re;
                im[start + k + half] = u_im - v_im;
                let tmp = w_re * wn_re - w_im * wn_im;
                w_im = w_re * wn_im + w_im * wn_re;
                w_re = tmp;
            }
        }
        len <<= 1;
    }
}

#[inline(never)]
fn secret_check(val: i32) -> bool {
    if val > 100 {
        println!("Value is large!");
        if val % 2 == 0 {
            println!("And it's even.");
            true
        } else {
            println!("But it's odd.");
            false
        }
    } else {
        println!("Value is small.");
        false
    }
}

fn main() {
    let secret_string = "This is a secret message that should be encrypted.";
    println!("{}", secret_string);

    let a = 10;
    let b = 20;
    let c = a + b;
    let d = c - 5;
    let e = c ^ d;

    println!("Calculation result: {}", e);

    if secret_check(black_box(150)) {
        println!("Check passed!");
    } else {
        println!("Check failed!");
    }

    // FFT of a simple signal: DC + cosine at bin 1
    let mut re = black_box([1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0]);
    re[0] += 1.0; // extra DC
    let mut im = [0.0f64; 8];
    fft(&mut re, &mut im);
    println!("FFT bin[0] = ({:.1}, {:.1})", re[0], im[0]);
    println!("FFT bin[1] = ({:.1}, {:.1})", re[1], im[1]);
}
