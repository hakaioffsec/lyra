// Copyright (c) 2025 Hakai Offensive Security.
//
// This program is free software: you can redistribute it and/or modify it
// under the terms of the GNU General Public License as published by the
// Free Software Foundation, version 3. See the LICENSE file for details.
//
// Mixed Boolean-Arithmetic (MBA) substitution pass — ROADMAP Sprint 3 / B1.
//
// What it does
// ------------
// Replaces integer arithmetic and bitwise instructions (add, sub, xor,
// and, or) with algebraically equivalent expressions that mix boolean
// (bitwise) and arithmetic operations. Each operation has three MBA
// identities; one is chosen at random per instruction.
//
// Also replaces floating-point arithmetic (fadd, fsub, fmul) with
// algebraically equivalent multi-step expressions. Rust uses strict
// IEEE 754 semantics (no fast-math flags), so LLVM -O2 cannot
// reassociate or factor these expressions back to the originals.
//
// Two rounds of substitution create depth: round 2 transforms the MBA
// instructions emitted by round 1, producing deeply nested expressions
// that resist pattern matching and symbolic simplification.
//
// Only 60 % of eligible instructions are transformed per round (seeded
// RNG), which controls code-size growth and makes the pattern irregular
// enough that a blanket simplification rule can't undo all of them.
//
// Integer MBA identities
// ----------------------
// ADD:  (x^y) + 2(x&y)          |  (x|y) + (x&y)          |  2(x|y) - (x^y)
// SUB:  (x^y) - 2(~x&y)        |  (x&~y) - (~x&y)        |  ~(~x + y)
// XOR:  (x|y) - (x&y)          |  (x+y) - 2(x&y)         |  (x|y) & ~(x&y)
// AND:  (x+y) - (x|y)          |  ~(~x | ~y)              |  (x|y) - (x^y)
// OR:   (x+y) - (x&y)          |  (x^y) + (x&y)           |  (x&~y) + y
//
// Float MBA identities (survive -O2 without fast-math/reassoc)
// -------------------------------------------------------------
// FADD: 2a + (b - a)            |  a + (2b - b)            |  (a-b) + 2b
// FSUB: 2a - (a + b)            |  (a + b) - 2b            |  (a-b) + (b-b) [→ a-b+0, but via separate ops]
// FMUL: (a+a)*(b*0.5)           |  (a*0.5)*(b+b)           |  (a*2.0)*(b*0.5)
//
// What the analyst sees
// ---------------------
// Every simple arithmetic operation explodes into 3-5 instructions per
// round. After two rounds a single `add` becomes up to 16 instructions of
// interleaved bitwise and arithmetic ops (typically ~6-10 at the 60%
// substitution gate). IDA's micro-code optimizer and
// Hex-Rays decompiler cannot fold these back to the original operation,
// so:
//   - Decompiled pseudo-C is unreadable noise for any function with MBA
//   - When combined with indirect_branch, the dispatch-table index
//     computation becomes opaque — IDA's value tracking fails and the
//     CFG stays broken even if the analyst manually creates xrefs
//   - Symbolic execution (angr, Triton) must reason through deep MBA
//     trees, making automated analysis orders of magnitude slower

use anyhow::Result;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::values::{
    AsValueRef, BasicValueEnum, FloatValue, FunctionValue, InstructionOpcode, InstructionValue,
    IntValue, Operand,
};
use rand::Rng;

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

    let functions: Vec<FunctionValue<'ctx>> = module.get_functions().collect();
    for function in functions {
        if should_skip_function(&function) {
            continue;
        }
        let count = transform_function(context, &function, &mut rng)?;
        if count > 0 {
            total_fns += 1;
            total_subs += count;
        }
    }

    println!(
        "        \u{2713} {} substitution(s) in {} function(s) ({} round(s))",
        total_subs, total_fns, ROUNDS
    );
    Ok(())
}

