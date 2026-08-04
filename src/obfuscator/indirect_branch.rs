// Copyright (c) 2025 Hakai Offensive Security.
//
// This program is free software: you can redistribute it and/or modify it
// under the terms of the GNU General Public License as published by the
// Free Software Foundation, version 3. See the LICENSE file for details.
//
// Indirect Branch pass — ROADMAP Sprint 1 / S1.
//
// What it does
// ------------
// Rewrites every direct `br` terminator inside eligible functions so the
// destination goes through a per-function block-address dispatch table:
//
//   @lyra_ib_tbl_<fn> = private constant [N x ptr] [
//       ptr blockaddress(@<fn>, %b1),
//       ptr blockaddress(@<fn>, %b2),
//       ...
//   ]
//
//   ; unconditional br %X  becomes:
//   %gep  = getelementptr inbounds [N x ptr], ptr @lyra_ib_tbl_<fn>,
//                                   i32 0, i32 <X_idx>
//   %addr = load ptr, ptr %gep
//   indirectbr ptr %addr, [ label %X ]
//
//   ; conditional br i1 %c, %T, %F  becomes:
//   %idx  = select i1 %c, i32 <T_idx>, i32 <F_idx>
//   %gep  = getelementptr inbounds [N x ptr], ptr @lyra_ib_tbl_<fn>,
//                                   i32 0, i32 %idx
//   %addr = load ptr, ptr %gep
//   indirectbr ptr %addr, [ label %T, label %F ]
//
// The entry block is excluded from the table because LLVM forbids
// `blockaddress` on entry blocks. No `br` ever targets the entry block
// in well-formed IR, so we never need to index it.
//
// What the analyst sees
// ---------------------
// IDA / Ghidra / Binary Ninja CFG reconstruction breaks: every branch
// inside a transformed function reaches the `indirectbr` terminator with
// an address-typed value the disassembler cannot resolve statically
// (even though the complete destination list is in the `indirectbr`
// operand list — most decompilers don't recover that symbolically).
// Decompilation output collapses into goto-style gibberish.
//
// Limitations (first cut)
// -----------------------
//   * `switch` terminators are left alone. A follow-up pass will lower
//     switch-to-indirectbr; for now switch-heavy functions keep their
//     tables.
//   * Conditional branches use offset-biased select operands with a
//     runtime add, giving the MBA pass an instruction to transform.
//   * Unconditional branches use a volatile-loaded key + icmp + select
//     with offset-biased operands (same shape as conditional branches).
//     LLVM can't resolve the volatile load's value, so the select
//     stays runtime-dependent and the indirectbr stays indirect.
//   * Functions with exception-handling pads are skipped via
//     `should_skip_function`.

use anyhow::{Context as _, Result};
use inkwell::basic_block::BasicBlock;
use inkwell::context::Context;
use inkwell::module::{Linkage, Module};
use inkwell::values::{
    AsValueRef, BasicValueEnum, FunctionValue, InstructionOpcode, InstructionValue, IntValue,
    Operand, PointerValue,
};
use inkwell::{AddressSpace, IntPredicate};
use rand::Rng;
use std::collections::HashMap;

use crate::obfuscator::seed;
use crate::obfuscator::util::should_skip_function;

pub fn apply<'ctx>(context: &'ctx Context, module: &mut Module<'ctx>) -> Result<()> {
    println!("[*] Applying Indirect Branch...");

    let mut rng = seed::for_pass(
        "indirect_branch",
        module.get_name().to_str().unwrap_or("module"),
        module.get_source_file_name().to_str().unwrap_or(""),
    );

    let mut total_fns = 0usize;
    let mut total_uncond = 0usize;
    let mut total_cond = 0usize;

    let functions: Vec<FunctionValue<'ctx>> = module.get_functions().collect();
    for function in functions {
        if should_skip_function(&function) {
            continue;
        }

        match transform_function(context, module, &function, &mut rng) {
            Ok(Some(stats)) => {
                total_fns += 1;
                total_uncond += stats.uncond;
                total_cond += stats.cond;
            }
            Ok(None) => {} // nothing to do
            Err(e) => {
                eprintln!(
                    "[-] indirect_branch: skipping {} - {}",
                    function.get_name().to_str().unwrap_or("?"),
                    e
                );
            }
        }
    }

    println!(
        "        ✓ Rewrote {} unconditional + {} conditional br in {} function(s)",
        total_uncond, total_cond, total_fns
    );
    Ok(())
}

