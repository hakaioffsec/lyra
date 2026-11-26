// Copyright (c) 2025 Hakai Offensive Security.
//
// This program is free software: you can redistribute it and/or modify it
// under the terms of the GNU General Public License as published by the
// Free Software Foundation, either version 3 or (at your option) any later
// version. See LICENSE and the generated-output permission in LICENSE-EXCEPTION.md.
//
// Mixed Boolean-Arithmetic (MBA) integer substitution pass.
//
// What it does
// ------------
// Replaces integer arithmetic and bitwise instructions (add, sub, xor,
// and, or) with algebraically equivalent expressions that mix boolean
// (bitwise) and arithmetic operations. Each operation has three MBA
// identities; one is chosen at random per instruction.
//
// Floating-point instructions are left unchanged. Real-number identities can
// change IEEE 754 rounding, overflow, subnormals, signed zero, and NaNs even
// without fast-math flags.
//
// Two rounds can transform instructions emitted by the preceding round. The
// 60 % per-round selection gate controls growth; effectiveness against a given
// analysis tool still requires measurement.
//
// Integer MBA identities
// ----------------------
// ADD:  (x^y) + 2(x&y)          |  (x|y) + (x&y)          |  2(x|y) - (x^y)
// SUB:  (x^y) - 2(~x&y)        |  (x&~y) - (~x&y)        |  ~(~x + y)
// XOR:  (x|y) - (x&y)          |  (x+y) - 2(x&y)         |  (x|y) & ~(x&y)
// AND:  (x+y) - (x|y)          |  ~(~x | ~y)              |  (x|y) - (x^y)
// OR:   (x+y) - (x&y)          |  (x^y) + (x&y)           |  (x&~y) + y
//
// Eligibility and semantics
// -------------------------
// Scalar widths above one bit only, with conservatively proven-defined
// operands. Duplicating undef-derived values invalidates some identities.
// No new freeze instructions are introduced: LLVM 22.1.8's optimized backend
// miscompiles a retained freeze(undef) counterexample. Unproven values remain
// unchanged. Identities use modular arithmetic; source poison-producing flags
// are not copied to intermediates that may wrap on otherwise defined inputs.

use std::collections::HashSet;

use anyhow::Result;
use inkwell::attributes::{Attribute, AttributeLoc};
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::values::{
    AnyValueEnum, AsValueRef, BasicValueEnum, FunctionValue, InstructionOpcode, InstructionValue,
    IntValue, Operand,
};
use rand::RngExt;

use crate::obfuscator::coverage::{self, Counts};
use crate::obfuscator::seed;
use crate::obfuscator::util::should_skip_function;

const ROUNDS: usize = 2;
const PROBABILITY: f64 = 0.6;

pub fn apply<'ctx>(context: &'ctx Context, module: &mut Module<'ctx>) -> Result<()> {
    println!("[*] Applying MBA...");

    let mut rng = seed::for_pass(
        "mba",
        module.get_name().to_str().unwrap_or("module"),
        module.get_source_file_name().to_str().unwrap_or(""),
    );

    let mut total_fns = 0usize;
    let mut total_subs = 0usize;
    let mut rounds =
        coverage::enabled().then(|| std::array::from_fn::<_, ROUNDS, _>(|_| Counts::default()));

    let functions: Vec<FunctionValue<'ctx>> = module.get_functions().collect();
    for function in functions {
        if should_skip_function(&function) {
            // Excluded originals belong only to round zero; later rounds never visit them.
            if let Some(rounds) = rounds.as_mut() {
                for block in function.get_basic_blocks() {
                    for inst in block.get_instructions() {
                        if is_candidate_opcode(inst.get_opcode()) {
                            rounds[0].considered += 1;
                            rounds[0].skip("function_exclusion", 1);
                        }
                    }
                }
            }
            continue;
        }
        let count = match transform_function(context, &function, &mut rng, rounds.as_mut()) {
            Ok(count) => count,
            Err(error) => {
                if let Some(rounds) = rounds.as_mut() {
                    for round in rounds.iter_mut() {
                        round.skip(
                            "pass_aborted",
                            round.considered - round.transformed - round.skipped,
                        );
                    }
                    emit_coverage(module, rounds, false);
                }
                return Err(error);
            }
        };
        if count > 0 {
            total_fns += 1;
            total_subs += count;
        }
    }

    println!(
        "        \u{2713} {} substitution(s) in {} function(s) ({} round(s))",
        total_subs, total_fns, ROUNDS
    );
    if let Some(rounds) = rounds {
        emit_coverage(module, &rounds, true);
    }
    Ok(())
}

