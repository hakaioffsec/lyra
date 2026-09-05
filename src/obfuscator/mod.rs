// Copyright (c) 2025 Hakai Offensive Security.
//
// This program is free software: you can redistribute it and/or modify it
// under the terms of the GNU General Public License as published by the
// Free Software Foundation, either version 3 or (at your option) any later
// version. See LICENSE and the generated-output permission in LICENSE-EXCEPTION.md.
//
// Obfuscation passes.
//
// Shipped passes:
//
//   * `string_encryption` — per-byte random XOR of constant byte arrays
//     with a global ctor decrypting them in place before `main`. Tested
//     end-to-end on quasar (9501 strings across quasar bin + quasar lib
//     + protean, runtime behaviour preserved).
//   * `shuffle_blocks` — permutes basic-block textual order within each
//     eligible function. Defeats byte-sequence signatures and BinDiff
//     block-layout matching per build. Does not change CFG semantics
//     since LLVM IR uses explicit terminators.
//
// The earlier `bogus_control_flow`, `control_flow_flattening`, and
// `instruction_substitution` passes were deleted because they shipped
// known-broken behaviour (ad-hoc name blocklists in CFF, shared-global
// opaque predicates in BCF, unconfigurable templates and miscounted
// stats in instruction substitution) and had not been regression-tested
// against the current RUSTC_WRAPPER pipeline or LLVM 22. Their
// replacements — ranked in ROADMAP.md — are better-designed rewrites
// rather than incremental fixes.

pub mod indirect_branch;
pub mod mba;
pub mod seed;
pub mod shuffle_blocks;
pub mod string_encryption;
pub mod util;

use anyhow::Result;
use inkwell::context::Context;
use inkwell::module::Module;

pub struct Obfuscator<'ctx> {
    pub context: &'ctx Context,
    pub module: Module<'ctx>,
}

impl<'ctx> Obfuscator<'ctx> {
    pub fn apply_string_encryption(&mut self) -> Result<()> {
        string_encryption::apply(self.context, &mut self.module)?;
        Ok(())
    }

    pub fn apply_shuffle_blocks(&mut self) -> Result<()> {
        shuffle_blocks::apply(self.context, &mut self.module)?;
        Ok(())
    }

    pub fn apply_indirect_branch(&mut self) -> Result<()> {
        indirect_branch::apply(self.context, &mut self.module)?;
        Ok(())
    }

    pub fn apply_mba(&mut self) -> Result<()> {
        mba::apply(self.context, &mut self.module)?;
        Ok(())
    }
}
