// Copyright (c) 2025 Hakai Offensive Security.
// SPDX-License-Identifier: GPL-3.0-or-later

//! Bounded outlining of original, acyclic, call-free computation. LLVM's
//! CodeExtractor owns the SSA/PHI and live-out stack-slot rewrite. The native
//! shim rejects frame-sensitive functions and unsupported boundary types; it
//! never adds freeze or parameter promises such as noundef. A helper is a
//! surviving call boundary, not a claim that its computation becomes secret.

use anyhow::{ensure, Result};
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::values::AsValueRef;
use llvm_sys::prelude::LLVMValueRef;
use rand::Rng;

use super::coverage::{self, Counts};
use super::seed;
use super::util::should_skip_function;

const REASONS: [&str; 11] = [
    "unsafe_function",
    "entry_block",
    "unreachable_block",
    "cyclic_block",
    "unsupported_terminator",
    "unsupported_region_instruction",
    "region_too_small",
    "unsupported_boundary_type",
    "boundary_budget",
    "extractor_ineligible",
    "selection_budget",
];

// Mirrored by OutlineStats in block_outline.cpp; fixed-width integers avoid a
// host size_t/usize dependency in the only native ABI exposed by this pass.
#[repr(C)]
#[derive(Default)]
struct OutlineStats {
    eligible: u64,
    skipped: [u64; REASONS.len()],
}

unsafe extern "C" {
    fn lyra_outline_function(function: LLVMValueRef, seed: u64, stats: *mut OutlineStats) -> i32;
}

pub fn apply<'ctx>(_context: &'ctx Context, module: &mut Module<'ctx>) -> Result<()> {
    println!("[*] Applying Block Outline...");
    let mut rng = seed::for_pass(
        "block_outline",
        module.get_name().to_str().unwrap_or("module"),
        module.get_source_file_name().to_str().unwrap_or(""),
    );
    let mut counts = coverage::enabled().then(Counts::default);
    let result = transform(module, &mut rng, counts.as_mut());
    if let Some(counts) = counts.as_mut() {
        if result.is_err() {
            counts.skip(
                "transformation_error",
                counts.considered - counts.transformed - counts.skipped,
            );
        }
        counts.emit(
            "block-outline",
            "basic_blocks",
            module.get_name().to_str().unwrap_or("module"),
            if result.is_ok() {
                "\"complete\":true,"
            } else {
                "\"complete\":false,"
            },
        );
    }
    let transformed = result?;
    println!("        ✓ Outlined {transformed} basic block(s)");
    Ok(())
}