fn emit_coverage(module: &Module<'_>, rounds: &[Counts; ROUNDS], complete: bool) {
    let mut total = Counts::default();
    for round in rounds {
        total.merge(round);
    }
    let extra = format!(
        "\"complete\":{},\"rounds\":[{},{}],",
        complete,
        rounds[0].json(),
        rounds[1].json()
    );
    total.emit(
        "mba",
        "candidate_instruction_visits",
        module.get_name().to_str().unwrap_or("module"),
        &extra,
    );
}

fn transform_function<'ctx>(
    context: &'ctx Context,
    function: &FunctionValue<'ctx>,
    rng: &mut impl RngExt,
    mut rounds: Option<&mut [Counts; ROUNDS]>,
) -> Result<usize> {
    let builder = context.create_builder();
    let mut total = 0;

    for round in 0..ROUNDS {
        let mut counts = rounds.as_mut().map(|rounds| &mut rounds[round]);
        let targets = collect_targets(context, function, counts.as_deref_mut());
        if targets.is_empty() {
            break;
        }
        for inst in targets {
            if rng.random::<f64>() > PROBABILITY {
                if let Some(counts) = counts.as_deref_mut() {
                    counts.skip("random_gate", 1);
                }
                continue;
            }
            if substitute(&builder, inst, rng)? {
                total += 1;
                if let Some(counts) = counts.as_deref_mut() {
                    counts.transformed += 1;
                }
            } else if let Some(counts) = counts.as_deref_mut() {
                counts.skip("substitution_declined", 1);
            }
        }
    }

    Ok(total)
}

fn collect_targets<'ctx>(
    context: &'ctx Context,
    function: &FunctionValue<'ctx>,
    mut counts: Option<&mut Counts>,
) -> Vec<InstructionValue<'ctx>> {
    // Compute eligibility before mutating IR. The worklist follows SSA users,
    // avoiding recursion and remaining independent of shuffled block order.
    // Cyclic PHIs and unsupported producers stay unproven, not assumed safe.
    let mut defined = HashSet::new();
    let noundef_attr = Attribute::get_named_enum_kind_id("noundef");
    let noundef_metadata = context.get_kind_id("noundef");
    for (index, parameter) in function.get_param_iter().enumerate() {
        if let BasicValueEnum::IntValue(value) = parameter {
            if function
                .get_enum_attribute(AttributeLoc::Param(index as u32), noundef_attr)
                .is_some()
            {
                defined.insert(value);
            }
        }
    }
    let mut worklist = Vec::new();
    for block in function.get_basic_blocks() {
        for inst in block.get_instructions() {
            if result_is_defined(inst, &defined, noundef_metadata) {
                let value = unsafe { IntValue::new(inst.as_value_ref()) };
                defined.insert(value);
                worklist.push(inst);
            }
        }
    }
    while let Some(inst) = worklist.pop() {
        let mut next_use = inst.get_first_use();
        while let Some(usage) = next_use {
            next_use = usage.get_next_use();
            let AnyValueEnum::InstructionValue(user) = usage.get_user() else {
                continue;
            };
            if !user.get_type().is_int_type() {
                continue;
            }
            let value = unsafe { IntValue::new(user.as_value_ref()) };
            if !defined.contains(&value) && result_is_defined(user, &defined, noundef_metadata) {
                defined.insert(value);
                worklist.push(user);
            }
        }
    }

    let mut out = Vec::new();
    for block in function.get_basic_blocks() {
        for inst in block.get_instructions() {
            if !is_candidate_opcode(inst.get_opcode()) {
                continue;
            }
            if let Some(counts) = counts.as_deref_mut() {
                counts.considered += 1;
            }
            let (
                Some(Operand::Value(BasicValueEnum::IntValue(lhs))),
                Some(Operand::Value(BasicValueEnum::IntValue(rhs))),
            ) = (inst.get_operand(0), inst.get_operand(1))
            else {
                if let Some(counts) = counts.as_deref_mut() {
                    counts.skip("unsupported_type", 1);
                }
                continue;
            };
            if lhs.get_type().get_bit_width() > 1
                && value_is_defined(lhs, &defined)
                && value_is_defined(rhs, &defined)
            {
                out.push(inst);
                if let Some(counts) = counts.as_deref_mut() {
                    counts.eligible += 1;
                }
            } else if let Some(counts) = counts.as_deref_mut() {
                counts.skip(
                    if lhs.get_type().get_bit_width() <= 1 {
                        "i1"
                    } else {
                        "unproven_operands"
                    },
                    1,
                );
            }
        }
    }
    out
}

