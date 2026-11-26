// Copyright (c) 2025 Hakai Offensive Security.
//
// This program is free software: you can redistribute it and/or modify it
// under the terms of the GNU General Public License as published by the
// Free Software Foundation, either version 3 or (at your option) any later
// version. See LICENSE and the generated-output permission in LICENSE-EXCEPTION.md.
//
// Indirect Branch pass (S1).
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
// Analysis impact and limits
// --------------------------
// Indirect transfers can impede automatic CFG recovery, but the tables and
// runtime index computations remain available to analysis. Native measurements
// and decompiler observations must be scoped to the workload, engine and settings.
//
//   * `switch` terminators are left alone.
//   * Conditional branches use offset-biased select operands with a
//     runtime add, giving the MBA pass an instruction to transform.
//   * Unconditional branches use a volatile-loaded key + icmp + select
//     with offset-biased operands (same shape as conditional branches).
//     LLVM can't resolve the volatile load's value, so the select
//     stays runtime-dependent and the indirectbr stays indirect.
//   * Functions with exception-handling pads are skipped via
//     `should_skip_function`.
//   * GEPs of a rewritten function lose `nuw`: LLVM 22 and earlier rebase a
//     GEP onto a sibling from an `indirectbr` block and keep the flag, which
//     makes the rebased GEP poison (`drop_gep_nuw_flags`).

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

