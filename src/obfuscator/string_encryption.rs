// Copyright (c) 2025 Hakai Offensive Security.
//
// This program is free software: you can redistribute it and/or modify it
// under the terms of the GNU General Public License as published by the
// Free Software Foundation, version 3. See the LICENSE file for details.
//
// String Encryption Pass (asm-stub style)
//
// Encrypts all eligible constant byte-array string globals in the module
// using a per-byte random XOR key, then injects:
//   1. A shared decryptor function `__lyra_dec_strings_<rand>__(ptr, i32, ptr)`
//      which XORs the bytes in place using the key array.
//   2. A zero-argument stub `lyra_decrypt_strings_stub` that calls the
//      decryptor once for every encrypted global.
//   3. An entry in `llvm.global_ctors` pointing at the stub, so the
//      decryption runs once at program startup, before `main`.
//
// Design notes:
// - Both C-style `[N x i8]` globals and Rust/C++-style struct-wrapped
//   string globals (e.g. `alloc_XXX = private constant <{ [N x i8] }>`)
//   are handled.
// - Globals are converted from `constant` to mutable, and LinkOnce/Weak
//   linkages are demoted to `Internal` with the COMDAT cleared so the
//   encrypted initializer isn't undone by another TU at link time.
// - No `.str`-name filter is applied: Rust globals are named `alloc_XXX`.
// - No thread-safety flag is used; the ctor runs once before `main`.
//
// Modelled after the `amice` project's global-timing XOR pass, simplified
// for the lyra "old asm-stub" architecture.

use anyhow::{Context as _, Result};
use inkwell::AddressSpace;
use inkwell::comdat::Comdat;
use inkwell::context::Context;
use inkwell::module::{Linkage, Module};
use inkwell::values::{
    ArrayValue, AsValueRef, BasicValueEnum, FunctionValue, GlobalValue,
    UnnamedAddress,
};
use inkwell::GlobalVisibility;
use rand::{Rng, RngCore};
use std::ptr::null_mut;

use crate::obfuscator::seed;

/// Record of a string global that has been encrypted and needs a decrypt
/// call emitted in the ctor stub.
struct EncryptedString<'ctx> {
    /// The mutated global (initializer now holds ciphertext).
    global: GlobalValue<'ctx>,
    /// Global holding the per-byte XOR key stream.
    key_global: GlobalValue<'ctx>,
    /// Number of bytes in the string (ciphertext length).
    len: u32,
    /// If the string lives inside a struct, the field index. `None` means
    /// the global *is* a bare `[N x i8]` array.
    field_idx: Option<u32>,
}

