// Copyright (c) 2025 Hakai Offensive Security.
// SPDX-License-Identifier: GPL-3.0-or-later

//! Encoded local call targets, not encryption or semantic secrecy.
//!
//! Only ordinary, nonvariadic calls to exact internal/private definitions in
//! address space zero are selected. Existing function exclusions still apply.
//! Intrinsics, inline assembly, indirect/alias targets, declarations, interposable
//! definitions, musttail calls and operand bundles are left intact. Calls with
//! `callees`, `prof` or `callee_type` metadata are excluded: retaining these hints
//! can disclose/promote the original target, and stripping them would violate the
//! metadata-preservation contract. Other metadata, ABI attributes, conventions,
//! arguments, results and normal tail markers survive because calls are never
//! rebuilt. Invoke and callbr are outside the call-instruction denominator.
//!
//! A nonempty LLVM data layout must describe integral 32/64-bit default pointers
//! with equal address/index widths and default program/global address spaces.
//! LLVM's specified 64-bit default when a nonempty layout omits `p0` is used,
//! never the host width. Unsupported or absent layouts are accounted skips.
//!
//! Planning precedes mutation. Each distinct target has one private mutable
//! integer-table entry (ptrtoint(target) + a positive 31-bit seeded key). A
//! volatile load, wrapping subtraction and inttoptr recover its address. The
//! original call's final operand alone changes; no constructor/cache is needed.

use anyhow::{Context as _, Result};
use inkwell::context::Context;
use inkwell::module::{Linkage, Module};
use inkwell::values::{
    AsValueRef, FunctionValue, InstructionOpcode, InstructionValue, PointerValue,
};
use inkwell::AddressSpace;
use llvm_sys::core::{
    LLVMGetCalledFunctionType, LLVMGetCalledValue, LLVMGetIntrinsicID, LLVMGetMetadata,
    LLVMGetNumOperandBundles, LLVMGetTailCallKind, LLVMIsAFunction, LLVMIsAInlineAsm,
    LLVMIsFunctionVarArg, LLVMSetVolatile,
};
use llvm_sys::LLVMTailCallKind;
use rand::{seq::SliceRandom, Rng};
use std::collections::HashMap;

use super::coverage::{self, Counts};
use super::seed;
use super::util::should_skip_function;

struct Site<'ctx> {
    call: InstructionValue<'ctx>,
    target: usize,
}

struct Target<'ctx> {
    function: FunctionValue<'ctx>,
    key: u64,
    slot: Option<PointerValue<'ctx>>,
}

pub fn apply<'ctx>(context: &'ctx Context, module: &mut Module<'ctx>) -> Result<()> {
    println!("[*] Applying Indirect Call...");
    let mut rng = seed::for_pass(
        "indirect_call",
        module.get_name().to_str().unwrap_or("module"),
        module.get_source_file_name().to_str().unwrap_or(""),
    );
    let mut counts = coverage::enabled().then(Counts::default);
    let result = transform(context, module, &mut rng, counts.as_mut());
    if let Some(counts) = counts.as_mut() {
        if result.is_err() {
            counts.skip(
                "transformation_error",
                counts.considered - counts.transformed - counts.skipped,
            );
        }
        counts.emit(
            "indirect-call",
            "call_instructions",
            module.get_name().to_str().unwrap_or("module"),
            if result.is_ok() {
                "\"complete\":true,"
            } else {
                "\"complete\":false,"
            },
        );
    }
    let transformed = result?;
    println!("        ✓ Rewrote {transformed} local call(s)");
    Ok(())
}