fn is_candidate_opcode(opcode: InstructionOpcode) -> bool {
    matches!(
        opcode,
        InstructionOpcode::Add
            | InstructionOpcode::Sub
            | InstructionOpcode::Xor
            | InstructionOpcode::And
            | InstructionOpcode::Or
    )
}

fn value_is_defined<'ctx>(value: IntValue<'ctx>, defined: &HashSet<IntValue<'ctx>>) -> bool {
    value.is_constant_int() || defined.contains(&value)
}

fn result_is_defined<'ctx>(
    inst: InstructionValue<'ctx>,
    defined: &HashSet<IntValue<'ctx>>,
    noundef_metadata: u32,
) -> bool {
    if !inst.get_type().is_int_type() {
        return false;
    }
    let raw = inst.as_value_ref();
    let cannot_introduce_poison = match inst.get_opcode() {
        InstructionOpcode::Load => return inst.get_metadata(noundef_metadata).is_some(),
        InstructionOpcode::Add
        | InstructionOpcode::Sub
        | InstructionOpcode::Mul
        | InstructionOpcode::Trunc => unsafe {
            llvm_sys::core::LLVMGetNSW(raw) == 0 && llvm_sys::core::LLVMGetNUW(raw) == 0
        },
        InstructionOpcode::Or => unsafe { llvm_sys::core::LLVMGetIsDisjoint(raw) == 0 },
        InstructionOpcode::ZExt => unsafe { llvm_sys::core::LLVMGetNNeg(raw) == 0 },
        InstructionOpcode::ICmp => unsafe { llvm_sys::core::LLVMGetICmpSameSign(raw) == 0 },
        InstructionOpcode::Shl => {
            // MBA only introduces shifts by one. Other shift bounds are not
            // inferred here; in particular shifting an i1 by one is poison.
            let Some(Operand::Value(BasicValueEnum::IntValue(amount))) = inst.get_operand(1)
            else {
                return false;
            };
            amount.get_type().get_bit_width() > 1
                && amount == amount.get_type().const_int(1, false)
                && unsafe {
                    llvm_sys::core::LLVMGetNSW(raw) == 0 && llvm_sys::core::LLVMGetNUW(raw) == 0
                }
        }
        InstructionOpcode::And
        | InstructionOpcode::Xor
        | InstructionOpcode::SExt
        | InstructionOpcode::Select
        | InstructionOpcode::Phi
        // Do not use freeze of an unknown value as proof: LLVM 22.1.8's
        // optimized backend miscompiles some freeze(undef) MBA expansions.
        | InstructionOpcode::Freeze => true,
        _ => false,
    };
    cannot_introduce_poison
        && inst.get_operands().all(|operand| {
            matches!(
                operand,
                Some(Operand::Value(BasicValueEnum::IntValue(value)))
                    if value_is_defined(value, defined)
            )
        })
}

fn substitute<'ctx>(
    builder: &Builder<'ctx>,
    inst: InstructionValue<'ctx>,
    rng: &mut impl RngExt,
) -> Result<bool> {
    let opcode = inst.get_opcode();
    let Some(parent) = inst.get_parent() else {
        return Ok(false);
    };

    builder.position_at(parent, &inst);

    let variant = rng.random::<u32>() % 3;

    match opcode {
        InstructionOpcode::Add
        | InstructionOpcode::Sub
        | InstructionOpcode::Xor
        | InstructionOpcode::And
        | InstructionOpcode::Or => {
            let Some(Operand::Value(BasicValueEnum::IntValue(lhs))) = inst.get_operand(0) else {
                return Ok(false);
            };
            let Some(Operand::Value(BasicValueEnum::IntValue(rhs))) = inst.get_operand(1) else {
                return Ok(false);
            };
            // Do not propagate nsw/nuw/disjoint onto intermediate operations:
            // they may wrap even when the original instruction is defined.
            let result = match opcode {
                InstructionOpcode::Add => mba_add(builder, lhs, rhs, variant),
                InstructionOpcode::Sub => mba_sub(builder, lhs, rhs, variant),
                InstructionOpcode::Xor => mba_xor(builder, lhs, rhs, variant),
                InstructionOpcode::And => mba_and(builder, lhs, rhs, variant),
                InstructionOpcode::Or => mba_or(builder, lhs, rhs, variant),
                _ => return Ok(false),
            }?;
            unsafe {
                llvm_sys::core::LLVMReplaceAllUsesWith(inst.as_value_ref(), result.as_value_ref());
                llvm_sys::core::LLVMInstructionEraseFromParent(inst.as_value_ref());
            }
        }
        _ => return Ok(false),
    }

    Ok(true)
}