pub fn apply<'ctx>(context: &'ctx Context, module: &mut Module<'ctx>) -> Result<()> {
    println!("[*] Applying String Encryption...");

    let mut rng = seed::for_pass(
        "string_encryption",
        module.get_name().to_str().unwrap_or("module"),
        module.get_source_file_name().to_str().unwrap_or(""),
    );
    let mut encrypted: Vec<EncryptedString<'ctx>> = Vec::new();
    let mut array_count = 0usize;
    let mut struct_count = 0usize;

    // Snapshot globals up front - we will be adding new ones (key arrays)
    // and don't want them re-entered into the iteration.
    let all_globals: Vec<GlobalValue<'ctx>> = module.get_globals().collect();

    for global in all_globals {
        if !is_candidate_global(&global) {
            continue;
        }

        let Some(init) = global.get_initializer() else {
            continue;
        };

        match init {
            // C-style: the initializer IS a const byte array
            BasicValueEnum::ArrayValue(arr) => {
                if let Some(bytes) = string_bytes(&arr) {
                    if bytes.len() > 1 {
                        if let Some(rec) = encrypt_array_global(
                            context,
                            module,
                            global,
                            &arr,
                            bytes,
                            None,
                            &mut rng,
                        )? {
                            encrypted.push(rec);
                            array_count += 1;
                        }
                    }
                }
            }
            // Struct-wrapped (Rust / C++ NTTP): scan every field for
            // const string arrays, encrypt each, then rebuild the struct
            // initializer in one pass.
            BasicValueEnum::StructValue(stru) => {
                let mut field_records: Vec<(u32, Vec<u8>, Vec<u8>, ArrayValue<'ctx>)> = Vec::new();

                for i in 0..stru.count_fields() {
                    let Some(field) = stru.get_field_at_index(i) else {
                        continue;
                    };
                    let BasicValueEnum::ArrayValue(arr) = field else {
                        continue;
                    };
                    let Some(bytes) = string_bytes(&arr) else {
                        continue;
                    };
                    if bytes.len() <= 1 {
                        continue;
                    }

                    // Generate per-byte key and ciphertext
                    let key: Vec<u8> = (0..bytes.len())
                        .map(|_| {
                            let mut b = [0u8; 1];
                            rng.fill_bytes(&mut b);
                            if b[0] == 0 { 0xA5 } else { b[0] }
                        })
                        .collect();
                    let mut ciphertext = bytes.to_vec();
                    for (c, k) in ciphertext.iter_mut().zip(key.iter()) {
                        *c ^= *k;
                    }
                    field_records.push((i, ciphertext, key, arr));
                }

                if field_records.is_empty() {
                    continue;
                }

                // Rebuild the struct initializer with ciphertext arrays in
                // place of the plaintext array fields.
                let mut new_fields: Vec<BasicValueEnum<'ctx>> = stru.get_fields().collect();
                for (idx, ciphertext, _key, _arr) in &field_records {
                    let new_arr = context.const_string(ciphertext, false);
                    new_fields[*idx as usize] = BasicValueEnum::ArrayValue(new_arr);
                }
                let new_init = stru.get_type().const_named_struct(&new_fields);
                global.set_initializer(&new_init);

                // Demote linkage / unnamed_addr so the init survives linking
                demote_for_write(&global);

                // Emit key globals and encryption records
                for (idx, _ciphertext, key, arr) in field_records {
                    let len = arr.get_type().len() as u32;
                    let key_global = create_key_global(context, module, &global, idx, &key);
                    encrypted.push(EncryptedString {
                        global,
                        key_global,
                        len,
                        field_idx: Some(idx),
                    });
                    struct_count += 1;
                }
            }
            _ => {}
        }
    }

    if encrypted.is_empty() {
        println!("        (no eligible string globals found)");
        return Ok(());
    }

    println!(
        "        ✓ Encrypted {} string(s) ({} array, {} struct-wrapped)",
        encrypted.len(),
        array_count,
        struct_count
    );

    // Shared decryptor: void decrypt(i8* ptr, i32 len, i8* key)
    let dec_name = format!("__lyra_dec_strings_{:08x}__", rng.random::<u32>());
    let decrypt_fn = build_decrypt_function(context, module, &dec_name)?;

    // Stub: void lyra_decrypt_strings_stub() that calls decrypt(...) for each
    let stub_name = format!("lyra_decrypt_strings_stub_{:08x}", rng.random::<u32>());
    let stub_fn = build_ctor_stub(context, module, &stub_name, decrypt_fn, &encrypted)?;

    // Append into @llvm.global_ctors (priority 0 => runs very early)
    append_to_global_ctors(context, module, stub_fn, 0)?;

    Ok(())
}

// -----------------------------------------------------------------------------
// Candidate filtering
// -----------------------------------------------------------------------------

fn is_candidate_global<'ctx>(g: &GlobalValue<'ctx>) -> bool {
    // Skip declarations
    if g.get_initializer().is_none() {
        return false;
    }

    // Skip external linkage
    if matches!(g.get_linkage(), Linkage::External) {
        return false;
    }

    // Skip things in llvm.metadata section (debug / stackmaps / etc.)
    if let Some(sec) = g.get_section() {
        if sec.to_str() == Ok("llvm.metadata") {
            return false;
        }
    }

    // Skip specials
    if let Ok(name) = g.get_name().to_str() {
        if name.starts_with("llvm.") || name.starts_with("__llvm_") {
            return false;
        }
        // Skip our own key globals from prior runs
        if name.starts_with("lyra_str_key") || name.starts_with("__lyra_") || name.starts_with("lyra_decrypt_") {
            return false;
        }
    }

    true
}