fn transform<'ctx>(
    context: &'ctx Context,
    module: &mut Module<'ctx>,
    rng: &mut impl Rng,
    mut counts: Option<&mut Counts>,
) -> Result<usize> {
    let layout = module.get_data_layout();
    let width = pointer_width(layout.as_str().to_str().unwrap_or(""));
    let target_hints = ["callees", "prof", "callee_type"].map(|name| context.get_kind_id(name));
    let mut sites = Vec::new();
    let mut targets = Vec::new();
    let mut target_ids = HashMap::new();

    for function in module.get_functions() {
        let excluded = should_skip_function(&function);
        if excluded && counts.is_none() {
            continue;
        }
        for block in function.get_basic_block_iter() {
            for call in block.get_instructions() {
                if call.get_opcode() != InstructionOpcode::Call {
                    continue;
                }
                if let Some(counts) = counts.as_deref_mut() {
                    counts.considered += 1;
                }
                let target = if excluded {
                    Err("function_exclusion")
                } else if let Err(reason) = width {
                    Err(reason)
                } else {
                    eligible_target(call, &target_hints)
                };
                match target {
                    Err(reason) => {
                        if let Some(counts) = counts.as_deref_mut() {
                            counts.skip(reason, 1);
                        }
                    }
                    Ok(function) => {
                        let target =
                            *target_ids
                                .entry(function.as_value_ref())
                                .or_insert_with(|| {
                                    let index = targets.len();
                                    targets.push(Target {
                                        function,
                                        key: 0,
                                        slot: None,
                                    });
                                    index
                                });
                        sites.push(Site { call, target });
                        if let Some(counts) = counts.as_deref_mut() {
                            counts.eligible += 1;
                        }
                    }
                }
            }
        }
    }

    let transformed = sites.len();
    if !sites.is_empty() {
        let width = width.map_err(|reason| anyhow::anyhow!(reason))?;
        let int_type = if width == 32 {
            context.i32_type()
        } else {
            context.i64_type()
        };
        let pointer_type = context.ptr_type(AddressSpace::default());
        let size =
            u32::try_from(targets.len()).context("indirect-call table exceeds LLVM array size")?;
        let table_type = int_type.array_type(size);
        // HashMap iteration never influences RNG draws or output ordering.
        let mut order: Vec<_> = (0..targets.len()).collect();
        order.shuffle(rng);
        let initializer: Vec<_> = order
            .iter()
            .map(|&id| {
                let target = &mut targets[id];
                target.key = rng.random_range(1..=0x7fff_ffffu32) as u64;
                target
                    .function
                    .as_global_value()
                    .as_pointer_value()
                    .const_to_int(int_type)
                    .const_add(int_type.const_int(target.key, false))
            })
            .collect();
        let table = module.add_global(table_type, Some(AddressSpace::default()), "lyra_ic_table");
        table.set_linkage(Linkage::Private);
        table.set_constant(false);
        table.set_initializer(&int_type.const_array(&initializer));
        let zero = int_type.const_zero();
        for (index, &id) in order.iter().enumerate() {
            // Constant GEP construction allocates only once per target, not per call.
            targets[id].slot = Some(unsafe {
                table.as_pointer_value().const_in_bounds_gep(
                    table_type,
                    &[zero, int_type.const_int(index as u64, false)],
                )
            });
        }
        let builder = context.create_builder();
        for site in sites {
            let target = &targets[site.target];
            builder.position_before(&site.call);
            // Empty names avoid temporary CString allocations in inkwell's builders.
            let encoded =
                builder.build_load(int_type, target.slot.context("unplanned call target")?, "")?;
            unsafe { LLVMSetVolatile(encoded.as_value_ref(), 1) };
            let address = builder.build_int_sub(
                encoded.into_int_value(),
                int_type.const_int(target.key, false),
                "",
            )?;
            let callee = builder.build_int_to_ptr(address, pointer_type, "")?;
            // CallInst stores its callee last, after arguments and bundles (bundles
            // are excluded). Preserve the complete instruction and its SSA users.
            let operand = site.call.get_num_operands() - 1;
            anyhow::ensure!(
                site.call.set_operand(operand, callee),
                "failed to replace call target"
            );
            if let Some(counts) = counts.as_deref_mut() {
                counts.transformed += 1;
            }
        }
    }
    module
        .verify()
        .map_err(|error| anyhow::anyhow!("indirect-call verification failed: {error}"))?;
    Ok(transformed)
}