fn transform_function<'ctx>(
    context: &'ctx Context,
    function: &FunctionValue<'ctx>,
    rng: &mut impl Rng,
) -> Result<usize> {
    let builder = context.create_builder();
    let mut total = 0;

    for _round in 0..ROUNDS {
        let targets = collect_targets(function);
        if targets.is_empty() {
            break;
        }
        for inst in targets {
            if rng.random::<f64>() > PROBABILITY {
                continue;
            }
            if substitute(&builder, inst, rng)? {
                total += 1;
            }
        }
    }

    Ok(total)
}

fn collect_targets<'ctx>(function: &FunctionValue<'ctx>) -> Vec<InstructionValue<'ctx>> {
    let mut out = Vec::new();
    for bb in function.get_basic_blocks() {
        for inst in bb.get_instructions() {
            match inst.get_opcode() {
                InstructionOpcode::Add
                | InstructionOpcode::Sub
                | InstructionOpcode::Xor
                | InstructionOpcode::And
                | InstructionOpcode::Or => {
                    let (Some(Operand::Value(BasicValueEnum::IntValue(v))), Some(Operand::Value(BasicValueEnum::IntValue(_)))) =
                        (inst.get_operand(0), inst.get_operand(1))
                    else {
                        continue;
                    };
                    if v.get_type().get_bit_width() <= 1 {
                        continue;
                    }
                }
                InstructionOpcode::FAdd
                | InstructionOpcode::FSub
                | InstructionOpcode::FMul => {
                    let (Some(Operand::Value(BasicValueEnum::FloatValue(_))), Some(Operand::Value(BasicValueEnum::FloatValue(_)))) =
                        (inst.get_operand(0), inst.get_operand(1))
                    else {
                        continue;
                    };
                }
                _ => continue,
            }
            out.push(inst);
        }
    }
    out
}

fn substitute<'ctx>(
    builder: &Builder<'ctx>,
    inst: InstructionValue<'ctx>,
    rng: &mut impl Rng,
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
            let result = match opcode {
                InstructionOpcode::Add => mba_add(builder, lhs, rhs, variant),
                InstructionOpcode::Sub => mba_sub(builder, lhs, rhs, variant),
                InstructionOpcode::Xor => mba_xor(builder, lhs, rhs, variant),
                InstructionOpcode::And => mba_and(builder, lhs, rhs, variant),
                InstructionOpcode::Or => mba_or(builder, lhs, rhs, variant),
                _ => return Ok(false),
            }?;
            unsafe {
                llvm_sys::core::LLVMReplaceAllUsesWith(
                    inst.as_value_ref(),
                    result.as_value_ref(),
                );
                llvm_sys::core::LLVMInstructionEraseFromParent(inst.as_value_ref());
            }
        }
        InstructionOpcode::FAdd | InstructionOpcode::FSub | InstructionOpcode::FMul => {
            let Some(Operand::Value(BasicValueEnum::FloatValue(lhs))) = inst.get_operand(0)
            else {
                return Ok(false);
            };
            let Some(Operand::Value(BasicValueEnum::FloatValue(rhs))) = inst.get_operand(1)
            else {
                return Ok(false);
            };
            let result = match opcode {
                InstructionOpcode::FAdd => fmba_add(builder, lhs, rhs, variant),
                InstructionOpcode::FSub => fmba_sub(builder, lhs, rhs, variant),
                InstructionOpcode::FMul => fmba_mul(builder, lhs, rhs, variant),
                _ => return Ok(false),
            }?;
            unsafe {
                llvm_sys::core::LLVMReplaceAllUsesWith(
                    inst.as_value_ref(),
                    result.as_value_ref(),
                );
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

// ===== Float MBA substitutions =====
// These survive LLVM -O2 because Rust emits strict IEEE 754 (no
// fast-math flags). Without `reassoc`, LLVM cannot factor or
// reassociate the intermediate fadd/fsub/fmul back to the original.

/// fadd(a, b)
fn fmba_add<'ctx>(
    b: &Builder<'ctx>,
    a: FloatValue<'ctx>,
    y: FloatValue<'ctx>,
    variant: u32,
) -> Result<FloatValue<'ctx>> {
    let two = a.get_type().const_float(2.0);
    match variant % 3 {
        // 2a + (b - a)  =  a + b
        0 => {
            let t1 = b.build_float_mul(a, two, "fmba").map_err(bld)?;
            let t2 = b.build_float_sub(y, a, "fmba").map_err(bld)?;
            b.build_float_add(t1, t2, "fmba").map_err(bld)
        }
        // a + (2b - b)  =  a + b
        1 => {
            let t1 = b.build_float_mul(y, two, "fmba").map_err(bld)?;
            let t2 = b.build_float_sub(t1, y, "fmba").map_err(bld)?;
            b.build_float_add(a, t2, "fmba").map_err(bld)
        }
        // (a - b) + 2b  =  a + b
        _ => {
            let t1 = b.build_float_sub(a, y, "fmba").map_err(bld)?;
            let t2 = b.build_float_mul(y, two, "fmba").map_err(bld)?;
            b.build_float_add(t1, t2, "fmba").map_err(bld)
        }
    }
}