#[derive(Default)]
struct Stats {
    uncond: usize,
    cond: usize,
}

fn transform_function<'ctx>(
    context: &'ctx Context,
    module: &mut Module<'ctx>,
    function: &FunctionValue<'ctx>,
    rng: &mut impl Rng,
) -> Result<Option<Stats>> {
    // Gather all `br` terminators (conditional + unconditional). Skip
    // `switch`, `indirectbr`, `ret`, `invoke`, `unreachable`, etc.
    let mut uncond_brs: Vec<(BasicBlock<'ctx>, InstructionValue<'ctx>, BasicBlock<'ctx>)> = Vec::new();
    let mut cond_brs: Vec<(
        BasicBlock<'ctx>,
        InstructionValue<'ctx>,
        IntValue<'ctx>,
        BasicBlock<'ctx>,
        BasicBlock<'ctx>,
    )> = Vec::new();

    for bb in function.get_basic_blocks() {
        let Some(term) = bb.get_terminator() else {
            continue;
        };
        if term.get_opcode() != InstructionOpcode::Br {
            continue;
        }
        let num = term.get_num_operands();
        match num {
            1 => {
                // unconditional: 1 operand = destination BasicBlock
                let Some(Operand::Block(dest)) = term.get_operand(0) else {
                    continue;
                };
                uncond_brs.push((bb, term, dest));
            }
            3 => {
                // conditional: LLVM stores operands as [cond, false_dest, true_dest]
                let Some(Operand::Value(BasicValueEnum::IntValue(cond_iv))) = term.get_operand(0)
                else {
                    continue;
                };
                let Some(Operand::Block(false_dest)) = term.get_operand(1) else {
                    continue;
                };
                let Some(Operand::Block(true_dest)) = term.get_operand(2) else {
                    continue;
                };
                cond_brs.push((bb, term, cond_iv, true_dest, false_dest));
            }
            _ => {}
        }
    }

    if uncond_brs.is_empty() && cond_brs.is_empty() {
        return Ok(None);
    }

    // Collect all non-entry blocks (blockaddress is illegal on entry).
    // These are the only blocks that can appear in the table or as a
    // destination of any `br` in well-formed IR.
    let entry = function.get_first_basic_block();
    let indexable: Vec<BasicBlock<'ctx>> = function
        .get_basic_blocks()
        .into_iter()
        .filter(|b| Some(*b) != entry)
        .collect();
    if indexable.is_empty() {
        return Ok(None);
    }

    let mut idx_of: HashMap<usize, u32> = HashMap::new();
    for (i, b) in indexable.iter().enumerate() {
        // `BasicBlock`'s PartialEq uses the raw pointer; we key by that.
        idx_of.insert(bb_key(*b), i as u32);
    }

    // Build the block-address table. `BasicBlock::get_address` is `unsafe`
    // because taking the address of the entry block is UB; we've filtered.
    let ptr_ty = context.ptr_type(AddressSpace::default());
    let i32_ty = context.i32_type();
    let table_ty = ptr_ty.array_type(indexable.len() as u32);

    let addrs: Vec<PointerValue<'ctx>> = indexable
        .iter()
        .map(|b| unsafe { b.get_address() })
        .collect::<Option<Vec<_>>>()
        .context("BasicBlock::get_address returned None (unexpected for non-entry block)")?;
    let addr_vals: Vec<BasicValueEnum<'ctx>> = addrs
        .iter()
        .copied()
        .map(BasicValueEnum::PointerValue)
        .collect();
    let table_init = const_array_of_pointers(&ptr_ty, &addr_vals);

    // Generate a stable, unique table name per function.
    let fn_name = function.get_name().to_str().unwrap_or("fn");
    let table_name = sanitize_global_name(&format!("lyra_ib_tbl.{}", fn_name));

    let table_gv = module.add_global(table_ty, None, &table_name);
    table_gv.set_linkage(Linkage::Private);
    table_gv.set_initializer(&table_init);
    table_gv.set_constant(true);

    // Per-function key global for unconditional branch opaque predicate.
    // Volatile-loaded + icmp manufactures a runtime condition LLVM can't
    // resolve, giving unconditional branches the same select + offset
    // shape as conditional ones.
    let key: u32 = rng.random();
    let key_name = sanitize_global_name(&format!("lyra_ib_key.{}", fn_name));
    let key_gv = module.add_global(i32_ty, None, &key_name);
    key_gv.set_linkage(Linkage::Private);
    key_gv.set_initializer(&i32_ty.const_int(key as u64, false));

    let builder = context.create_builder();

    let mut stats = Stats::default();
    let n_blocks = indexable.len() as u32;

    // Rewrite unconditional branches.
    for (parent_bb, old_term, dest) in uncond_brs {
        let Some(&idx) = idx_of.get(&bb_key(dest)) else {
            continue;
        };

        delete_instruction(old_term);

        builder.position_at_end(parent_bb);

        let key_load = builder
            .build_load(i32_ty, key_gv.as_pointer_value(), "ib_key")
            .map_err(|e| anyhow::anyhow!("build_load key: {e:?}"))?;
        unsafe {
            llvm_sys::core::LLVMSetVolatile(key_load.as_value_ref(), 1);
        }
        let key_const = i32_ty.const_int(key as u64, false);
        let cond = builder
            .build_int_compare(
                IntPredicate::EQ,
                key_load.into_int_value(),
                key_const,
                "ib_cond",
            )
            .map_err(|e| anyhow::anyhow!("build_int_compare: {e:?}"))?;

        let offset = (rng.random::<u32>() % 200).wrapping_add(1);
        // Decoy index: random value different from real index.  Never
        // added to the indirectbr destination list (that would create
        // a new CFG edge and break SSA dominance / PHI nodes).  The
        // select still has two distinct arms so LLVM can't fold it.
        let decoy_idx = if n_blocks > 1 {
            let mut d = rng.random::<u32>() % n_blocks;
            if d == idx {
                d = (d + 1) % n_blocks;
            }
            d
        } else {
            idx.wrapping_add(1)
        };

        let real_biased = i32_ty.const_int(idx.wrapping_sub(offset) as u64, false);
        let decoy_biased = i32_ty.const_int(decoy_idx.wrapping_sub(offset) as u64, false);
        let offset_val = i32_ty.const_int(offset as u64, false);
        let selected = builder
            .build_select(cond, real_biased, decoy_biased, "ib_sel")
            .map_err(|e| anyhow::anyhow!("build_select: {e:?}"))?;
        let idx_val = builder
            .build_int_add(selected.into_int_value(), offset_val, "ib_idx")
            .map_err(|e| anyhow::anyhow!("build_int_add: {e:?}"))?;

        let gep = build_table_gep(&builder, table_ty, table_gv.as_pointer_value(), idx_val)?;
        let addr = builder
            .build_load(ptr_ty, gep, "ib_addr")
            .map_err(|e| anyhow::anyhow!("build_load: {e:?}"))?;
        builder
            .build_indirect_branch(addr, &[dest])
            .map_err(|e| anyhow::anyhow!("build_indirect_branch: {e:?}"))?;
        stats.uncond += 1;
    }

    // Rewrite conditional branches.
    for (parent_bb, old_term, cond_iv, true_dest, false_dest) in cond_brs {
        let Some(&t_idx) = idx_of.get(&bb_key(true_dest)) else {
            continue;
        };
        let Some(&f_idx) = idx_of.get(&bb_key(false_dest)) else {
            continue;
        };

        delete_instruction(old_term);

        builder.position_at_end(parent_bb);
        let offset = (rng.random::<u32>() % 200).wrapping_add(1);
        let t_biased = i32_ty.const_int(t_idx.wrapping_sub(offset) as u64, false);
        let f_biased = i32_ty.const_int(f_idx.wrapping_sub(offset) as u64, false);
        let offset_val = i32_ty.const_int(offset as u64, false);
        let selected = builder
            .build_select(cond_iv, t_biased, f_biased, "ib_sel")
            .map_err(|e| anyhow::anyhow!("build_select: {e:?}"))?;
        let idx_val = builder
            .build_int_add(selected.into_int_value(), offset_val, "ib_idx")
            .map_err(|e| anyhow::anyhow!("build_int_add: {e:?}"))?;

        let gep = build_table_gep(&builder, table_ty, table_gv.as_pointer_value(), idx_val)?;
        let addr = builder
            .build_load(ptr_ty, gep, "ib_addr")
            .map_err(|e| anyhow::anyhow!("build_load: {e:?}"))?;
        builder
            .build_indirect_branch(addr, &[true_dest, false_dest])
            .map_err(|e| anyhow::anyhow!("build_indirect_branch: {e:?}"))?;
        stats.cond += 1;
    }

    Ok(Some(stats))
}

/// Build `getelementptr inbounds [N x ptr], ptr %table, i32 0, i32 %idx`.
fn build_table_gep<'ctx>(
    builder: &inkwell::builder::Builder<'ctx>,
    table_ty: inkwell::types::ArrayType<'ctx>,
    table_ptr: PointerValue<'ctx>,
    idx: IntValue<'ctx>,
) -> Result<PointerValue<'ctx>> {
    let i32_ty = idx.get_type();
    let zero = i32_ty.const_zero();
    unsafe {
        builder
            .build_in_bounds_gep(table_ty, table_ptr, &[zero, idx], "ib_gep")
            .map_err(|e| anyhow::anyhow!("build_in_bounds_gep: {e:?}"))
    }
}