fn eligible_target<'ctx>(
    call: InstructionValue<'ctx>,
    target_hints: &[u32; 3],
) -> std::result::Result<FunctionValue<'ctx>, &'static str> {
    // All call-specific C API operations are guarded by the caller's opcode test.
    unsafe {
        let raw = call.as_value_ref();
        if LLVMGetTailCallKind(raw) == LLVMTailCallKind::LLVMTailCallKindMustTail {
            return Err("musttail");
        }
        if LLVMGetNumOperandBundles(raw) != 0 {
            return Err("operand_bundles");
        }
        let callee = LLVMGetCalledValue(raw);
        if !LLVMIsAInlineAsm(callee).is_null() {
            return Err("inline_asm");
        }
        if LLVMIsAFunction(callee).is_null() {
            return Err("indirect_or_alias_target");
        }
        let function = FunctionValue::new(callee).ok_or("indirect_or_alias_target")?;
        if LLVMGetIntrinsicID(callee) != 0 {
            return Err("intrinsic");
        }
        if function.count_basic_blocks() == 0 {
            return Err("declaration");
        }
        if !matches!(function.get_linkage(), Linkage::Internal | Linkage::Private) {
            return Err("nonlocal_linkage");
        }
        if function.get_type().is_var_arg()
            || LLVMIsFunctionVarArg(LLVMGetCalledFunctionType(raw)) != 0
        {
            return Err("variadic");
        }
        if function
            .as_global_value()
            .as_pointer_value()
            .get_type()
            .get_address_space()
            != AddressSpace::default()
        {
            return Err("nonzero_address_space");
        }
        if target_hints
            .iter()
            .any(|&kind| !LLVMGetMetadata(raw, kind).is_null())
        {
            return Err("target_metadata");
        }
        Ok(function)
    }
}