/// fsub(a, b)
fn fmba_sub<'ctx>(
    b: &Builder<'ctx>,
    a: FloatValue<'ctx>,
    y: FloatValue<'ctx>,
    variant: u32,
) -> Result<FloatValue<'ctx>> {
    let two = a.get_type().const_float(2.0);
    match variant % 3 {
        // 2a - (a + b)  =  a - b
        0 => {
            let t1 = b.build_float_mul(a, two, "fmba").map_err(bld)?;
            let t2 = b.build_float_add(a, y, "fmba").map_err(bld)?;
            b.build_float_sub(t1, t2, "fmba").map_err(bld)
        }
        // (a + b) - 2b  =  a - b
        1 => {
            let t1 = b.build_float_add(a, y, "fmba").map_err(bld)?;
            let t2 = b.build_float_mul(y, two, "fmba").map_err(bld)?;
            b.build_float_sub(t1, t2, "fmba").map_err(bld)
        }
        // (2a - b) - a  =  a - b
        _ => {
            let t1 = b.build_float_mul(a, two, "fmba").map_err(bld)?;
            let t2 = b.build_float_sub(t1, y, "fmba").map_err(bld)?;
            b.build_float_sub(t2, a, "fmba").map_err(bld)
        }
    }
}

/// fmul(a, b)
fn fmba_mul<'ctx>(
    b: &Builder<'ctx>,
    a: FloatValue<'ctx>,
    y: FloatValue<'ctx>,
    variant: u32,
) -> Result<FloatValue<'ctx>> {
    let half = a.get_type().const_float(0.5);
    let two = a.get_type().const_float(2.0);
    match variant % 3 {
        // (a + a) * (b * 0.5)  =  2a * 0.5b  =  ab
        0 => {
            let t1 = b.build_float_add(a, a, "fmba").map_err(bld)?;
            let t2 = b.build_float_mul(y, half, "fmba").map_err(bld)?;
            b.build_float_mul(t1, t2, "fmba").map_err(bld)
        }
        // (a * 0.5) * (b + b)  =  0.5a * 2b  =  ab
        1 => {
            let t1 = b.build_float_mul(a, half, "fmba").map_err(bld)?;
            let t2 = b.build_float_add(y, y, "fmba").map_err(bld)?;
            b.build_float_mul(t1, t2, "fmba").map_err(bld)
        }
        // (a * 2) * (b * 0.5)  =  2a * 0.5b  =  ab
        _ => {
            let t1 = b.build_float_mul(a, two, "fmba").map_err(bld)?;
            let t2 = b.build_float_mul(y, half, "fmba").map_err(bld)?;
            b.build_float_mul(t1, t2, "fmba").map_err(bld)
        }
    }
}

fn bld(e: impl std::fmt::Debug) -> anyhow::Error {
    anyhow::anyhow!("MBA builder: {e:?}")
}