// ===== Substitution functions =====

/// add(x, y)
fn mba_add<'ctx>(
    b: &Builder<'ctx>,
    x: IntValue<'ctx>,
    y: IntValue<'ctx>,
    variant: u32,
) -> Result<IntValue<'ctx>> {
    let one = x.get_type().const_int(1, false);
    match variant % 3 {
        // (x ^ y) + 2*(x & y)
        0 => {
            let t1 = b.build_xor(x, y, "mba").map_err(bld)?;
            let t2 = b.build_and(x, y, "mba").map_err(bld)?;
            let t3 = b.build_left_shift(t2, one, "mba").map_err(bld)?;
            b.build_int_add(t1, t3, "mba").map_err(bld)
        }
        // (x | y) + (x & y)
        1 => {
            let t1 = b.build_or(x, y, "mba").map_err(bld)?;
            let t2 = b.build_and(x, y, "mba").map_err(bld)?;
            b.build_int_add(t1, t2, "mba").map_err(bld)
        }
        // 2*(x | y) - (x ^ y)
        _ => {
            let t1 = b.build_or(x, y, "mba").map_err(bld)?;
            let t2 = b.build_left_shift(t1, one, "mba").map_err(bld)?;
            let t3 = b.build_xor(x, y, "mba").map_err(bld)?;
            b.build_int_sub(t2, t3, "mba").map_err(bld)
        }
    }
}

/// sub(x, y)
fn mba_sub<'ctx>(
    b: &Builder<'ctx>,
    x: IntValue<'ctx>,
    y: IntValue<'ctx>,
    variant: u32,
) -> Result<IntValue<'ctx>> {
    let one = x.get_type().const_int(1, false);
    match variant % 3 {
        // (x ^ y) - 2*(~x & y)
        0 => {
            let t1 = b.build_xor(x, y, "mba").map_err(bld)?;
            let t2 = b.build_not(x, "mba").map_err(bld)?;
            let t3 = b.build_and(t2, y, "mba").map_err(bld)?;
            let t4 = b.build_left_shift(t3, one, "mba").map_err(bld)?;
            b.build_int_sub(t1, t4, "mba").map_err(bld)
        }
        // (x & ~y) - (~x & y)
        1 => {
            let t1 = b.build_not(y, "mba").map_err(bld)?;
            let t2 = b.build_and(x, t1, "mba").map_err(bld)?;
            let t3 = b.build_not(x, "mba").map_err(bld)?;
            let t4 = b.build_and(t3, y, "mba").map_err(bld)?;
            b.build_int_sub(t2, t4, "mba").map_err(bld)
        }
        // ~(~x + y)
        _ => {
            let t1 = b.build_not(x, "mba").map_err(bld)?;
            let t2 = b.build_int_add(t1, y, "mba").map_err(bld)?;
            b.build_not(t2, "mba").map_err(bld)
        }
    }
}