fn string_bytes<'a>(arr: &'a ArrayValue<'a>) -> Option<&'a [u8]> {
    if arr.is_null() || arr.is_undef() {
        return None;
    }
    // IMPORTANT: LLVM's `LLVMIsConstantString` asserts internally that the
    // element type is i8 AND that the value is a ConstantDataSequential.
    // If we blindly call it on `[1 x ptr]`, on `[11 x i8]` ConstantArrays
    // (with poison), or similar, we hit:
    //     isa<To>(Val) && "cast<Ty>() argument of incompatible type!"
    // Guard on both the element type and on `LLVMIsAConstantDataArray`.
    let elem_ty = arr.get_type().get_element_type();
    if !elem_ty.is_int_type() {
        return None;
    }
    let int_ty = elem_ty.into_int_type();
    if int_ty.get_bit_width() != 8 {
        return None;
    }
    unsafe {
        use llvm_sys::core::LLVMIsAConstantDataArray;
        if LLVMIsAConstantDataArray(arr.as_value_ref()).is_null() {
            return None;
        }
    }
    if !arr.is_const_string() {
        return None;
    }
    arr.as_const_string()
}

// -----------------------------------------------------------------------------
// Encryption helpers
// -----------------------------------------------------------------------------

fn demote_for_write<'ctx>(g: &GlobalValue<'ctx>) {
    // Make the storage writable so the decryptor can patch it in place.
    g.set_constant(false);
    // Prevent LLVM from merging identical globals (our keys differ!).
    g.set_unnamed_address(UnnamedAddress::None);

    let link = g.get_linkage();
    if matches!(
        link,
        Linkage::LinkOnceAny
            | Linkage::LinkOnceODR
            | Linkage::WeakAny
            | Linkage::WeakODR
            | Linkage::Common
    ) {
        g.set_linkage(Linkage::Internal);
        // Clear any COMDAT so linker doesn't deduplicate encrypted copies
        g.set_comdat(unsafe { Comdat::new(null_mut()) });
        g.set_visibility(GlobalVisibility::Default);
    }
}

fn create_key_global<'ctx>(
    context: &'ctx Context,
    module: &mut Module<'ctx>,
    for_global: &GlobalValue<'ctx>,
    field_idx: u32,
    key: &[u8],
) -> GlobalValue<'ctx> {
    let base = for_global
        .get_name()
        .to_str()
        .ok()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("anon_{:x}", for_global.as_value_ref() as usize));

    // Sanitize name: LLVM tolerates most chars but dots/dollar-signs are
    // safer to keep and '.' is conventional in Rust mangled names.
    let key_name = format!("lyra_str_key.{base}.{field_idx}");

    let i8_ty = context.i8_type();
    let arr_ty = i8_ty.array_type(key.len() as u32);
    let key_global = module.add_global(arr_ty, None, &key_name);

    let key_vals: Vec<_> = key
        .iter()
        .map(|b| i8_ty.const_int(*b as u64, false))
        .collect();
    let init = i8_ty.const_array(&key_vals);
    key_global.set_initializer(&init);
    key_global.set_linkage(Linkage::Private);
    key_global.set_constant(true);
    key_global.set_unnamed_address(UnnamedAddress::None);

    key_global
}

fn encrypt_array_global<'ctx>(
    context: &'ctx Context,
    module: &mut Module<'ctx>,
    global: GlobalValue<'ctx>,
    arr: &ArrayValue<'ctx>,
    bytes: &[u8],
    field_idx: Option<u32>,
    rng: &mut impl Rng,
) -> Result<Option<EncryptedString<'ctx>>> {
    let len = arr.get_type().len() as u32;
    if len as usize != bytes.len() {
        // Shouldn't happen, but guard anyway.
        return Ok(None);
    }

    // Per-byte random key (avoiding zero so no-op XOR doesn't leak plaintext)
    let key: Vec<u8> = (0..bytes.len())
        .map(|_| {
            let mut b = [0u8; 1];
            rng.fill_bytes(&mut b);
            if b[0] == 0 { 0xA5 } else { b[0] }
        })
        .collect();
    let mut ciphertext = bytes.to_vec();
    for (c, k) in ciphertext.iter_mut().zip(key.iter()) {
        *c ^= *k;
    }

    // Overwrite the global's initializer with the ciphertext.
    let new_arr = context.const_string(&ciphertext, false);
    global.set_initializer(&new_arr);
    demote_for_write(&global);

    let key_global = create_key_global(context, module, &global, 0, &key);

    Ok(Some(EncryptedString {
        global,
        key_global,
        len,
        field_idx,
    }))
}