/// Produce a stable, key-usable integer for a `BasicBlock` identity.
/// `BasicBlock`'s `PartialEq` uses the raw pointer, so the pointer value
/// works as a HashMap key.
fn bb_key(b: BasicBlock<'_>) -> usize {
    b.as_mut_ptr() as usize
}

/// Delete an instruction from its parent block.
fn delete_instruction(inst: InstructionValue<'_>) {
    unsafe {
        use llvm_sys::core::LLVMInstructionEraseFromParent;
        LLVMInstructionEraseFromParent(inst.as_value_ref());
    }
}

/// Build a `constant [N x ptr]` from a slice of pointer-typed
/// `BasicValueEnum`s. Exists because inkwell's `ArrayValue::const_array`
/// is typed on the element type; we construct via the lower-level LLVM-C
/// API to avoid having to juggle IntValue/PointerValue variants through
/// a single typed constructor.
fn const_array_of_pointers<'ctx>(
    ptr_ty: &inkwell::types::PointerType<'ctx>,
    values: &[BasicValueEnum<'ctx>],
) -> inkwell::values::ArrayValue<'ctx> {
    let elts: Vec<PointerValue<'ctx>> = values
        .iter()
        .map(|v| match v {
            BasicValueEnum::PointerValue(p) => *p,
            _ => unreachable!("non-pointer element in indirect-branch table"),
        })
        .collect();
    ptr_ty.const_array(&elts)
}

/// LLVM global names can contain most ASCII but a few characters (spaces,
/// quotes, etc.) need to be escaped. Rust mangled names are usually
/// clean, but if the crate is weird we replace noisy bytes with `_`.
fn sanitize_global_name(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '$' {
                c
            } else {
                '_'
            }
        })
        .collect()
}