fn pointer_width(layout: &str) -> std::result::Result<u32, &'static str> {
    if layout.is_empty() {
        return Err("missing_data_layout");
    }
    // LLVM's C API does not expose non-integral pointer properties. Inspect
    // LLVM 22's already-validated layout, including its specified default.
    let mut width = 64;
    for part in layout.split('-') {
        if let Some(space) = part.strip_prefix('P').or_else(|| part.strip_prefix('G')) {
            if space
                .parse::<u32>()
                .map_err(|_| "unsupported_pointer_layout")?
                != 0
            {
                return Err("nonzero_address_space");
            }
        }
        if let Some(spaces) = part.strip_prefix("ni:") {
            if spaces.split(':').any(|space| space.parse::<u32>() == Ok(0)) {
                return Err("nonintegral_pointer_layout");
            }
        }
        if let Some(spec) = part.strip_prefix('p') {
            let (head, fields) = spec.split_once(':').ok_or("unsupported_pointer_layout")?;
            let address = head
                .trim_start_matches(['u', 'e'])
                .split('(')
                .next()
                .unwrap_or("");
            if !address.is_empty()
                && address
                    .parse::<u32>()
                    .map_err(|_| "unsupported_pointer_layout")?
                    != 0
            {
                continue;
            }
            if head.starts_with(['u', 'e']) {
                return Err("nonintegral_pointer_layout");
            }
            let mut fields = fields.split(':');
            let size = fields.next().ok_or("unsupported_pointer_layout")?;
            width = size.parse().map_err(|_| "unsupported_pointer_layout")?;
            let index = fields
                .nth(2)
                .map(str::parse::<u32>)
                .transpose()
                .map_err(|_| "unsupported_pointer_layout")?;
            if index.is_some_and(|index| index != width) {
                return Err("pointer_index_width_mismatch");
            }
        }
    }
    match width {
        32 | 64 => Ok(width),
        _ => Err("unsupported_pointer_width"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use inkwell::memory_buffer::MemoryBuffer;
    use inkwell::OptimizationLevel;
    use rand::{rngs::StdRng, SeedableRng};

    fn parse<'ctx>(context: &'ctx Context, ir: &str) -> Module<'ctx> {
        let mut bytes = ir.as_bytes().to_vec();
        bytes.push(0);
        let module = context
            .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
                &bytes,
                "indirect-call-test",
            ))
            .unwrap();
        module.verify().unwrap();
        module
    }

    #[test]
    fn native_calls_preserve_abi_memory_recursion_and_tail_results() {
        let ir = r#"
            target datalayout = "e-p:64:64-i64:64-n8:16:32:64-S128"
            %Pair = type { i64, double }
            define private i64 @narrow(i8 signext %a, i16 zeroext %b, i32 %c, i64 %d) noinline {
                %aa = sext i8 %a to i64
                %bb = zext i16 %b to i64
                %cc = sext i32 %c to i64
                %ab = add i64 %aa, %bb
                %cd = add i64 %cc, %d
                %r = add i64 %ab, %cd
                ret i64 %r
            }
            define i64 @integers(i8 signext %a, i16 zeroext %b, i32 %c, i64 %d) {
                %r = call i64 @narrow(i8 signext %a, i16 zeroext %b, i32 %c, i64 %d)
                ret i64 %r
            }
            define internal fastcc double @mixed(float %a, double %b, i64 %c) noinline {
                %aa = fpext float %a to double
                %cc = sitofp i64 %c to double
                %ab = fsub double %aa, %b
                %r = fadd double %ab, %cc
                ret double %r
            }
            define double @floating(float %a, double %b, i64 %c) {
                %r = call fastcc double @mixed(float %a, double %b, i64 %c)
                ret double %r
            }
            define private %Pair @pair_value(%Pair %p) noinline {
                %a = extractvalue %Pair %p, 0
                %b = extractvalue %Pair %p, 1
                %aa = add i64 %a, 7
                %bb = fmul double %b, 2.0
                %q = insertvalue %Pair poison, i64 %aa, 0
                %r = insertvalue %Pair %q, double %bb, 1
                ret %Pair %r
            }
            define i64 @aggregate(i64 %x, double %y) {
                %a = insertvalue %Pair poison, i64 %x, 0
                %b = insertvalue %Pair %a, double %y, 1
                %p = call %Pair @pair_value(%Pair %b)
                %c = extractvalue %Pair %p, 0
                %d = extractvalue %Pair %p, 1
                %e = fptosi double %d to i64
                %r = add i64 %c, %e
                ret i64 %r
            }
            define private void @pair_copy(ptr sret(%Pair) align 8 %out, ptr byval(%Pair) align 8 %in) noinline {
                %a = load i64, ptr %in, align 8
                %bp = getelementptr %Pair, ptr %in, i32 0, i32 1
                %b = load double, ptr %bp, align 8
                %aa = add i64 %a, 11
                %bb = fadd double %b, 3.0
                store i64 %aa, ptr %out, align 8
                %ob = getelementptr %Pair, ptr %out, i32 0, i32 1
                store double %bb, ptr %ob, align 8
                store i64 999, ptr %in, align 8
                ret void
            }
            define i64 @sret_byval(ptr %in) {
                %out = alloca %Pair, align 8
                call void @pair_copy(ptr sret(%Pair) align 8 %out, ptr byval(%Pair) align 8 %in)
                %a = load i64, ptr %out, align 8
                %bp = getelementptr %Pair, ptr %out, i32 0, i32 1
                %b = load double, ptr %bp, align 8
                %c = fptosi double %b to i64
                %r = add i64 %a, %c
                ret i64 %r
            }
            define private i64 @update(ptr %state, i64 %delta) noinline {
                %old = load volatile i64, ptr %state, align 8
                %new = add i64 %old, %delta
                store volatile i64 %new, ptr %state, align 8
                ret i64 %old
            }
            define i64 @stateful(ptr %state, i64 %delta) {
                %first = call i64 @update(ptr %state, i64 %delta)
                %second = call i64 @update(ptr %state, i64 %delta)
                %r = xor i64 %first, %second
                ret i64 %r
            }
            define internal i64 @sum(i64 %n) noinline {
                %base = icmp eq i64 %n, 0
                br i1 %base, label %done, label %recurse
            done:
                ret i64 0
            recurse:
                %prev = sub i64 %n, 1
                %nested = call i64 @sum(i64 %prev)
                %r = add i64 %nested, %n
                ret i64 %r
            }
            define i64 @recursive(i64 %n) {
                %r = call i64 @sum(i64 %n)
                ret i64 %r
            }
            define private i64 @tail_target(i64 %n) noinline {
                %r = add i64 %n, 13
                ret i64 %r
            }
            define i64 @normal_tail(i64 %n) {
                %r = tail call i64 @tail_target(i64 %n)
                ret i64 %r
            }
        "#;
        #[repr(C)]
        struct Pair {
            integer: i64,
            floating: f64,
        }
        for seed in [0, 1, 42] {
            let context = Context::create();
            let mut module = parse(&context, ir);
            super::super::util::set_native_test_target(&module);
            let mut counts = Counts::default();
            transform(
                &context,
                &mut module,
                &mut StdRng::seed_from_u64(seed),
                Some(&mut counts),
            )
            .unwrap();
            assert_eq!(
                (counts.considered, counts.transformed, counts.skipped),
                (9, 9, 0)
            );
            let engine = module
                .create_jit_execution_engine(OptimizationLevel::Aggressive)
                .unwrap();
            unsafe {
                let integer = engine
                    .get_function::<unsafe extern "C" fn(i8, u16, i32, i64) -> i64>("integers")
                    .unwrap();
                for (a, b, c, d) in [
                    (-128, 65535, i32::MIN, i64::MAX),
                    (127, 0, i32::MAX, i64::MIN),
                    (-1, 19, -27, 37),
                    (0, 0, 0, 0),
                ] {
                    assert_eq!(
                        integer.call(a, b, c, d),
                        (a as i64)
                            .wrapping_add(b as i64)
                            .wrapping_add(c as i64)
                            .wrapping_add(d)
                    );
                }
                let floating = engine
                    .get_function::<unsafe extern "C" fn(f32, f64, i64) -> f64>("floating")
                    .unwrap();
                for (a, b, c) in [(1.5, 2.25, 7), (-8.0, 4.5, -3), (0.0, -0.5, 0)] {
                    assert_eq!(floating.call(a, b, c), f64::from(a) - b + c as f64);
                }
                let aggregate = engine
                    .get_function::<unsafe extern "C" fn(i64, f64) -> i64>("aggregate")
                    .unwrap();
                let sret = engine
                    .get_function::<unsafe extern "C" fn(*mut Pair) -> i64>("sret_byval")
                    .unwrap();
                for (x, y) in [(-71, 3.5), (0, 0.0), (1200, -6.5)] {
                    assert_eq!(aggregate.call(x, y), x + 7 + (y * 2.0) as i64);
                    let mut pair = Pair {
                        integer: x,
                        floating: y,
                    };
                    assert_eq!(sret.call(&mut pair), x + 11 + (y + 3.0) as i64);
                    assert_eq!((pair.integer, pair.floating), (x, y));
                }
                let stateful = engine
                    .get_function::<unsafe extern "C" fn(*mut u64, u64) -> u64>("stateful")
                    .unwrap();
                for initial in [0u64, 17, u64::MAX - 1] {
                    let mut state = initial;
                    for delta in [0u64, 1, 91, u64::MAX] {
                        let old = state;
                        assert_eq!(
                            stateful.call(&mut state, delta),
                            old ^ old.wrapping_add(delta)
                        );
                        assert_eq!(state, old.wrapping_add(delta).wrapping_add(delta));
                    }
                }
                let recursive = engine
                    .get_function::<unsafe extern "C" fn(u64) -> u64>("recursive")
                    .unwrap();
                let tail = engine
                    .get_function::<unsafe extern "C" fn(u64) -> u64>("normal_tail")
                    .unwrap();
                for n in [0, 1, 2, 17, 64] {
                    assert_eq!(recursive.call(n), n * (n + 1) / 2);
                    assert_eq!(tail.call(n), n + 13);
                }
                assert_eq!(tail.call(u64::MAX), 12);
            }
        }
    }

    #[test]
    fn excluded_calls_keep_native_results_and_complete_accounting() {
        let ir = r#"
            target datalayout = "e-p:64:64-i64:64-n8:16:32:64-S128"
            define private i64 @local(i64 %x) noinline {
                %r = add i64 %x, 5
                ret i64 %r
            }
            define i64 @must_tail(i64 %x) {
                %r = musttail call i64 @local(i64 %x)
                ret i64 %r
            }
            define private i64 @variadic(i64 %x, ...) noinline {
                %r = mul i64 %x, 3
                ret i64 %r
            }
            define i64 @call_variadic(i64 %x) {
                %r = call i64 (i64, ...) @variadic(i64 %x, double 2.0)
                ret i64 %r
            }
            declare i64 @foreign(i64)
            define i64 @call_foreign(i64 %x) {
                %r = call i64 @foreign(i64 %x)
                ret i64 %r
            }
            define i64 @interposable(i64 %x) noinline {
                %r = xor i64 %x, 91
                ret i64 %r
            }
            define i64 @call_interposable(i64 %x) {
                %r = call i64 @interposable(i64 %x)
                ret i64 %r
            }
            define i64 @hinted(i64 %x) {
                %r = call i64 @local(i64 %x), !prof !0
                ret i64 %r
            }
            define i64 @already_indirect(ptr %target, i64 %x) {
                %r = call i64 %target(i64 %x)
                ret i64 %r
            }
            !0 = !{!"branch_weights", i32 9}
        "#;
        unsafe extern "C" fn foreign(x: u64) -> u64 {
            x.wrapping_mul(7).wrapping_sub(2)
        }
        let context = Context::create();
        let mut module = parse(&context, ir);
        super::super::util::set_native_test_target(&module);
        let mut counts = Counts::default();
        transform(
            &context,
            &mut module,
            &mut StdRng::seed_from_u64(42),
            Some(&mut counts),
        )
        .unwrap();
        assert_eq!(
            (
                counts.considered,
                counts.eligible,
                counts.transformed,
                counts.skipped
            ),
            (6, 0, 0, 6)
        );
        for reason in [
            "musttail",
            "variadic",
            "declaration",
            "nonlocal_linkage",
            "target_metadata",
            "indirect_or_alias_target",
        ] {
            assert_eq!(counts.skip_reasons.get(reason), Some(&1));
        }
        let engine = module
            .create_jit_execution_engine(OptimizationLevel::Aggressive)
            .unwrap();
        engine.add_global_mapping(
            &module.get_function("foreign").unwrap(),
            foreign as *const () as usize,
        );
        unsafe {
            for (name, kind) in [
                ("must_tail", 0),
                ("call_variadic", 1),
                ("call_foreign", 2),
                ("call_interposable", 3),
                ("hinted", 0),
            ] {
                let function = engine
                    .get_function::<unsafe extern "C" fn(u64) -> u64>(name)
                    .unwrap();
                for x in [0u64, 1, 71, u64::MAX] {
                    let expected = match kind {
                        0 => x.wrapping_add(5),
                        1 => x.wrapping_mul(3),
                        2 => foreign(x),
                        _ => x ^ 91,
                    };
                    assert_eq!(function.call(x), expected);
                }
            }
            let indirect = engine
                .get_function::<unsafe extern "C" fn(unsafe extern "C" fn(u64) -> u64, u64) -> u64>(
                    "already_indirect",
                )
                .unwrap();
            assert_eq!(indirect.call(foreign, 19), foreign(19));
        }
    }

    #[test]
    fn layout_boundaries_are_accounted_without_host_width_fallback() {
        for (layout, reason) in [
            ("", "missing_data_layout"),
            ("e-p:16:16", "unsupported_pointer_width"),
            ("e-p:64:64:64:32", "pointer_index_width_mismatch"),
            ("e-P1", "nonzero_address_space"),
            ("e-G1", "nonzero_address_space"),
        ] {
            let context = Context::create();
            let ir = format!(
                r#"
                target datalayout = "{layout}"
                define private i32 @local() {{ ret i32 17 }}
                define i32 @entry() {{ %r = call i32 @local() ret i32 %r }}
            "#
            );
            let mut module = parse(&context, &ir);
            let mut counts = Counts::default();
            transform(
                &context,
                &mut module,
                &mut StdRng::seed_from_u64(0),
                Some(&mut counts),
            )
            .unwrap();
            assert_eq!(
                (counts.considered, counts.transformed, counts.skipped),
                (1, 0, 1)
            );
            assert_eq!(counts.skip_reasons.get(reason), Some(&1));
        }
        for layout in ["e-p:32:32", "e-p:64:64", "e-i64:64-n8:16:32:64"] {
            let context = Context::create();
            let ir = format!(
                r#"
                target datalayout = "{layout}"
                define private i32 @local() {{ ret i32 17 }}
                define i32 @entry() {{ %r = call i32 @local() ret i32 %r }}
            "#
            );
            let mut module = parse(&context, &ir);
            let mut counts = Counts::default();
            transform(
                &context,
                &mut module,
                &mut StdRng::seed_from_u64(0),
                Some(&mut counts),
            )
            .unwrap();
            assert_eq!(
                (counts.considered, counts.transformed, counts.skipped),
                (1, 1, 0)
            );
        }
    }
}