// -----------------------------------------------------------------------------
// Runtime code: decrypt function + ctor stub + llvm.global_ctors append
// -----------------------------------------------------------------------------

fn build_decrypt_function<'ctx>(
    context: &'ctx Context,
    module: &mut Module<'ctx>,
    name: &str,
) -> Result<FunctionValue<'ctx>> {
    let i8_ty = context.i8_type();
    let i32_ty = context.i32_type();
    let ptr_ty = context.ptr_type(AddressSpace::default());

    let fn_ty = context
        .void_type()
        .fn_type(&[ptr_ty.into(), i32_ty.into(), ptr_ty.into()], false);
    let f = module.add_function(name, fn_ty, None);
    f.set_linkage(Linkage::Internal);

    let entry = context.append_basic_block(f, "entry");
    let loop_head = context.append_basic_block(f, "loop_head");
    let loop_body = context.append_basic_block(f, "loop_body");
    let loop_exit = context.append_basic_block(f, "loop_exit");

    let builder = context.create_builder();

    // entry:
    //   %idx = alloca i32
    //   store i32 0, i32* %idx
    //   br label %loop_head
    builder.position_at_end(entry);
    let idx_ptr = builder
        .build_alloca(i32_ty, "idx")
        .context("alloca idx failed")?;
    builder
        .build_store(idx_ptr, i32_ty.const_zero())
        .context("store 0 failed")?;
    builder
        .build_unconditional_branch(loop_head)
        .context("br loop_head failed")?;

    // loop_head:
    //   %cur = load i32, i32* %idx
    //   %cond = icmp ult i32 %cur, %len
    //   br i1 %cond, label %loop_body, label %loop_exit
    builder.position_at_end(loop_head);
    let cur = builder
        .build_load(i32_ty, idx_ptr, "cur")
        .context("load idx failed")?
        .into_int_value();
    let len_param = f.get_nth_param(1).unwrap().into_int_value();
    let cond = builder
        .build_int_compare(inkwell::IntPredicate::ULT, cur, len_param, "cond")
        .context("icmp failed")?;
    builder
        .build_conditional_branch(cond, loop_body, loop_exit)
        .context("br cond failed")?;

    // loop_body:
    //   %src_p = gep i8, i8* %ptr, i32 %cur
    //   %ch = load i8, i8* %src_p
    //   %key_p = gep i8, i8* %key, i32 %cur
    //   %k = load i8, i8* %key_p
    //   %x = xor i8 %ch, %k
    //   store i8 %x, i8* %src_p
    //   %nxt = add i32 %cur, 1
    //   store i32 %nxt, i32* %idx
    //   br label %loop_head
    builder.position_at_end(loop_body);
    let ptr_param = f.get_nth_param(0).unwrap().into_pointer_value();
    let key_param = f.get_nth_param(2).unwrap().into_pointer_value();

    let src_gep = unsafe {
        builder
            .build_gep(i8_ty, ptr_param, &[cur], "src_p")
            .context("gep src failed")?
    };
    let ch = builder
        .build_load(i8_ty, src_gep, "ch")
        .context("load ch failed")?
        .into_int_value();

    let key_gep = unsafe {
        builder
            .build_gep(i8_ty, key_param, &[cur], "key_p")
            .context("gep key failed")?
    };
    let k = builder
        .build_load(i8_ty, key_gep, "k")
        .context("load k failed")?
        .into_int_value();

    let x = builder.build_xor(ch, k, "x").context("xor failed")?;
    builder
        .build_store(src_gep, x)
        .context("store x failed")?;

    let one = i32_ty.const_int(1, false);
    let nxt = builder
        .build_int_add(cur, one, "nxt")
        .context("add 1 failed")?;
    builder
        .build_store(idx_ptr, nxt)
        .context("store nxt failed")?;
    builder
        .build_unconditional_branch(loop_head)
        .context("br loop_head 2 failed")?;

    // loop_exit: ret void
    builder.position_at_end(loop_exit);
    builder
        .build_return(None)
        .context("ret void failed")?;

    Ok(f)
}

