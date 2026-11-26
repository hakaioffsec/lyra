// Copyright (c) 2026 Hakai Offensive Security.
// SPDX-License-Identifier: GPL-3.0-or-later

use std::env;
use std::io::{self, BufWriter, Write};
use std::process::{Command, Stdio};

/// Invoke the publisher's audit hook without imposing Python on ordinary Lyra
/// builds. A configured but incomplete/failing hook is never silently skipped.
pub fn run<'a>(action: &str, arguments: impl IntoIterator<Item = &'a str>) -> io::Result<()> {
    let Some(script) = env::var_os("LYRA_AUDIT_SCRIPT") else {
        return Ok(());
    };
    let python = env::var_os("LYRA_AUDIT_PYTHON")
        .ok_or_else(|| io::Error::other("LYRA_AUDIT_SCRIPT requires LYRA_AUDIT_PYTHON"))?;
    let mut child = Command::new(python)
        .arg(script)
        .arg(action)
        .stdin(Stdio::piped())
        .spawn()?;
    let input = child.stdin.take().expect("audit stdin was piped");
    let mut writer = BufWriter::new(input);
    let sent = (|| {
        for argument in arguments {
            if argument.as_bytes().contains(&0) {
                return Err(io::Error::other("NUL in audit argument"));
            }
            writer.write_all(argument.as_bytes())?;
            writer.write_all(&[0])?;
        }
        writer.flush()
    })();
    drop(writer);
    let status = child.wait()?;
    sent?;
    if !status.success() {
        return Err(io::Error::other(format!("{action} audit failed: {status}")));
    }
    Ok(())
}