fn transform(
    module: &mut Module<'_>,
    rng: &mut impl Rng,
    mut counts: Option<&mut Counts>,
) -> Result<usize> {
    // Extraction appends helpers but never erases originals. Capture the
    // original end once and each next pointer before extraction: no module-size
    // snapshot allocation and no recursively outlined generated helpers.
    let last_original = module.get_last_function();
    let mut current = module.get_first_function();
    let mut transformed = 0;
    while let Some(function) = current {
        let next = function.get_next_function();
        let blocks = function.count_basic_blocks() as usize;
        if let Some(counts) = counts.as_deref_mut() {
            counts.considered += blocks;
        }
        if should_skip_function(&function) {
            if let Some(counts) = counts.as_deref_mut() {
                counts.skip("function_exclusion", blocks);
            }
        } else {
            let mut stats = OutlineStats::default();
            let stats_ptr = if counts.is_some() {
                &mut stats as *mut OutlineStats
            } else {
                std::ptr::null_mut()
            };
            // The module/context outlive the call; no Rust instruction/block
            // wrappers survive the native SSA mutation. A failure is fatal,
            // never reported as a successful unobfuscated fallback.
            let result =
                unsafe { lyra_outline_function(function.as_value_ref(), rng.random(), stats_ptr) };
            if let Some(counts) = counts.as_deref_mut() {
                counts.eligible += stats.eligible as usize;
                for (reason, skipped) in REASONS.iter().zip(stats.skipped) {
                    counts.skip(reason, skipped as usize);
                }
                if result == 1 {
                    counts.transformed += 1;
                }
            }
            ensure!(
                result == 0 || result == 1,
                "block-outline extraction failed in {}",
                function.get_name().to_string_lossy()
            );
            transformed += result as usize;
        }
        if Some(function) == last_original {
            break;
        }
        current = next;
    }
    module
        .verify()
        .map_err(|error| anyhow::anyhow!("block-outline verification failed: {error}"))?;
    Ok(transformed)
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
                "block-outline-test",
            ))
            .unwrap();
        module.verify().unwrap();
        module
    }

    #[test]
    fn native_phi_memory_boundaries_recursion_and_exclusions() {
        for level in [OptimizationLevel::None, OptimizationLevel::Aggressive] {
            for seed in [0, 1, 42] {
                let context = Context::create();
                let mut module = parse(&context, include_str!("../../tests/block_outlining.ir"));
                super::super::util::set_native_test_target(&module);
                // Establish that the fixture itself runs on this host before
                // attributing a native failure to the outlining pass.
                {
                    let baseline = module.clone();
                    let engine = baseline.create_jit_execution_engine(level).unwrap();
                    unsafe {
                        let main = engine
                            .get_function::<unsafe extern "C" fn() -> i32>("main")
                            .unwrap();
                        assert_eq!(main.call(), 0, "untransformed native fixture");
                    }
                }
                let original_blocks: usize = module
                    .get_functions()
                    .map(|f| f.count_basic_blocks() as usize)
                    .sum();
                let mut counts = Counts::default();
                assert_eq!(
                    transform(
                        &mut module,
                        &mut StdRng::seed_from_u64(seed),
                        Some(&mut counts),
                    )
                    .unwrap(),
                    4
                );
                // Bounds apply to originals, including cyclic/unreachable and
                // excluded-function blocks, never the helper's new CFG.
                assert_eq!(counts.considered, original_blocks);
                assert_eq!(counts.considered, counts.transformed + counts.skipped);
                assert_eq!(counts.eligible, 4);
                assert_eq!(counts.skip_reasons.get("cyclic_block"), Some(&3));
                let engine = module.create_jit_execution_engine(level).unwrap();
                unsafe {
                    let main = engine
                        .get_function::<unsafe extern "C" fn() -> i32>("main")
                        .unwrap();
                    assert_eq!(main.call(), 0, "seed {seed}, level {level:?}");
                    let memory = engine
                        .get_function::<unsafe extern "C" fn(i32, i32, bool, *mut i32) -> i64>(
                            "phi_memory",
                        )
                        .unwrap();
                    for initial in [i32::MIN, -10, 0, 10, i32::MAX] {
                        for input in [i32::MIN, -3, 0, i32::MAX] {
                            for mode in [0, 1, 2] {
                                for side in [false, true] {
                                    let mut cell = initial;
                                    let result = memory.call(input, mode, side, &mut cell);
                                    if mode == 2 {
                                        assert_eq!(result, if side { -111 } else { -222 });
                                        assert_eq!(cell, initial);
                                    } else {
                                        let sum =
                                            initial.wrapping_add(if mode == 0 { input } else { 7 });
                                        let mixed = sum ^ 85;
                                        assert_eq!(cell, mixed.wrapping_add(3));
                                        assert_eq!(
                                            result,
                                            if side {
                                                i64::from(mixed & 255) + 11
                                            } else {
                                                i64::from(sum)
                                            }
                                        );
                                    }
                                }
                            }
                        }
                    }
                    let floating = engine
                        .get_function::<unsafe extern "C" fn(f32, f64) -> f64>("floating")
                        .unwrap();
                    for (a, b) in [
                        (-0.0, -0.0),
                        (f32::MIN_POSITIVE, -1.0),
                        (f32::MAX, 0.5),
                        (f32::INFINITY, 3.0),
                        (f32::NEG_INFINITY, 2.0),
                    ] {
                        let expected = (((f64::from(a) * 2.0 + b) - 3.0) / 2.0 + 4.0) * 0.5 - 1.0;
                        assert_eq!(floating.call(a, b).to_bits(), expected.to_bits());
                    }
                    assert!(floating.call(f32::NAN, 0.0).is_nan());
                }
            }
        }
    }

    #[test]
    fn poison_live_in_can_be_masked_without_new_ub() {
        let ir = r#"
            define i32 @masked(i32 %x, i1 %choose) {
            entry:
                %maybe = add nsw i32 %x, 1
                br label %work
            work:
                %safe = select i1 %choose, i32 %maybe, i32 5
                %a = add i32 %safe, 1
                %b = add i32 %a, 2
                %c = add i32 %b, 3
                %d = add i32 %c, 4
                %e = add i32 %d, 5
                %f = add i32 %e, 6
                %r = add i32 %f, 7
                br label %done
            done:
                ret i32 %r
            }
            define i32 @masked_undef() {
                %r = call i32 @masked(i32 undef, i1 false)
                ret i32 %r
            }
        "#;
        for level in [OptimizationLevel::None, OptimizationLevel::Aggressive] {
            let context = Context::create();
            let mut module = parse(&context, ir);
            super::super::util::set_native_test_target(&module);
            assert_eq!(
                transform(&mut module, &mut StdRng::seed_from_u64(7), None).unwrap(),
                1
            );
            let engine = module.create_jit_execution_engine(level).unwrap();
            unsafe {
                let masked = engine
                    .get_function::<unsafe extern "C" fn(i32, bool) -> i32>("masked")
                    .unwrap();
                assert_eq!(masked.call(i32::MAX, false), 33);
                assert_eq!(masked.call(41, true), 70);
                let undef = engine
                    .get_function::<unsafe extern "C" fn() -> i32>("masked_undef")
                    .unwrap();
                assert_eq!(undef.call(), 33);
            }
        }
    }

    #[test]
    fn only_one_original_region_is_selected_per_function() {
        let ir = r#"
            define i32 @two_regions(i32 %x, ptr %p) {
            entry:
                br label %first
            first:
                %a1 = add i32 %x, 1
                %a2 = mul i32 %a1, 3
                %a3 = xor i32 %a2, 7
                %a4 = add i32 %a3, 5
                %a5 = xor i32 %a4, 11
                %a6 = sub i32 %a5, 2
                store volatile i32 %a6, ptr %p
                %a7 = add i32 %a6, 13
                br label %second
            second:
                %b1 = load volatile i32, ptr %p
                %b2 = add i32 %a7, %b1
                %b3 = xor i32 %b2, 17
                %b4 = sub i32 %b3, 19
                %b5 = mul i32 %b4, 23
                %b6 = add i32 %b5, 29
                store volatile i32 %b6, ptr %p
                %b7 = xor i32 %b6, 31
                br label %done
            done:
                ret i32 %b7
            }
        "#;
        for seed in [0, 1, 42] {
            let context = Context::create();
            let mut module = parse(&context, ir);
            super::super::util::set_native_test_target(&module);
            let mut counts = Counts::default();
            assert_eq!(
                transform(
                    &mut module,
                    &mut StdRng::seed_from_u64(seed),
                    Some(&mut counts)
                )
                .unwrap(),
                1
            );
            assert_eq!(
                (
                    counts.considered,
                    counts.eligible,
                    counts.transformed,
                    counts.skipped
                ),
                (4, 2, 1, 3)
            );
            assert_eq!(counts.skip_reasons.get("selection_budget"), Some(&1));
            let engine = module
                .create_jit_execution_engine(OptimizationLevel::Aggressive)
                .unwrap();
            unsafe {
                let run = engine
                    .get_function::<unsafe extern "C" fn(i32, *mut i32) -> i32>("two_regions")
                    .unwrap();
                for x in [i32::MIN, -1, 0, 17, i32::MAX] {
                    let a = ((x.wrapping_add(1).wrapping_mul(3) ^ 7).wrapping_add(5) ^ 11)
                        .wrapping_sub(2);
                    let b = ((a.wrapping_add(13).wrapping_add(a) ^ 17).wrapping_sub(19))
                        .wrapping_mul(23)
                        .wrapping_add(29);
                    let mut cell = 0;
                    assert_eq!(run.call(x, &mut cell), b ^ 31);
                    assert_eq!(cell, b);
                }
            }
        }
    }

    #[test]
    fn byval_copy_keeps_caller_storage_and_aliasing() {
        let ir = r#"
            define internal i64 @copy(ptr byval(i64) align 8 %p) {
            entry:
                br label %work
            work:
                %v = load volatile i64, ptr %p, align 8
                %a = add i64 %v, 1
                %b = add i64 %a, 2
                %c = add i64 %b, 3
                %d = add i64 %c, 4
                %e = add i64 %d, 5
                store volatile i64 %e, ptr %p, align 8
                %r = xor i64 %e, 31
                br label %done
            done:
                ret i64 %r
            }
            define i64 @observe(i64 %x) {
                %p = alloca i64, align 8
                store i64 %x, ptr %p, align 8
                %r = call i64 @copy(ptr byval(i64) align 8 %p)
                %unchanged = load volatile i64, ptr %p, align 8
                %answer = xor i64 %r, %unchanged
                ret i64 %answer
            }
        "#;
        let context = Context::create();
        let mut module = parse(&context, ir);
        super::super::util::set_native_test_target(&module);
        assert_eq!(
            transform(&mut module, &mut StdRng::seed_from_u64(42), None).unwrap(),
            1
        );
        let engine = module
            .create_jit_execution_engine(OptimizationLevel::Aggressive)
            .unwrap();
        unsafe {
            let observe = engine
                .get_function::<unsafe extern "C" fn(i64) -> i64>("observe")
                .unwrap();
            for x in [i64::MIN, -17, 0, i64::MAX] {
                assert_eq!(observe.call(x), (x.wrapping_add(15) ^ 31) ^ x);
            }
        }
    }

    #[test]
    fn rejects_unbounded_boundaries_and_sensitive_functions() {
        let ir = r#"
            declare i32 @twice() returns_twice
            declare ptr @llvm.coro.noop()
            define i32 @inputs(i32 %a, i32 %b, i32 %c, i32 %d, i32 %e, i32 %f, i32 %g) {
            entry:
                br label %work
            work:
                %v1 = add i32 %a, %b
                %v2 = add i32 %v1, %c
                %v3 = add i32 %v2, %d
                %v4 = add i32 %v3, %e
                %v5 = add i32 %v4, %f
                %v6 = add i32 %v5, %g
                %v7 = xor i32 %v6, 17
                %v8 = mul i32 %v7, 3
                br label %done
            done:
                ret i32 %v8
            }
            define i32 @outputs(i32 %x) {
            entry:
                br label %work
            work:
                %a = add i32 %x, 1
                %b = mul i32 %a, 3
                %c = xor i32 %b, 7
                %d = add i32 %c, 5
                %e = xor i32 %d, 11
                %f = sub i32 %e, 2
                %g = add i32 %f, 13
                %h = mul i32 %g, 17
                br label %done
            done:
                %i = add i32 %f, %g
                %r = add i32 %i, %h
                ret i32 %r
            }
        "#;
        // Each generated region has eight real scalar instructions and one
        // output; only the boundary/function property under test disqualifies it.
        let work = r#"
            work:
                %a = add i32 %x, 1
                %b = mul i32 %a, 3
                %c = xor i32 %b, 7
                %d = add i32 %c, 5
                %e = xor i32 %d, 11
                %f = sub i32 %e, 2
                %g = add i32 %f, 13
                %h = mul i32 %g, 17
                br label %done
            done:
                ret i32 %h
        "#;
        let mut ir = ir.to_owned();
        for (name, signature, attributes, entry) in [
            ("returns_twice", "i32 %x", "", "%call = call i32 @twice()"),
            (
                "coroutine",
                "i32 %x",
                "",
                "%handle = call ptr @llvm.coro.noop()",
            ),
            ("gc", "i32 %x", "gc \"statepoint-example\"", ""),
            ("variadic", "i32 %x, ...", "", ""),
            ("convergent", "i32 %x", "convergent", ""),
        ] {
            ir.push_str(&format!(
                "define i32 @{name}({signature}) {attributes} {{\nentry:\n{entry}\nbr label %work\n{work}\n}}\n"
            ));
        }
        for (name, ty) in [
            ("fixed_vector", "<2 x i32>"),
            ("scalable_vector", "<vscale x 2 x i32>"),
        ] {
            ir.push_str(&format!(
                "define i32 @{name}({ty} %v) {{\nentry:\nbr label %work\nwork:\n%x = extractelement {ty} %v, i32 0\n{}\n}}\n",
                work.strip_prefix("\n            work:").unwrap()
            ));
        }
        ir.push_str(&format!(
            "define i32 @dead(i32 %x) {{\nentry:\nret i32 0\n{work}\n}}\n"
        ));
        let context = Context::create();
        let mut module = parse(&context, &ir);
        let mut counts = Counts::default();
        assert_eq!(
            transform(
                &mut module,
                &mut StdRng::seed_from_u64(0),
                Some(&mut counts)
            )
            .unwrap(),
            0
        );
        assert_eq!(counts.considered, counts.skipped);
        assert_eq!(counts.skip_reasons.get("boundary_budget"), Some(&2));
        assert_eq!(
            counts.skip_reasons.get("unsupported_boundary_type"),
            Some(&2)
        );
        assert_eq!(counts.skip_reasons.get("unreachable_block"), Some(&2));
        assert_eq!(counts.skip_reasons.get("function_exclusion"), Some(&3));
    }
}
