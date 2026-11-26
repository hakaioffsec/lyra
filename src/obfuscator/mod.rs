// Copyright (c) 2025 Hakai Offensive Security.
//
// This program is free software: you can redistribute it and/or modify it
// under the terms of the GNU General Public License as published by the
// Free Software Foundation, either version 3 or (at your option) any later
// version. See LICENSE and the generated-output permission in LICENSE-EXCEPTION.md.
//
// Obfuscation passes.
//
// Passes are opt-in; see ROADMAP.md for supported subsets and measured limits.
// String encryption changes selected globals, block shuffle changes layout,
// indirect branches/calls change selected control transfers, and MBA rewrites
// only proven-defined scalar integer expressions.
//
// The old BCF, CFF and instruction-substitution implementations were removed for
// correctness reasons; they are not dependencies of the current pipeline.

pub mod block_outline;
mod coverage;
pub mod indirect_branch;
pub mod indirect_call;
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

    pub fn apply_block_outline(&mut self) -> Result<()> {
        block_outline::apply(self.context, &mut self.module)?;
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

    pub fn apply_indirect_call(&mut self) -> Result<()> {
        indirect_call::apply(self.context, &mut self.module)?;
        Ok(())
    }

    pub fn apply_mba(&mut self) -> Result<()> {
        mba::apply(self.context, &mut self.module)?;
        Ok(())
    }
}