use crate::obfuscator::coverage::{self, Counts};
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
    let mut counts = coverage::enabled().then(Counts::default);

    let functions: Vec<FunctionValue<'ctx>> = module.get_functions().collect();
    for function in functions {
        if should_skip_function(&function) {
            if let Some(counts) = counts.as_mut() {
                for block in function.get_basic_blocks() {
                    if block
                        .get_terminator()
                        .is_some_and(|term| is_direct_branch(&term))
                    {
                        counts.considered += 1;
                        counts.skip("function_exclusion", 1);
                    }
                }
            }
            continue;
        }

        match transform_function(context, module, &function, &mut rng, counts.as_mut()) {
            Ok(Some(stats)) => {
                total_fns += 1;
                total_uncond += stats.uncond;
                total_cond += stats.cond;
            }
            Ok(None) => {} // nothing to do
            Err(e) => {
                if let Some(counts) = counts.as_mut() {
                    // Keep successful rewrites if a later LLVM builder operation failed.
                    counts.skip(
                        "transformation_error",
                        counts.considered - counts.transformed - counts.skipped,
                    );
                }
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
    if let Some(counts) = counts {
        counts.emit(
            "indirect-branch",
            "direct_br_terminators",
            module.get_name().to_str().unwrap_or("module"),
            "",
        );
    }
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
    mut counts: Option<&mut Counts>,
) -> Result<Option<Stats>> {
    // Gather all `br` terminators (conditional + unconditional). Skip
    // `switch`, `indirectbr`, `ret`, `invoke`, `unreachable`, etc.
    let mut uncond_brs: Vec<(BasicBlock<'ctx>, InstructionValue<'ctx>, BasicBlock<'ctx>)> =
        Vec::new();
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
        if !is_direct_branch(&term) {
            continue;
        }
        if let Some(counts) = counts.as_deref_mut() {
            counts.considered += 1;
        }
        let num = term.get_num_operands();
        match num {
            1 => {
                // unconditional: 1 operand = destination BasicBlock
                let Some(Operand::Block(dest)) = term.get_operand(0) else {
                    if let Some(counts) = counts.as_deref_mut() {
                        counts.skip("unsupported_branch_shape", 1);
                    }
                    continue;
                };
                uncond_brs.push((bb, term, dest));
            }
            3 => {
                // conditional: LLVM 23 stores operands as [cond, true_dest, false_dest]
                let Some(Operand::Value(BasicValueEnum::IntValue(cond_iv))) = term.get_operand(0)
                else {
                    if let Some(counts) = counts.as_deref_mut() {
                        counts.skip("unsupported_branch_shape", 1);
                    }
                    continue;
                };
                let Some(Operand::Block(true_dest)) = term.get_operand(1) else {
                    if let Some(counts) = counts.as_deref_mut() {
                        counts.skip("unsupported_branch_shape", 1);
                    }
                    continue;
                };
                let Some(Operand::Block(false_dest)) = term.get_operand(2) else {
                    if let Some(counts) = counts.as_deref_mut() {
                        counts.skip("unsupported_branch_shape", 1);
                    }
                    continue;
                };
                cond_brs.push((bb, term, cond_iv, true_dest, false_dest));
            }
            _ => {
                if let Some(counts) = counts.as_deref_mut() {
                    counts.skip("unsupported_branch_shape", 1);
                }
            }
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
        if let Some(counts) = counts.as_deref_mut() {
            counts.skip("no_indexable_blocks", uncond_brs.len() + cond_brs.len());
        }
        return Ok(None);
    }

    let mut idx_of: HashMap<usize, u32> = HashMap::new();
    for (i, b) in indexable.iter().enumerate() {
        // `BasicBlock`'s PartialEq uses the raw pointer; we key by that.
        idx_of.insert(bb_key(*b), i as u32);
    }
    if let Some(counts) = counts.as_deref_mut() {
        for (_, _, dest) in &uncond_brs {
            if idx_of.contains_key(&bb_key(*dest)) {
                counts.eligible += 1;
            } else {
                counts.skip("destination_not_indexable", 1);
            }
        }
        for (_, _, _, true_dest, false_dest) in &cond_brs {
            if idx_of.contains_key(&bb_key(*true_dest)) && idx_of.contains_key(&bb_key(*false_dest))
            {
                counts.eligible += 1;
            } else {
                counts.skip("destination_not_indexable", 1);
            }
        }
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
        if let Some(counts) = counts.as_deref_mut() {
            counts.transformed += 1;
        }
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
        if let Some(counts) = counts.as_deref_mut() {
            counts.transformed += 1;
        }
    }

    if stats.uncond + stats.cond > 0 {
        drop_gep_nuw_flags(function);
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

/// Remove `nuw` from every GEP in `function`, keeping `inbounds` and `nusw`.
///
/// Once this pass ends a block with `indirectbr`, LLVM 22 and earlier rebase
/// a GEP that shares its base onto a sibling GEP from that block, and keep
/// the flag on the result (`CodeGenPrepare::tryUnmergingGEPsAcrossIndirectBr`,
/// llvm/llvm-project#193487):
///
///   %next = getelementptr inbounds nuw i8, ptr %cur, i64 232
///   indirectbr ...
///   %p = getelementptr inbounds nuw i8, ptr %cur, i64 231
///     -> getelementptr inbounds nuw i8, ptr %next, i64 -1
///
/// `nuw` reads the offset as unsigned, so `%next + (2^64 - 1)` avoids
/// wrapping only when `%next` is null: the rebased GEP is poison for every
/// real slot and instruction selection folds the load to the absolute address
/// -1. Without `nuw` the rebase is exact. Upstream drops the flag on a
/// negative index from LLVM 23; this keeps every consumer LLVM correct.
fn drop_gep_nuw_flags(function: &FunctionValue<'_>) {
    for gep in function
        .get_basic_blocks()
        .into_iter()
        .flat_map(|block| block.get_instructions())
        .filter(|inst| inst.get_opcode() == InstructionOpcode::GetElementPtr)
    {
        unsafe {
            use llvm_sys::core::{LLVMGEPGetNoWrapFlags, LLVMGEPSetNoWrapFlags};
            use llvm_sys::LLVMGEPFlagNUW;
            let raw = gep.as_value_ref();
            LLVMGEPSetNoWrapFlags(raw, LLVMGEPGetNoWrapFlags(raw) & !LLVMGEPFlagNUW);
        }
    }
}

/// A direct `br`, conditional or unconditional; LLVM 23 splits the opcode.
fn is_direct_branch(term: &InstructionValue<'_>) -> bool {
    matches!(
        term.get_opcode(),
        InstructionOpcode::UncondBr | InstructionOpcode::CondBr
    )
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

#[cfg(test)]
mod tests {
    use super::*;
    use inkwell::memory_buffer::MemoryBuffer;
    use inkwell::OptimizationLevel;
    use rand::{rngs::StdRng, SeedableRng};

    #[test]
    fn indirect_branches_preserve_volatile_loops_recursion_and_tail_calls() {
        let ir = "
            define i64 @loop_volatile(i32 noundef %limit, ptr %state) {
            entry:
                %start = load volatile i32, ptr %state, align 4
                %empty = icmp eq i32 %limit, 0
                br i1 %empty, label %fast, label %setup
            fast:
                %initial = zext i32 %start to i64
                ret i64 %initial
            setup:
                br label %header
            header:
                %index = phi i32 [ 0, %setup ], [ %next_index, %latch ]
                %sum = phi i64 [ 0, %setup ], [ %next_sum, %latch ]
                %more = icmp ult i32 %index, %limit
                br i1 %more, label %body, label %done
            body:
                %value = load volatile i32, ptr %state, align 4
                %next_value = add i32 %value, 1
                store volatile i32 %next_value, ptr %state, align 4
                %wide = zext i32 %value to i64
                %bit = and i32 %value, 1
                %even = icmp eq i32 %bit, 0
                br i1 %even, label %add, label %xor
            add:
                %added = add i64 %sum, %wide
                br label %latch
            xor:
                %xored = xor i64 %sum, %wide
                br label %latch
            latch:
                %next_sum = phi i64 [ %added, %add ], [ %xored, %xor ]
                %next_index = add i32 %index, 1
                br label %header
            done:
                %final = load volatile i32, ptr %state, align 4
                %final_wide = zext i32 %final to i64
                %result = add i64 %sum, %final_wide
                ret i64 %result
            }
            define i64 @recursive_sum(i32 noundef %n) {
            entry:
                %base = icmp eq i32 %n, 0
                br i1 %base, label %done, label %recurse
            done:
                ret i64 0
            recurse:
                %smaller = sub i32 %n, 1
                %nested = call i64 @recursive_sum(i32 %smaller)
                br label %join
            join:
                %wide = zext i32 %n to i64
                %result = add i64 %nested, %wide
                ret i64 %result
            }
            define i32 @direct_entry(i32 noundef %n) {
            entry:
                br label %body
            body:
                %result = add i32 %n, 3
                br label %done
            done:
                ret i32 %result
            }
            define i32 @tail_target(i32 noundef %n) {
                %result = add i32 %n, 7
                ret i32 %result
            }
            define i32 @tail_entry(i32 noundef %n) {
            entry:
                %result = musttail call i32 @tail_target(i32 %n)
                ret i32 %result
            dead:
                br label %dead_exit
            dead_exit:
                ret i32 99
            }
        ";
        for seed in [0, 1, 42] {
            let context = Context::create();
            let mut bytes = ir.as_bytes().to_vec();
            bytes.push(0);
            let mut module = context
                .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
                    &bytes, "ib-test",
                ))
                .unwrap();
            module.verify().unwrap();
            let mut rng = StdRng::seed_from_u64(seed);
            let functions: Vec<_> = module.get_functions().collect();
            for function in functions {
                transform_function(&context, &mut module, &function, &mut rng, None).unwrap();
            }
            module.verify().unwrap();
            let engine = module
                .create_jit_execution_engine(OptimizationLevel::Aggressive)
                .unwrap();
            let loop_fn = unsafe {
                engine
                    .get_function::<unsafe extern "C" fn(u32, *mut u32) -> u64>("loop_volatile")
                    .unwrap()
            };
            for initial in [0u32, 1, 37, u32::MAX - 1, u32::MAX] {
                for limit in [0, 1, 2, 3, 31, 128] {
                    let mut state = initial;
                    // Repeated calls verify updates to caller-owned state too.
                    for _ in 0..3 {
                        let mut expected_state = state;
                        let mut sum = 0u64;
                        for _ in 0..limit {
                            sum = if expected_state & 1 == 0 {
                                sum.wrapping_add(u64::from(expected_state))
                            } else {
                                sum ^ u64::from(expected_state)
                            };
                            expected_state = expected_state.wrapping_add(1);
                        }
                        let actual = unsafe { loop_fn.call(limit, &mut state) };
                        assert_eq!(actual, sum.wrapping_add(u64::from(expected_state)));
                        assert_eq!(state, expected_state);
                    }
                }
            }
            let recursive = unsafe {
                engine
                    .get_function::<unsafe extern "C" fn(u32) -> u64>("recursive_sum")
                    .unwrap()
            };
            for n in [0, 1, 2, 7, 24] {
                assert_eq!(
                    unsafe { recursive.call(n) },
                    u64::from(n) * u64::from(n + 1) / 2
                );
            }
            let direct = unsafe {
                engine
                    .get_function::<unsafe extern "C" fn(u32) -> u32>("direct_entry")
                    .unwrap()
            };
            for n in [0u32, 1, u32::MAX - 2, u32::MAX] {
                assert_eq!(unsafe { direct.call(n) }, n.wrapping_add(3));
            }
            let tail = unsafe {
                engine
                    .get_function::<unsafe extern "C" fn(u32) -> u32>("tail_entry")
                    .unwrap()
            };
            for n in [0u32, 1, u32::MAX - 6, u32::MAX] {
                assert_eq!(unsafe { tail.call(n) }, n.wrapping_add(7));
            }
        }
    }

    /// A slot loop advances `%cur` by 232 and a successor block reads `%cur +
    /// 231`. Once this pass ends the loop header with an `indirectbr`, LLVM 22
    /// and earlier rebase the second GEP onto the first as `%next + -1`
    /// (`CodeGenPrepare::tryUnmergingGEPsAcrossIndirectBr`) and keep its `nuw`.
    /// `nuw` reads the offset as unsigned, so the rebased GEP is poison for
    /// every non-null base and the load is selected as one from the absolute
    /// address -1 (llvm/llvm-project#193487).
    #[test]
    fn indirect_branches_keep_rebased_nuw_gep_loads_register_relative() {
        use inkwell::targets::{
            CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetTriple,
        };

        let ir = r#"
            @slot_key = global i32 42

            define i8 @slot_byte(ptr %slots) {
            entry:
                br label %header
            header:
                %cur = phi ptr [ %slots, %entry ], [ %next, %latch ]
                %next = getelementptr inbounds nuw i8, ptr %cur, i64 232
                %key = load volatile i32, ptr @slot_key
                %hit = icmp eq i32 %key, 42
                br i1 %hit, label %mid, label %latch
            latch:
                %done = icmp eq ptr %next, null
                br i1 %done, label %spin, label %header
            spin:
                br label %spin
            mid:
                br i1 %hit, label %found, label %latch
            found:
                %byte_ptr = getelementptr inbounds nuw i8, ptr %cur, i64 231
                %byte = load i8, ptr %byte_ptr
                ret i8 %byte
            }
        "#;
        fn transformed<'ctx>(context: &'ctx Context, ir: &str, seed: u64) -> Module<'ctx> {
            let mut bytes = ir.as_bytes().to_vec();
            bytes.push(0);
            let mut module = context
                .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
                    &bytes,
                    "ib-gep-test",
                ))
                .unwrap();
            let mut rng = StdRng::seed_from_u64(seed);
            let function = module.get_function("slot_byte").unwrap();
            transform_function(&context, &mut module, &function, &mut rng, None).unwrap();
            module.verify().unwrap();
            module
        }

        for seed in [0, 1, 42] {
            // Every memory operand of the x86-64 code needs a base register.
            let context = Context::create();
            let module = transformed(&context, ir, seed);
            Target::initialize_x86(&InitializationConfig::default());
            let triple = TargetTriple::create("x86_64-unknown-linux-gnu");
            let target = Target::from_triple(&triple).unwrap();
            let machine = target
                .create_target_machine(
                    &triple,
                    "generic",
                    "",
                    OptimizationLevel::Aggressive,
                    RelocMode::Default,
                    CodeModel::Default,
                )
                .unwrap();
            module.set_triple(&triple);
            module.set_data_layout(&machine.get_target_data().get_data_layout());
            let assembly = machine
                .write_to_memory_buffer(&module, FileType::Assembly)
                .unwrap();
            let assembly = String::from_utf8(assembly.as_slice().to_vec()).unwrap();
            let absolute = absolute_memory_operands(&assembly);
            assert!(
                absolute.is_empty(),
                "seed {seed}: x86-64 code addresses memory absolutely: {absolute:#?}"
            );

            // Natively, the transformed function still answers slots[231].
            let context = Context::create();
            let module = transformed(&context, ir, seed);
            Target::initialize_native(&InitializationConfig::default()).unwrap();
            let engine = module
                .create_jit_execution_engine(OptimizationLevel::Aggressive)
                .unwrap();
            let slot_byte = unsafe {
                engine
                    .get_function::<unsafe extern "C" fn(*mut u8) -> u8>("slot_byte")
                    .unwrap()
            };
            let mut slots = [0u8; 232];
            slots[231] = 0x5a;
            assert_eq!(unsafe { slot_byte.call(slots.as_mut_ptr()) }, 0x5a);
        }
    }

    /// Instructions of an assembly listing whose memory operand is a bare
    /// displacement: no base, index or `%rip`.
    fn absolute_memory_operands(assembly: &str) -> Vec<String> {
        fn displacement(operand: &str) -> bool {
            let unsigned = operand.strip_prefix('-').unwrap_or(operand);
            unsigned.parse::<u64>().is_ok()
                || unsigned
                    .strip_prefix("0x")
                    .is_some_and(|digits| u64::from_str_radix(digits, 16).is_ok())
        }
        assembly
            .lines()
            .filter_map(|line| line.strip_prefix('\t'))
            .filter(|line| !line.starts_with('.'))
            .filter_map(|line| line.split_once('\t'))
            .filter(|(mnemonic, _)| {
                !mnemonic.starts_with('j')
                    && !mnemonic.starts_with("call")
                    && !mnemonic.starts_with("movabs")
            })
            .flat_map(|(mnemonic, operands)| {
                operands
                    .split(',')
                    .map(move |operand| (mnemonic, operand.trim()))
            })
            .filter(|(_, operand)| {
                !operand.starts_with('$') && !operand.contains('(') && displacement(operand)
            })
            .map(|(mnemonic, operand)| format!("{mnemonic} {operand}"))
            .collect()
    }
}