fn build_ctor_stub<'ctx>(
    context: &'ctx Context,
    module: &mut Module<'ctx>,
    name: &str,
    decrypt_fn: FunctionValue<'ctx>,
    strings: &[EncryptedString<'ctx>],
) -> Result<FunctionValue<'ctx>> {
    let i32_ty = context.i32_type();
    let stub_ty = context.void_type().fn_type(&[], false);
    let stub = module.add_function(name, stub_ty, None);
    stub.set_linkage(Linkage::Internal);

    let entry = context.append_basic_block(stub, "entry");
    let builder = context.create_builder();
    builder.position_at_end(entry);

    for s in strings {
        // Resolve data pointer: either `@global` (array global) or
        // `getelementptr %Struct, ptr @global, i32 0, i32 field_idx`.
        let data_ptr = if let Some(field_idx) = s.field_idx {
            // Build struct GEP
            let gp = s.global.as_pointer_value();
            let struct_ty = match s.global.get_value_type() {
                inkwell::types::AnyTypeEnum::StructType(st) => st,
                other => {
                    anyhow::bail!(
                        "expected struct global type for field access but got {:?}",
                        other
                    )
                }
            };
            builder
                .build_struct_gep(struct_ty, gp, field_idx, "field_gep")
                .map_err(|e| anyhow::anyhow!("struct_gep failed: {e:?}"))?
        } else {
            s.global.as_pointer_value()
        };

        let key_ptr = s.key_global.as_pointer_value();
        let len_val = i32_ty.const_int(s.len as u64, false);

        builder
            .build_call(
                decrypt_fn,
                &[data_ptr.into(), len_val.into(), key_ptr.into()],
                "",
            )
            .map_err(|e| anyhow::anyhow!("build_call failed: {e:?}"))?;
    }

    builder
        .build_return(None)
        .map_err(|e| anyhow::anyhow!("ret failed: {e:?}"))?;

    Ok(stub)
}

/// Append a ctor entry into `llvm.global_ctors`. If the variable already
/// exists, its existing initializer entries are preserved; otherwise a new
/// appending-linkage global is created.
fn append_to_global_ctors<'ctx>(
    context: &'ctx Context,
    module: &mut Module<'ctx>,
    function: FunctionValue<'ctx>,
    priority: i32,
) -> Result<()> {
    let i32_ty = context.i32_type();
    let ptr_ty = context.ptr_type(AddressSpace::default());

    // struct { i32, ptr, ptr }
    let ctor_struct_ty = context.struct_type(&[i32_ty.into(), ptr_ty.into(), ptr_ty.into()], false);

    let fn_ptr = function.as_global_value().as_pointer_value();

    // Build the new entry: { i32 <priority>, ptr @function, ptr null }
    let new_entry = ctor_struct_ty.const_named_struct(&[
        i32_ty.const_int(priority as u64, true).into(),
        fn_ptr.into(),
        ptr_ty.const_null().into(),
    ]);

    // Collect existing entries if @llvm.global_ctors is already present.
    let mut entries: Vec<inkwell::values::StructValue<'ctx>> = Vec::new();
    let existing = module.get_global("llvm.global_ctors");

    if let Some(existing_gv) = existing {
        if let Some(init) = existing_gv.get_initializer() {
            if let BasicValueEnum::ArrayValue(arr) = init {
                // We have to extract each struct element. inkwell doesn't
                // expose `LLVMGetAggregateElement` directly, but the
                // ArrayValue type we can at least count via its array type
                // length.
                let count = arr.get_type().len();
                // Use LLVM C API to grab each element
                use llvm_sys::core::LLVMGetAggregateElement;
                for i in 0..count {
                    let raw = unsafe { LLVMGetAggregateElement(arr.as_value_ref(), i) };
                    if raw.is_null() {
                        continue;
                    }
                    // Each element is a { i32, ptr, ptr } struct value.
                    let sv = unsafe { inkwell::values::StructValue::new(raw) };
                    entries.push(sv);
                }
            }
        }
        // Remove the existing global so we can recreate with a bigger array.
        unsafe {
            use llvm_sys::core::LLVMDeleteGlobal;
            LLVMDeleteGlobal(existing_gv.as_value_ref());
        }
    }

    entries.push(new_entry);

    // Build the new array initializer
    let arr_ty = ctor_struct_ty.array_type(entries.len() as u32);
    let arr_init = ctor_struct_ty.const_array(&entries);

    // Sanity: recreate the global
    let new_global = module.add_global(arr_ty, None, "llvm.global_ctors");
    new_global.set_linkage(Linkage::Appending);
    new_global.set_initializer(&arr_init);

    Ok(())
}
