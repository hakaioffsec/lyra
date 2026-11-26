// Copyright (c) 2025 Hakai Offensive Security.
//
// This program is free software: you can redistribute it and/or modify it
// under the terms of the GNU General Public License as published by the
// Free Software Foundation, either version 3 or (at your option) any later
// version. See LICENSE and the generated-output permission in LICENSE-EXCEPTION.md.
//
// Shuffle Blocks pass (A2).
//
// What it does
// ------------
// For each eligible function, shuffles the *textual* order of its basic
// blocks while preserving control-flow semantics. The entry block always
// stays first (LLVM requires it); every other block is permuted by a
// seeded RNG.
//
// Why this works
// --------------
// LLVM IR always has explicit terminators (unconditional `br`, `br i1`,
// `switch`, `indirectbr`, `ret`, `unreachable`, `invoke`). There is no
// implicit fallthrough between textually adjacent blocks like there is
// in machine code. PHI-node operands are block-identity-based, not
// position-based, so reordering doesn't touch them.
//
// Final-code effects depend on LLVM's backend layout decisions. A textual
// permutation is not a guarantee of different bytes or failed function matching;
// measure the linked artifacts rather than inferring protection from moved blocks.

use anyhow::Result;
use inkwell::basic_block::BasicBlock;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::values::FunctionValue;
use rand::seq::SliceRandom;
use rand::Rng;

use crate::obfuscator::coverage::{self, Counts};
use crate::obfuscator::seed;
use crate::obfuscator::util::should_skip_function;

/// Functions with fewer than this many basic blocks are left alone.
/// Shuffle only has meaning when there are at least two non-entry blocks
/// to permute.
const MIN_BLOCKS: usize = 3;

pub fn apply<'ctx>(_context: &'ctx Context, module: &mut Module<'ctx>) -> Result<()> {
    println!("[*] Applying Shuffle Blocks...");

    let mut rng = seed::for_pass(
        "shuffle_blocks",
        module.get_name().to_str().unwrap_or("module"),
        module.get_source_file_name().to_str().unwrap_or(""),
    );
    let mut total_functions = 0usize;
    let mut total_blocks_moved = 0usize;
    let mut counts = coverage::enabled().then(Counts::default);

    let functions: Vec<FunctionValue<'ctx>> = module.get_functions().collect();
    for function in functions {
        if let Some(counts) = counts.as_mut() {
            if function.count_basic_blocks() != 0 {
                counts.considered += 1;
            }
        }
        if should_skip_function(&function) {
            if let Some(counts) = counts.as_mut() {
                if function.count_basic_blocks() != 0 {
                    counts.skip("function_exclusion", 1);
                }
            }
            continue;
        }

        let moved = shuffle_function_blocks(&function, &mut rng);
        if let Some(counts) = counts.as_mut() {
            if function.count_basic_blocks() < MIN_BLOCKS as u32 {
                counts.skip("fewer_than_three_blocks", 1);
            } else {
                counts.eligible += 1;
                if moved == 0 {
                    counts.skip("identity_permutation", 1);
                } else {
                    counts.transformed += 1;
                }
            }
        }
        if moved > 0 {
            total_functions += 1;
            total_blocks_moved += moved;
        }
    }

    println!(
        "        ✓ Shuffled {} block(s) across {} function(s)",
        total_blocks_moved, total_functions
    );
    if let Some(counts) = counts {
        counts.emit(
            "shuffle-blocks",
            "functions_with_bodies",
            module.get_name().to_str().unwrap_or("module"),
            "",
        );
    }
    Ok(())
}

/// Permute the non-entry basic blocks of `function`. Returns the number
/// of blocks that ended up in a different textual position.
fn shuffle_function_blocks<'ctx>(function: &FunctionValue<'ctx>, rng: &mut impl Rng) -> usize {
    let blocks: Vec<BasicBlock<'ctx>> = function.get_basic_blocks();
    if blocks.len() < MIN_BLOCKS {
        return 0;
    }

    // The entry block must remain first. `get_first_basic_block()` gives
    // us that block; everything else is shufflable.
    let Some(entry) = function.get_first_basic_block() else {
        return 0;
    };

    let mut others: Vec<BasicBlock<'ctx>> = blocks.into_iter().filter(|b| *b != entry).collect();

    // Capture the original order so we can count how many positions
    // actually changed (Fisher-Yates can by chance produce the original
    // order on small sets; reporting the real delta is more honest).
    let original = others.clone();

    others.shuffle(rng);

    // Lay the shuffled order out after `entry` in sequence:
    //   entry -> others[0] -> others[1] -> ... -> others[n-1]
    //
    // `move_after(prev)` places `self` immediately after `prev`. Iterating
    // from the first shuffled block forward keeps `prev` correct because
    // we're building the final layout left-to-right.
    let mut prev = entry;
    for bb in &others {
        // `move_after` on a BB that's already right after `prev` is a
        // no-op inside LLVM, so we don't need to special-case it.
        let _ = bb.move_after(prev);
        prev = *bb;
    }

    // Count position changes.
    others
        .iter()
        .zip(original.iter())
        .filter(|(a, b)| a != b)
        .count()
}
