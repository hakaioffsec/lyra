// Copyright (c) 2025 Hakai Offensive Security.
// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::BTreeMap;
use std::fmt::Write;

pub(super) fn enabled() -> bool {
    std::env::var_os("LYRA_COVERAGE").is_some_and(|value| value == "1")
}

#[derive(Default)]
pub(super) struct Counts {
    pub considered: usize,
    pub eligible: usize,
    pub transformed: usize,
    pub skipped: usize,
    pub skip_reasons: BTreeMap<&'static str, usize>,
}

impl Counts {
    pub fn skip(&mut self, reason: &'static str, count: usize) {
        if count != 0 {
            self.skipped += count;
            *self.skip_reasons.entry(reason).or_default() += count;
        }
    }

    pub fn merge(&mut self, other: &Self) {
        self.considered += other.considered;
        self.eligible += other.eligible;
        self.transformed += other.transformed;
        for (&reason, &count) in &other.skip_reasons {
            self.skip(reason, count);
        }
    }

    pub fn json(&self) -> String {
        let mut json = format!(
            "{{\"considered\":{},\"eligible\":{},\"transformed\":{},\"skipped\":{},\"skip_reasons\":{{",
            self.considered, self.eligible, self.transformed, self.skipped
        );
        for (index, (reason, count)) in self.skip_reasons.iter().enumerate() {
            if index != 0 {
                json.push(',');
            }
            write!(json, "{}:{}", quote(reason), count).unwrap();
        }
        json.push_str("}}");
        json
    }

    // Extra fields are internally generated JSON, never input text.
    pub fn emit(&self, pass: &str, unit: &str, module: &str, extra: &str) {
        let counts = self.json();
        println!(
            "LYRA_COVERAGE {{\"schema_version\":1,\"pass\":{},\"unit\":{},\"module\":{},{}{}",
            quote(pass),
            quote(unit),
            quote(module),
            extra,
            &counts[1..]
        );
    }
}

fn quote(text: &str) -> String {
    let mut json = String::with_capacity(text.len() + 2);
    json.push('"');
    for ch in text.chars() {
        match ch {
            '"' => json.push_str("\\\""),
            '\\' => json.push_str("\\\\"),
            '\u{00}'..='\u{1f}' => write!(json, "\\u{:04x}", ch as u32).unwrap(),
            _ => json.push(ch),
        }
    }
    json.push('"');
    json
}