/// xor(x, y)
fn mba_xor<'ctx>(
    b: &Builder<'ctx>,
    x: IntValue<'ctx>,
    y: IntValue<'ctx>,
    variant: u32,
) -> Result<IntValue<'ctx>> {
    let one = x.get_type().const_int(1, false);
    match variant % 3 {
        // (x | y) - (x & y)
        0 => {
            let t1 = b.build_or(x, y, "mba").map_err(bld)?;
            let t2 = b.build_and(x, y, "mba").map_err(bld)?;
            b.build_int_sub(t1, t2, "mba").map_err(bld)
        }
        // (x + y) - 2*(x & y)
        1 => {
            let t1 = b.build_int_add(x, y, "mba").map_err(bld)?;
            let t2 = b.build_and(x, y, "mba").map_err(bld)?;
            let t3 = b.build_left_shift(t2, one, "mba").map_err(bld)?;
            b.build_int_sub(t1, t3, "mba").map_err(bld)
        }
        // (x | y) & ~(x & y)
        _ => {
            let t1 = b.build_or(x, y, "mba").map_err(bld)?;
            let t2 = b.build_and(x, y, "mba").map_err(bld)?;
            let t3 = b.build_not(t2, "mba").map_err(bld)?;
            b.build_and(t1, t3, "mba").map_err(bld)
        }
    }
}

/// and(x, y)
fn mba_and<'ctx>(
    b: &Builder<'ctx>,
    x: IntValue<'ctx>,
    y: IntValue<'ctx>,
    variant: u32,
) -> Result<IntValue<'ctx>> {
    match variant % 3 {
        // (x + y) - (x | y)
        0 => {
            let t1 = b.build_int_add(x, y, "mba").map_err(bld)?;
            let t2 = b.build_or(x, y, "mba").map_err(bld)?;
            b.build_int_sub(t1, t2, "mba").map_err(bld)
        }
        // ~(~x | ~y)
        1 => {
            let t1 = b.build_not(x, "mba").map_err(bld)?;
            let t2 = b.build_not(y, "mba").map_err(bld)?;
            let t3 = b.build_or(t1, t2, "mba").map_err(bld)?;
            b.build_not(t3, "mba").map_err(bld)
        }
        // (x | y) - (x ^ y)
        _ => {
            let t1 = b.build_or(x, y, "mba").map_err(bld)?;
            let t2 = b.build_xor(x, y, "mba").map_err(bld)?;
            b.build_int_sub(t1, t2, "mba").map_err(bld)
        }
    }
}

/// or(x, y)
fn mba_or<'ctx>(
    b: &Builder<'ctx>,
    x: IntValue<'ctx>,
    y: IntValue<'ctx>,
    variant: u32,
) -> Result<IntValue<'ctx>> {
    match variant % 3 {
        // (x + y) - (x & y)
        0 => {
            let t1 = b.build_int_add(x, y, "mba").map_err(bld)?;
            let t2 = b.build_and(x, y, "mba").map_err(bld)?;
            b.build_int_sub(t1, t2, "mba").map_err(bld)
        }
        // (x ^ y) + (x & y)
        1 => {
            let t1 = b.build_xor(x, y, "mba").map_err(bld)?;
            let t2 = b.build_and(x, y, "mba").map_err(bld)?;
            b.build_int_add(t1, t2, "mba").map_err(bld)
        }
        // (x & ~y) + y
        _ => {
            let t1 = b.build_not(y, "mba").map_err(bld)?;
            let t2 = b.build_and(x, t1, "mba").map_err(bld)?;
            b.build_int_add(t2, y, "mba").map_err(bld)
        }
    }
}

