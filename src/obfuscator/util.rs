// Copyright (c) 2025 Hakai Offensive Security.
//
// This program is free software: you can redistribute it and/or modify it
// under the terms of the GNU General Public License as published by the
// Free Software Foundation, version 3. See the LICENSE file for details.
//
// Shared helpers used across obfuscation passes.
//
// The logic here was salvaged from the legacy (pre-rewrite) passes — the
// actual transformation code of the old instruction-sub / BCF / CFF passes
// was deleted for quality reasons, but their rules for deciding *which*
// functions and basic blocks are safe to touch are still worth keeping
// because every CFG-altering pass needs them.
//
// Most helpers are currently unused (no passes built on top yet); this
// module is a library of reusable building blocks for ROADMAP Sprint 1+
// passes. The `#![allow(dead_code)]` is intentional.
#![allow(dead_code)]

use inkwell::module::Module;
use inkwell::values::{AsValueRef, FunctionValue, InstructionOpcode};

/// Returns true if the function should be left alone entirely by a pass
/// that alters control flow or instruction selection.
///
/// Skip rules:
/// - Declarations (no body).
/// - LLVM intrinsics (`llvm.*`).
/// - `rust_eh_personality` (the Rust language runtime personality
///   function — touching it breaks unwinding).
/// - Any function containing an exception-handling pad (`landingpad`,
///   `catchpad`, `cleanuppad`, `catchswitch`). MSVC SEH and Itanium
///   EH both attach metadata that most transformation passes can't
///   preserve correctly.
/// - Any function that calls `llvm.coro.begin` — coroutine splitting
///   runs much later in the pipeline and expects the IR in a very
///   specific shape.
pub fn should_skip_function(function: &FunctionValue) -> bool {
    if function.count_basic_blocks() == 0 {
        return true;
    }

    if let Ok(name) = function.get_name().to_str() {
        if name.starts_with("llvm.") || name == "rust_eh_personality" {
            return true;
        }
    }

    for bb in function.get_basic_blocks() {
        for inst in bb.get_instructions() {
            match inst.get_opcode() {
                InstructionOpcode::LandingPad
                | InstructionOpcode::CatchPad
                | InstructionOpcode::CleanupPad
                | InstructionOpcode::CatchSwitch => return true,
                InstructionOpcode::Call => {
                    if callee_name_matches(&inst, |n| n.starts_with("llvm.coro.")) {
                        return true;
                    }
                }
                _ => {}
            }
        }
    }

    false
}

/// Returns true if the call instruction's callee is a function whose name
/// passes `pred`. Used to filter out calls to specific intrinsics without
/// walking the whole module.
fn callee_name_matches<F>(inst: &inkwell::values::InstructionValue, pred: F) -> bool
where
    F: Fn(&str) -> bool,
{
    let n = inst.get_num_operands();
    if n == 0 {
        return false;
    }
    // On a `call`, the last operand is the callee.
    let Some(op) = inst.get_operand(n - 1) else {
        return false;
    };
    let Some(val) = op.value() else {
        return false;
    };
    unsafe {
        use llvm_sys::core::LLVMIsAFunction;
        let raw = val.as_value_ref();
        if LLVMIsAFunction(raw).is_null() {
            return false;
        }
        let func = match FunctionValue::new(raw) {
            Some(f) => f,
            None => return false,
        };
        if let Ok(name) = func.get_name().to_str() {
            pred(name)
        } else {
            false
        }
    }
}

/// Verify the whole module after a pass finishes. On failure, prints the
/// LLVM-reported errors to stderr and returns `Err`. Callers can choose
/// to bail or to continue.
#[allow(dead_code)]
pub fn verify_module(module: &Module) -> anyhow::Result<()> {
    module
        .verify()
        .map_err(|e| anyhow::anyhow!("module verification failed: {}", e.to_string()))
}