fn bld(e: impl std::fmt::Debug) -> anyhow::Error {
    anyhow::anyhow!("MBA builder: {e:?}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use inkwell::memory_buffer::MemoryBuffer;
    use inkwell::OptimizationLevel;
    use rand::{rngs::StdRng, SeedableRng};

    fn parse_module<'ctx>(context: &'ctx Context, ir: &str) -> Module<'ctx> {
        let mut bytes = ir.as_bytes().to_vec();
        bytes.push(0);
        context
            .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
                &bytes, "mba-test",
            ))
            .unwrap()
    }

    #[test]
    fn undef_operands_preserve_defined_bits() {
        // The select also covers undef flowing through SSA, not just literal
        // undef operands. Each source result is defined for either condition.
        let ir = "
            define i64 @masked(i64 %input) {
                %r = and i8 undef, 0
                %wide = zext i8 %r to i64
                ret i64 %wide
            }
            define i64 @filled(i64 %input) {
                %r = or i8 undef, -1
                %wide = zext i8 %r to i64
                ret i64 %wide
            }
            define i64 @selected(i64 %input) {
                %cond = trunc i64 %input to i1
                %value = select i1 %cond, i8 undef, i8 0
                %r = or i8 %value, -1
                %wide = zext i8 %r to i64
                ret i64 %wide
            }
            define i64 @frozen(i64 %input) {
                %value = freeze i8 undef
                %r = or i8 %value, -1
                %wide = zext i8 %r to i64
                ret i64 %wide
            }
        ";
        for seed in 0..16 {
            let context = Context::create();
            let module = parse_module(&context, ir);
            let mut rng = StdRng::seed_from_u64(seed);
            for function in module.get_functions() {
                transform_function(&context, &function, &mut rng, None).unwrap();
            }
            module.verify().unwrap();
            let engine = module
                .create_jit_execution_engine(OptimizationLevel::Aggressive)
                .unwrap();
            for (name, expected) in [
                ("masked", 0),
                ("filled", 255),
                ("selected", 255),
                ("frozen", 255),
            ] {
                let function = unsafe {
                    engine
                        .get_function::<unsafe extern "C" fn(u64) -> u64>(name)
                        .unwrap()
                };
                for input in [0, 1] {
                    assert_eq!(
                        unsafe { function.call(input) },
                        expected,
                        "{name}({input}), seed {seed}\n{}",
                        module.print_to_string()
                    );
                }
            }
        }
    }

    #[test]
    fn integer_results_match_modular_arithmetic() {
        let widths = [1, 2, 3, 4, 8, 16, 32, 64, 128];
        let operations = ["add", "sub", "xor", "and", "or"];
        let mut ir = String::new();
        for bits in widths {
            for operation in operations {
                // Pointer arguments avoid platform-dependent i128/narrow integer
                // calling conventions. The caller supplies initialized integers,
                // expressed by !noundef on the loads; arithmetic keeps its width.
                let (inputs, result) = if bits == 128 {
                    (String::new(), String::new())
                } else {
                    (
                        format!(
                            "%a = trunc i128 %raw_a to i{bits}\n\
                             %b = trunc i128 %raw_b to i{bits}\n"
                        ),
                        format!("%wide = zext i{bits} %r to i128\n"),
                    )
                };
                let (a, b, r) = if bits == 128 {
                    ("raw_a", "raw_b", "r")
                } else {
                    ("a", "b", "wide")
                };
                ir.push_str(&format!(
                    "define void @{operation}_{bits}(ptr %lhs, ptr %rhs, ptr %out) {{\n\
                     %raw_a = load i128, ptr %lhs, align 1, !noundef !0\n\
                     %raw_b = load i128, ptr %rhs, align 1, !noundef !0\n\
                     {inputs}\
                     %r = {operation} i{bits} %{a}, %{b}\n\
                     {result}\
                     store i128 %{r}, ptr %out, align 1\n\
                     ret void\n}}\n"
                ));
            }
        }
        ir.push_str("!0 = !{}\n");
        for seed in [0, 1, 42] {
            let context = Context::create();
            let module = parse_module(&context, &ir);
            let mut rng = StdRng::seed_from_u64(seed);
            for function in module.get_functions() {
                transform_function(&context, &function, &mut rng, None).unwrap();
            }
            module.verify().unwrap();
            let engine = module
                .create_jit_execution_engine(OptimizationLevel::Aggressive)
                .unwrap();
            for bits in widths {
                let mask = u128::MAX >> (128 - bits);
                let values: Vec<u128> = if bits <= 8 {
                    (0..=mask).collect()
                } else {
                    vec![
                        0,
                        1,
                        2,
                        mask >> 1,
                        (mask >> 1) + 1,
                        mask - 1,
                        mask,
                        0xAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA & mask,
                        0x55555555555555555555555555555555 & mask,
                    ]
                };
                for operation in operations {
                    let function = unsafe {
                        engine
                            .get_function::<unsafe extern "C" fn(
                                *const u128,
                                *const u128,
                                *mut u128,
                            )>(&format!("{operation}_{bits}"))
                            .unwrap()
                    };
                    for &a in &values {
                        for &b in &values {
                            let expected = match operation {
                                "add" => a.wrapping_add(b),
                                "sub" => a.wrapping_sub(b),
                                "xor" => a ^ b,
                                "and" => a & b,
                                "or" => a | b,
                                _ => unreachable!(),
                            } & mask;
                            let mut actual = 0;
                            unsafe { function.call(&a, &b, &mut actual) };
                            assert_eq!(
                                actual, expected,
                                "{operation} i{bits} ({a}, {b}), seed {seed}"
                            );
                        }
                    }
                }
            }
        }
    }
}
