// Kernel (.kn) LLVM codegen -- v2 real native backend (Option B, RFC-003).
//
// Unlike codegen_c.rs (which emits -ffreestanding C for a bare-metal
// kernel booting under QEMU/real hardware, no OS, no libc, hand-rolled
// VGA text-mode print routines), this backend targets a REAL NATIVE
// EXECUTABLE for the host OS: a normal `main()`, linked against the
// system's real libc, printing through real `printf`. This is the
// "backend/server" half of the original roadmap, not another kernel --
// a genuinely different target, not just a faster way to build the same
// one.
//
// Honest v1.x scope for this backend (named gaps, not silent ones):
//   - Functions, structs, if/elif/else, while, for-in-range, return,
//     int/float/bool/str, borrows (&T / &mut T, same scope as
//     codegen_c.rs: field access through a BORROWED PARAMETER works,
//     field access through a locally `let`-bound borrow does not --
//     matching, not improving on, the C backend's existing limit).
//   - Fixed-size arrays (`[T;N]`): literals, indexing (read/write,
//     runtime-bounds-checked -- an out-of-range index calls real libc
//     `puts`+`exit(1)`, see gen_bounds_check/gen_panic, the native
//     twin of codegen_c.rs's kernel_panic halt), struct fields, and
//     function params (ALWAYS decay to a pointer, matching C/
//     codegen_c.rs exactly, since that's the only way C can pass one
//     either). Returning an array BY VALUE is still rejected (same
//     reason codegen_c.rs rejects it too: no sound by-value array
//     return in C's ABI model, and no caller-visible "borrow" to
//     return instead).
//   - String concatenation (`+`/`+=` on `str`): REAL heap allocation
//     via libc `malloc`/`strcpy`/`strcat` (see gen_concat) -- unlike
//     codegen_c.rs's kernel_strcat, which writes into a bounded
//     rotating pool of static buffers because there's no malloc/heap
//     at all in a -ffreestanding environment. Correct for any length,
//     no truncation, no "an older still-live result gets silently
//     overwritten" risk -- the real tradeoff the other way: nothing is
//     ever freed (no GC/arena in v1.x of EITHER backend), a named
//     memory-leak gap, not a silent one.
//   - `print` (via real libc printf), `abs` (manual select, no libm
//     dependency needed for something this simple), and `len()` (a
//     compile-time constant for arrays, real libc `strlen` for
//     strings).
//   - `read_file`/`write_file`: REAL file I/O via libc stdio
//     (fopen/fseek/ftell/rewind/fread/fwrite/fclose) -- this backend
//     targets a normal OS process with a real filesystem, unlike
//     codegen_c.rs's freestanding kernel target (no filesystem at all,
//     which is why that backend still rejects both outright; this is a
//     genuine backend-specific capability difference, not a cross-
//     backend disagreement, since the C backend's target literally
//     cannot have a filesystem). A failed `fopen` (bad path,
//     permissions) is a clear panic (see gen_null_check), never a
//     silent NULL dereference.
//   - NOT YET: `raw_write`/`unsafe` (meaningless here -- there's no
//     MMIO on a normal userspace process). REJECTED with a clear, named
//     error at codegen time rather than silently miscompiled or
//     silently dropped -- same "fail loud, name the gap" convention as
//     every other backend in this project.
//   - Global `static`/`const` items ARE supported (straightforward LLVM
//     global variables).
//   - Real short-circuit `&&`/`||` (via actual branching + a phi, not a
//     simple bitwise and/or) -- deliberately NOT simpler-than-necessary
//     here, since C's `&&`/`||` (what codegen_c.rs emits) really does
//     short-circuit, and backends silently disagreeing on an operator's
//     semantics is exactly the kind of gap this project's "backends must
//     agree" discipline exists to catch BEFORE it ships, not after.

use crate::ast::*;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::targets::{CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine};
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum, StructType};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, FunctionValue, PointerValue};
use inkwell::{AddressSpace, FloatPredicate, IntPredicate, OptimizationLevel};
use std::collections::HashMap;
use std::path::Path;

// Parses a Kernel array type string `[ElemType;N]` into (element Kernel
// type, N) -- the LLVM-backend twin of codegen_c.rs's array_info, same
// shape, operating on Kernel-level type strings directly rather than
// already-converted C ones. A single rfind(';') is sufficient and
// unambiguous for the same reason codegen_c.rs's array_info's is: v1.x
// has no nested arrays, so ElemType itself never contains a `;`.
fn kernel_array_info(ty: &str) -> Option<(String, i64)> {
    let trimmed = ty.trim();
    let rest = trimmed.strip_prefix('[')?;
    let rest = rest.strip_suffix(']')?;
    let semi = rest.rfind(';')?;
    let elem = rest[..semi].to_string();
    let n: i64 = rest[semi + 1..].parse().ok()?;
    Some((elem, n))
}

// Parses `Vec<ElemType>` -> `ElemType`. See Value::Vec's doc comment in
// interpreter.rs for the full design -- this is the LLVM-backend half
// of it (codegen only, scoped to primitive element types -- see
// vec_elem_byte_size's panic for structs/arrays/nested Vecs).
fn vec_elem_type(ty: &str) -> Option<String> {
    let trimmed = ty.trim();
    let rest = trimmed.strip_prefix("Vec<")?;
    let rest = rest.strip_suffix('>')?;
    Some(rest.trim().to_string())
}

struct FnCtx<'ctx> {
    function: FunctionValue<'ctx>,
    vars: HashMap<String, PointerValue<'ctx>>,
    // Kernel-level type strings ("int"/"float"/"str"/"bool"/"StructName"/
    // "&StructName"), the exact same role as codegen_c.rs's var_types --
    // deliberately NOT LLVM types, since field-type lookup, print
    // dispatch, etc. all need to reason about Kernel's own type space.
    var_types: HashMap<String, String>,
    // True only for a function PARAMETER declared `&T`/`&mut T` -- i.e.
    // its alloca holds a POINTER VALUE rather than the value directly.
    // Scope matches codegen_c.rs's is_ptr/accessor() exactly: a locally
    // `let`-bound borrow (`let r = &origin`) is never entered here, so
    // field access through it isn't supported, same named limit as the
    // C backend, not a new one.
    is_ptr: HashMap<String, bool>,
}

pub struct LlvmCodegen<'ctx> {
    context: &'ctx Context,
    module: Module<'ctx>,
    builder: Builder<'ctx>,
    structs: std::collections::HashSet<String>,
    struct_fields: HashMap<String, Vec<(String, String)>>, // ordered, for GEP index lookup
    struct_types: HashMap<String, StructType<'ctx>>,
    functions: HashMap<String, FnDecl>,
    printf_fn: FunctionValue<'ctx>,
    // Top-level `static`/`const` items (includes desugared enum variants --
    // see parser.rs's parse_enum_desugared) are plain LLVM globals, not
    // locals -- this is their Kernel type, the global-scope twin of
    // FnCtx::var_types, consulted wherever a local lookup misses (see
    // var_addr/var_kernel_type below).
    global_types: HashMap<String, String>,
    // Real ABI-size queries for Vec<T>'s realloc sizing (vec_elem_byte_size)
    // -- needed for element types beyond the hand-known primitives
    // (structs, fixed-size arrays, nested Vecs). Owned by compile_to_object's
    // TargetMachine, created BEFORE codegen runs specifically so this is
    // available during it.
    target_data: inkwell::targets::TargetData,
}

impl<'ctx> LlvmCodegen<'ctx> {
    fn new(context: &'ctx Context, module_name: &str, target_data: inkwell::targets::TargetData) -> Self {
        let module = context.create_module(module_name);
        let builder = context.create_builder();
        let ptr_ty = context.ptr_type(AddressSpace::default());
        let i64_ty = context.i64_type();
        let i32_ty = context.i32_type();
        let printf_ty = i32_ty.fn_type(&[ptr_ty.into()], true);
        let printf_fn = module.add_function("printf", printf_ty, None);
        // Real libc, since this backend targets a normal userspace
        // process (unlike codegen_c.rs's -ffreestanding target, which
        // hand-rolls everything down to kernel_strcat/kernel_panic
        // because there's no libc at all) -- see gen_concat/
        // gen_bounds_check_or_panic for what each of these backs.
        module.add_function("strlen", i64_ty.fn_type(&[ptr_ty.into()], false), None);
        module.add_function("malloc", ptr_ty.fn_type(&[i64_ty.into()], false), None);
        // Vec<T>'s growth (vec_push) -- realloc(NULL, n) is well-defined
        // as equivalent to malloc(n), so this alone covers both the
        // initial allocation (cap 0 -> N) and every later regrowth,
        // without needing a separate malloc+memcpy+free sequence.
        module.add_function("realloc", ptr_ty.fn_type(&[ptr_ty.into(), i64_ty.into()], false), None);
        module.add_function("strcpy", ptr_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false), None);
        module.add_function("strcat", ptr_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false), None);
        module.add_function("puts", i32_ty.fn_type(&[ptr_ty.into()], false), None);
        module.add_function("exit", context.void_type().fn_type(&[i32_ty.into()], false), None);
        // Real libc stdio, for read_file/write_file -- this backend
        // targets a normal OS process with a real filesystem, unlike
        // codegen_c.rs's freestanding kernel target (no filesystem at
        // all, hence that backend rejects both outright). See
        // gen_call's "read_file"/"write_file" cases.
        module.add_function("fopen", ptr_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false), None);
        module.add_function("fclose", i32_ty.fn_type(&[ptr_ty.into()], false), None);
        module.add_function("fseek", i32_ty.fn_type(&[ptr_ty.into(), i64_ty.into(), i32_ty.into()], false), None);
        module.add_function("ftell", i64_ty.fn_type(&[ptr_ty.into()], false), None);
        module.add_function("rewind", context.void_type().fn_type(&[ptr_ty.into()], false), None);
        module.add_function("fread", i64_ty.fn_type(&[ptr_ty.into(), i64_ty.into(), i64_ty.into(), ptr_ty.into()], false), None);
        module.add_function("fwrite", i64_ty.fn_type(&[ptr_ty.into(), i64_ty.into(), i64_ty.into(), ptr_ty.into()], false), None);
        LlvmCodegen {
            context,
            module,
            builder,
            structs: std::collections::HashSet::new(),
            struct_fields: HashMap::new(),
            struct_types: HashMap::new(),
            functions: HashMap::new(),
            printf_fn,
            global_types: HashMap::new(),
            target_data,
        }
    }

    fn ptr_type(&self) -> inkwell::types::PointerType<'ctx> {
        self.context.ptr_type(AddressSpace::default())
    }

    // Resolves an identifier's storage address, checking locals (params/
    // `let`s) first, then falling back to a module-level `static`/`const`
    // global (see global_types' doc comment) -- the single place that
    // fallback happens, so every caller (gen_expr/gen_addr/
    // struct_base_ptr) agrees on it instead of three separate ad hoc
    // lookups silently drifting apart.
    fn var_addr(&self, name: &str, ctx: &FnCtx<'ctx>) -> PointerValue<'ctx> {
        if let Some(addr) = ctx.vars.get(name) {
            return *addr;
        }
        self.module
            .get_global(name)
            .unwrap_or_else(|| panic!("Kernel LLVM codegen: unknown variable `{}`", name))
            .as_pointer_value()
    }

    fn var_kernel_type(&self, name: &str, ctx: &FnCtx<'ctx>) -> String {
        ctx.var_types
            .get(name)
            .or_else(|| self.global_types.get(name))
            .cloned()
            .unwrap_or_else(|| "int".to_string())
    }

    // Kernel type string -> LLVM type.
    fn llvm_type(&self, ty: &str) -> BasicTypeEnum<'ctx> {
        let trimmed = ty.trim_start_matches('&').trim();
        if let Some((elem_ty, n)) = kernel_array_info(trimmed) {
            return self.llvm_type(&elem_ty).array_type(n as u32).into();
        }
        if vec_elem_type(trimmed).is_some() {
            // Same universal {data ptr, len, cap} layout regardless of
            // the element type T -- LLVM pointers are opaque (no
            // per-pointee-type distinction at the type level since
            // LLVM 15+), so the struct's own shape never changes; only
            // element LOADS/STORES/GEPs (see gen_call's vec_* arms) need
            // T, via an explicit pointee type passed to each GEP.
            return self.vec_struct_type().into();
        }
        match trimmed {
            "int" => self.context.i64_type().into(),
            "float" => self.context.f64_type().into(),
            "bool" => self.context.bool_type().into(),
            "str" => self.ptr_type().into(),
            other if self.struct_types.contains_key(other) => self.struct_types[other].into(),
            other => panic!("Kernel LLVM codegen: unknown type `{}`", other),
        }
    }

    // `Vec<T>`'s universal runtime layout: { data: ptr, len: i64, cap: i64 }.
    fn vec_struct_type(&self) -> inkwell::types::StructType<'ctx> {
        self.context.struct_type(&[self.ptr_type().into(), self.context.i64_type().into(), self.context.i64_type().into()], false)
    }

    // Byte size of ONE element, for realloc's sizing in vec_push's
    // growth path. Primitives are hand-known (cheap, no query needed);
    // anything else (structs, fixed-size arrays, nested Vecs) goes
    // through a REAL ABI-size query against this backend's TargetData
    // -- the honest way to size an arbitrary type for realloc, rather
    // than guessing and silently corrupting the heap on a wrong size.
    fn vec_elem_byte_size(&self, ty: &str) -> u64 {
        match ty.trim() {
            "int" | "float" => 8,
            "bool" => 1,
            "str" => 8, // a pointer
            other => self.target_data.get_abi_size(&self.llvm_type(other)),
        }
    }

    // A function parameter/return's LLVM type: a borrowed type (&T) is
    // always a bare pointer regardless of what it points to, and a FIXED-
    // SIZE ARRAY ALWAYS decays to a bare pointer too (borrowed or not --
    // matching codegen_c.rs's "array parameters always decay to a
    // pointer in C no matter how they're declared" exactly, since C has
    // no other way to pass one either).
    fn llvm_type_for_decl(&self, ty: &str) -> BasicTypeEnum<'ctx> {
        let trimmed = ty.trim_start_matches('&').trim();
        if kernel_array_info(trimmed).is_some() || ty.starts_with('&') {
            self.ptr_type().into()
        } else {
            self.llvm_type(ty)
        }
    }

    fn struct_name_of<'a>(&self, ty: &'a str) -> Option<&'a str> {
        let trimmed = ty.trim_start_matches('&').trim();
        if self.structs.contains(trimmed) {
            Some(trimmed)
        } else {
            None
        }
    }

    fn field_index(&self, struct_name: &str, field: &str) -> u32 {
        self.struct_fields[struct_name]
            .iter()
            .position(|(n, _)| n == field)
            .unwrap_or_else(|| panic!("Kernel LLVM codegen: struct `{}` has no field `{}`", struct_name, field)) as u32
    }

    fn field_kernel_type(&self, struct_name: &str, field: &str) -> String {
        self.struct_fields[struct_name]
            .iter()
            .find(|(n, _)| n == field)
            .map(|(_, t)| t.clone())
            .unwrap_or_else(|| panic!("Kernel LLVM codegen: struct `{}` has no field `{}`", struct_name, field))
    }

    /// Infers an expression's KERNEL type string -- the LLVM-backend twin
    /// of codegen_c.rs's infer_c_type, same purpose, same fallback
    /// discipline (falls back to "int" only where inference genuinely has
    /// no better answer, never silently guessing for a type we actually
    /// know).
    fn infer_kernel_type(&self, e: &Expr, ctx: &FnCtx<'ctx>) -> String {
        match e {
            Expr::Int(_) => "int".to_string(),
            Expr::Float(_) => "float".to_string(),
            Expr::Str(_) => "str".to_string(),
            Expr::Bool(_) => "bool".to_string(),
            Expr::Ident(name) => ctx.var_types.get(name).or_else(|| self.global_types.get(name)).cloned().unwrap_or_else(|| "int".to_string()),
            Expr::Binary(l, BinOp::Add, r) => {
                let lt = self.infer_kernel_type(l, ctx);
                let rt = self.infer_kernel_type(r, ctx);
                if lt == "str" && rt == "str" {
                    "str".to_string()
                } else if lt == "float" || rt == "float" {
                    "float".to_string()
                } else {
                    "int".to_string()
                }
            }
            Expr::Binary(l, BinOp::Sub | BinOp::Mul | BinOp::Div, r) => {
                let lt = self.infer_kernel_type(l, ctx);
                let rt = self.infer_kernel_type(r, ctx);
                if lt == "float" || rt == "float" {
                    "float".to_string()
                } else {
                    "int".to_string()
                }
            }
            Expr::Binary(..) => "bool".to_string(),
            Expr::Unary(UnaryOp::Neg, inner) => self.infer_kernel_type(inner, ctx),
            Expr::Unary(UnaryOp::Not, _) => "bool".to_string(),
            Expr::Field(obj, field) => {
                let obj_ty = self.infer_kernel_type(obj, ctx);
                self.struct_name_of(&obj_ty)
                    .map(|s| self.field_kernel_type(s, field))
                    .unwrap_or_else(|| "int".to_string())
            }
            Expr::Borrow(inner, _) => format!("&{}", self.infer_kernel_type(inner, ctx)),
            Expr::Call { name, args } => {
                if name == "abs" {
                    return args.first().map(|a| self.infer_kernel_type(arg_expr(a), ctx)).unwrap_or_else(|| "int".to_string());
                }
                if name == "read_file" {
                    // A builtin, not a user Function -- doesn't go
                    // through the `self.functions.get(name)` lookup
                    // below at all, so it needs its own case here or it
                    // silently falls through to the "int" default
                    // (caught for real: `len(read_file(...))` crashed
                    // with "len() only supports arrays and strings...
                    // found a `int`" the first time this builtin's
                    // result was ever fed into another type-sensitive
                    // call, not just printed directly).
                    return "str".to_string();
                }
                if name == "vec_new" {
                    panic!(
                        "Kernel LLVM codegen: vec_new() can only appear directly as a `let`'s value (with an \
                         explicit `: Vec<ElemType>` annotation) -- found it elsewhere, where there's no way to \
                         know the element type"
                    );
                }
                if name == "vec_len" {
                    return "int".to_string();
                }
                if name == "vec_get" {
                    let vec_ty = args.first().map(|a| self.infer_kernel_type(arg_expr(a), ctx)).unwrap_or_else(|| "int".to_string());
                    let vec_ty = vec_ty.trim_start_matches('&').to_string();
                    return vec_elem_type(&vec_ty).unwrap_or_else(|| "int".to_string());
                }
                if matches!(name.as_str(), "vec_push" | "vec_set") {
                    // Unit, in effect -- these are only ever used as a
                    // standalone Stmt::Expr (see gen_stmt), never as a
                    // value, so this is never actually read; "int" is a
                    // harmless placeholder, not a real guess being acted
                    // on by anything.
                    return "int".to_string();
                }
                if self.structs.contains(name) {
                    name.clone()
                } else if let Some(f) = self.functions.get(name) {
                    f.return_ty.clone().unwrap_or_else(|| "int".to_string())
                } else {
                    "int".to_string()
                }
            }
            Expr::ArrayLit(elems) => {
                let elem_ty = elems.first().map(|e| self.infer_kernel_type(e, ctx)).unwrap_or_else(|| "int".to_string());
                format!("[{};{}]", elem_ty, elems.len())
            }
            Expr::Index(arr, _) => {
                let arr_ty = self.infer_kernel_type(arr, ctx);
                kernel_array_info(&arr_ty).map(|(e, _)| e).unwrap_or_else(|| "int".to_string())
            }
            Expr::Range(..) => panic!("Kernel LLVM codegen: a range is only valid as a for-loop iterator"),
            Expr::Closure { .. } => unreachable!("Kernel LLVM codegen: Expr::Closure should always be erased by lambda_lift.rs before codegen runs"),
        }
    }

    fn generate(&mut self, program: &Program) {
        // Register struct tables first (opaque, so self-/mutually-referencing
        // field types resolve regardless of declaration order), THEN set
        // each struct's real body -- the standard two-pass pattern for any
        // LLVM frontend with named aggregate types.
        for item in &program.items {
            if let Item::Struct(s) = item {
                self.structs.insert(s.name.clone());
                self.struct_fields.insert(s.name.clone(), s.fields.clone());
                self.struct_types.insert(s.name.clone(), self.context.opaque_struct_type(&s.name));
            }
        }
        for item in &program.items {
            if let Item::Struct(s) = item {
                let field_types: Vec<BasicTypeEnum> = s.fields.iter().map(|(_, t)| self.llvm_type(t)).collect();
                self.struct_types[&s.name].set_body(&field_types, false);
            }
        }
        for item in &program.items {
            if let Item::Function(f) = item {
                self.functions.insert(f.name.clone(), f.clone());
            }
        }

        // Globals (static/const) -- plain LLVM global variables. Their
        // initializers must be constant expressions (int/float/bool/str
        // literals only in practice -- same subset the checker/interpreter
        // actually allow at the top level).
        for item in &program.items {
            match item {
                Item::Static { name, ty, value, .. } | Item::Const { name, ty, value, .. } => {
                    self.global_types.insert(name.clone(), ty.clone());
                    self.declare_global(name, ty, value);
                }
                _ => {}
            }
        }

        // Forward declarations, so call order / recursion never matters --
        // same reasoning as codegen_c.rs's two-pass (declare, then define).
        for item in &program.items {
            if let Item::Function(f) = item {
                self.declare_function(f);
            }
        }
        for item in &program.items {
            if let Item::Function(f) = item {
                self.define_function(f);
            }
        }
    }

    fn declare_global(&self, name: &str, ty: &str, value: &Expr) {
        let llvm_ty = self.llvm_type(ty);
        let global = self.module.add_global(llvm_ty, None, name);
        let init: BasicValueEnum = match value {
            Expr::Int(n) => self.context.i64_type().const_int(*n as u64, true).into(),
            Expr::Float(x) => self.context.f64_type().const_float(*x).into(),
            Expr::Bool(b) => self.context.bool_type().const_int(*b as u64, false).into(),
            Expr::Str(s) => {
                // Can't use builder.build_global_string_ptr here (it
                // builds a GEP INSTRUCTION, which needs an active
                // insertion point -- this runs at module scope, before
                // any function body exists to position the builder in,
                // which is exactly what crashed here with "UnsetPosition"
                // before this fix). Building the constant array global
                // directly, with no instruction involved, works at any
                // point and is exactly what Expr::Str needs INSIDE a
                // function body too, just routed through the builder
                // there for convenience since a position already exists.
                let bytes = self.context.const_string(s.as_bytes(), true);
                let str_global = self.module.add_global(bytes.get_type(), None, &format!("{}_data", name));
                str_global.set_initializer(&bytes);
                str_global.set_constant(true);
                str_global.as_pointer_value().into()
            }
            other => panic!(
                "Kernel LLVM codegen: global `{}`'s initializer must be a literal (int/float/bool/str), found {:?}",
                name, other
            ),
        };
        global.set_initializer(&init);
    }

    fn fn_name(&self, f: &FnDecl) -> String {
        // Unlike codegen_c.rs's `main` -> `kmain` rename (needed because a
        // freestanding kernel's REAL entry point is boot.s's `_start`,
        // which calls `kmain` after setting up its own stack), this
        // backend targets a normal OS process -- Kernel's `main` becomes
        // the program's real, actual `main`, the one the OS itself calls.
        f.name.clone()
    }

    fn declare_function(&self, f: &FnDecl) {
        let param_types: Vec<BasicMetadataTypeEnum> = f.params.iter().map(|p| self.param_llvm_type(p).into()).collect();
        let name = self.fn_name(f);
        if name == "main" {
            // Real C ABI main: always `i32 main(void)`, regardless of
            // whether Kernel's own main declares a return type -- see
            // define_function for how a declared return value becomes the
            // process's actual exit code.
            let fn_type = self.context.i32_type().fn_type(&[], false);
            self.module.add_function("main", fn_type, None);
            return;
        }
        if let Some(t) = &f.return_ty {
            if kernel_array_info(t.trim_start_matches('&')).is_some() {
                // Plain LLVM has no sound by-value array return any more
                // than plain C does (and no caller-visible "borrow" to
                // return either) -- same named v1.x gap as
                // codegen_c.rs's fn_signature, for the same reason.
                panic!(
                    "Kernel LLVM codegen: function `{}` returns an array type ({}) directly -- \
                     not supported; wrap it in a struct or take an output parameter instead",
                    f.name, t
                );
            }
        }
        let fn_type = match &f.return_ty {
            Some(t) => self.llvm_type_for_decl(t).fn_type(&param_types, false),
            None => self.context.void_type().fn_type(&param_types, false),
        };
        self.module.add_function(&name, fn_type, None);
    }

    fn param_llvm_type(&self, p: &Param) -> BasicTypeEnum<'ctx> {
        if p.borrowed || kernel_array_info(&p.ty).is_some() {
            self.ptr_type().into()
        } else {
            self.llvm_type(&p.ty)
        }
    }

    fn define_function(&self, f: &FnDecl) {
        let name = self.fn_name(f);
        let function = self.module.get_function(&name).unwrap();
        let entry = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);

        let mut ctx = FnCtx { function, vars: HashMap::new(), var_types: HashMap::new(), is_ptr: HashMap::new() };
        for (i, p) in f.params.iter().enumerate() {
            let llvm_param = function.get_nth_param(i as u32).unwrap();
            let alloca = self.builder.build_alloca(self.param_llvm_type(p), &p.name).unwrap();
            self.builder.build_store(alloca, llvm_param).unwrap();
            ctx.vars.insert(p.name.clone(), alloca);
            // Arrays decay to a pointer regardless of `&` (matching
            // codegen_c.rs) -- so an array param's Kernel type string is
            // stored UNPREFIXED (just "[int;3]", never "&[int;3]") even
            // though its underlying storage is just as pointer-typed as a
            // borrowed param's; is_ptr is what actually drives the
            // deref-on-access decision for both cases uniformly (see
            // array_base_ptr/struct_base_ptr).
            let is_array = kernel_array_info(&p.ty).is_some();
            ctx.var_types.insert(p.name.clone(), if p.borrowed && !is_array { format!("&{}", p.ty) } else { p.ty.clone() });
            ctx.is_ptr.insert(p.name.clone(), p.borrowed || is_array);
        }

        for stmt in &f.body {
            self.gen_stmt(stmt, &mut ctx);
        }

        if !self.block_terminated() {
            // Fell off the end of the body with no explicit `return` --
            // main gets an implicit exit code of 0 (matching the C `int
            // main(void) { ...; return 0; }` convention every one of
            // these Kernel examples implicitly relies on, same as
            // codegen_c.rs's kmain, which the kernel's boot.s simply never
            // returns from at all). A non-main void function just
            // `ret void`s. A non-main function with a DECLARED return
            // type falling off the end with no `return` is a real
            // Kernel-level bug the checker should have caught already
            // (see checker.rs) -- codegen isn't the place to re-diagnose
            // it, so this intentionally returns a zeroed value rather
            // than silently miscompiling into undefined IR.
            if name == "main" {
                self.builder.build_return(Some(&self.context.i32_type().const_int(0, false))).unwrap();
            } else {
                match &f.return_ty {
                    Some(t) => {
                        let zero = self.zero_value(t);
                        self.builder.build_return(Some(&zero)).unwrap();
                    }
                    None => {
                        self.builder.build_return(None).unwrap();
                    }
                }
            }
        }
    }

    fn zero_value(&self, ty: &str) -> BasicValueEnum<'ctx> {
        match self.llvm_type(ty) {
            BasicTypeEnum::IntType(t) => t.const_zero().into(),
            BasicTypeEnum::FloatType(t) => t.const_zero().into(),
            BasicTypeEnum::PointerType(t) => t.const_null().into(),
            BasicTypeEnum::StructType(t) => t.const_zero().into(),
            other => panic!("Kernel LLVM codegen: no zero value for {:?}", other),
        }
    }

    fn block_terminated(&self) -> bool {
        self.builder.get_insert_block().and_then(|b| b.get_terminator()).is_some()
    }

    // The ADDRESS of a "place" expression (an ident, or a field/index
    // chain rooted in one) -- used for FieldAssign/IndexAssign's target,
    // and Expr::Borrow (`&x` is exactly "the address of x").
    fn gen_addr(&self, e: &Expr, ctx: &FnCtx<'ctx>) -> PointerValue<'ctx> {
        match e {
            Expr::Ident(name) => self.var_addr(name, ctx),
            Expr::Field(obj, field) => {
                let obj_ty = self.infer_kernel_type(obj, ctx);
                let struct_name = self
                    .struct_name_of(&obj_ty)
                    .unwrap_or_else(|| panic!("Kernel LLVM codegen: field access on a non-struct type `{}`", obj_ty))
                    .to_string();
                let base_ptr = self.struct_base_ptr(obj, ctx);
                let idx = self.field_index(&struct_name, field);
                let struct_ty = self.struct_types[&struct_name];
                self.builder.build_struct_gep(struct_ty, base_ptr, idx, "fieldaddr").unwrap()
            }
            Expr::Index(arr, idx) => self.array_elem_addr(arr, idx, ctx).0,
            Expr::Borrow(inner, _) => self.gen_addr(inner, ctx),
            other => panic!(
                "Kernel LLVM codegen: `{:?}` isn't a supported place expression in v1.x (only identifiers, \
                 field accesses, and array indexing, rooted in one)",
                other
            ),
        }
    }

    // The address of the STRUCT a field-access expression's base refers
    // to -- handles the is_ptr dereference-vs-direct distinction exactly
    // like codegen_c.rs's accessor() picks "." vs "->". Checks BOTH
    // `is_ptr` (function parameters) AND the variable's own Kernel type
    // (`&...`) -- `is_ptr` alone misses a LOCALLY `let`-bound borrow
    // (`let r = &p; r.field`), which is never tracked there (see
    // FnCtx::is_ptr's doc comment) -- a real, previously-latent bug,
    // unreachable until the checker's NLL improvement let more programs
    // with locally-bound borrows through at all: `r`'s own alloca holds
    // a POINTER VALUE, so treating its address AS the struct directly
    // (without the load this now does) read one level of indirection
    // too shallow -- a garbage pointer bit-pattern, not just a wrong
    // value. Same fix shape as gen_expr's Expr::Ident case already
    // uses (see its own comment) -- this accessor just hadn't gotten
    // it yet.
    fn struct_base_ptr(&self, obj: &Expr, ctx: &FnCtx<'ctx>) -> PointerValue<'ctx> {
        match obj {
            Expr::Ident(name) => {
                let addr = self.var_addr(name, ctx);
                let needs_deref = *ctx.is_ptr.get(name).unwrap_or(&false) || self.var_kernel_type(name, ctx).starts_with('&');
                if needs_deref {
                    self.builder.build_load(self.ptr_type(), addr, "deref").unwrap().into_pointer_value()
                } else {
                    addr
                }
            }
            Expr::Field(..) => self.gen_addr(obj, ctx),
            Expr::Borrow(inner, _) => self.gen_addr(inner, ctx),
            other => panic!("Kernel LLVM codegen: `{:?}` isn't a supported struct base expression in v1.x", other),
        }
    }

    // The address of the FIRST ELEMENT an array expression's base refers
    // to -- the array-indexing twin of struct_base_ptr. A local array
    // variable's own alloca already IS this address (an array's first
    // element starts at its own base address); a decayed array PARAMETER
    // (always is_ptr -- see FnCtx::is_ptr's doc comment) needs one load
    // to get at the pointer value it was passed as, exactly like a
    // borrowed struct parameter does.
    fn array_base_ptr(&self, arr: &Expr, ctx: &FnCtx<'ctx>) -> PointerValue<'ctx> {
        match arr {
            Expr::Ident(name) => {
                let addr = self.var_addr(name, ctx);
                // Same is_ptr-alone-misses-a-local-borrow fix as
                // struct_base_ptr above, for `let r = &arr; r[i]`.
                let needs_deref = *ctx.is_ptr.get(name).unwrap_or(&false) || self.var_kernel_type(name, ctx).starts_with('&');
                if needs_deref {
                    self.builder.build_load(self.ptr_type(), addr, "arrderef").unwrap().into_pointer_value()
                } else {
                    addr
                }
            }
            Expr::Field(..) => self.gen_addr(arr, ctx), // a struct field that's itself an array -- its field address IS its base pointer
            Expr::Borrow(inner, _) => self.gen_addr(inner, ctx),
            other => panic!("Kernel LLVM codegen: `{:?}` isn't a supported array base expression in v1.x", other),
        }
    }

    // Computes the bounds-checked ADDRESS of `arr[idx]`, plus the
    // element's Kernel type -- shared by Expr::Index (read) and
    // Stmt::IndexAssign (write) so the bounds check can never be
    // forgotten on one path but not the other. An out-of-range index
    // calls `kernel_panic_native` (puts + exit(1)) and never falls
    // through -- see this function's `unreachable` terminator -- matching
    // codegen_c.rs's kernel_panic halt-on-OOB behavior, just via real
    // libc instead of a hand-rolled freestanding routine.
    fn array_elem_addr(&self, arr: &Expr, idx: &Expr, ctx: &FnCtx<'ctx>) -> (PointerValue<'ctx>, String) {
        let arr_ty = self.infer_kernel_type(arr, ctx);
        let (elem_ty, n) = kernel_array_info(&arr_ty).unwrap_or_else(|| {
            panic!(
                "Kernel LLVM codegen: couldn't determine the array size for `[...]` to emit a bounds \
                 check -- this indicates inference failed upstream (see infer_kernel_type), a real bug, \
                 not a silent skip"
            )
        });
        let base_ptr = self.array_base_ptr(arr, ctx);
        let idx_val = self.gen_expr(idx, ctx).into_int_value();
        let checked_idx = self.gen_bounds_check(idx_val, n, ctx);
        let elem_llvm_ty = self.llvm_type(&elem_ty);
        let addr = unsafe { self.builder.build_gep(elem_llvm_ty, base_ptr, &[checked_idx], "idxaddr").unwrap() };
        (addr, elem_ty)
    }

    // Resolves a vec_push/vec_get/vec_len/vec_set call's first argument
    // to (the Vec variable's storage ADDRESS, its element Kernel type)
    // -- always via var_addr directly (never gen_expr/gen_addr's normal
    // Borrow-dependent logic), because mutation needs the real address
    // of the variable's OWN storage regardless of whether the caller
    // wrote an explicit `&mut` or not (both are accepted -- see
    // interpreter.rs's identical design/reasoning in eval_call's
    // "vec_push" | "vec_set" arm).
    fn vec_name_and_elem_ty(&self, e: &Expr, ctx: &FnCtx<'ctx>) -> (PointerValue<'ctx>, String) {
        let unwrapped = match e {
            Expr::Borrow(inner, _) => inner.as_ref(),
            other => other,
        };
        let name = match unwrapped {
            Expr::Ident(n) => n,
            _ => panic!(
                "Kernel LLVM codegen: a vec_push/vec_get/vec_len/vec_set call's first argument must be a \
                 plain Vec variable name (optionally &mut-prefixed), not an arbitrary expression"
            ),
        };
        let addr = self.var_addr(name, ctx);
        let vty = self.var_kernel_type(name, ctx);
        let elem_ty = vec_elem_type(vty.trim_start_matches('&'))
            .unwrap_or_else(|| panic!("Kernel LLVM codegen: `{}` is not a Vec<T> (found type `{}`)", name, vty));
        (addr, elem_ty)
    }

    // Same idea as gen_bounds_check, but against a RUNTIME length (a
    // Vec's len field) rather than a compile-time-constant array size --
    // arrays know their size from the type itself; Vecs don't.
    fn gen_bounds_check_dynamic(&self, idx: inkwell::values::IntValue<'ctx>, len: inkwell::values::IntValue<'ctx>, ctx: &FnCtx<'ctx>) -> inkwell::values::IntValue<'ctx> {
        let zero = self.context.i64_type().const_zero();
        let ge_zero = self.builder.build_int_compare(IntPredicate::SGE, idx, zero, "ge0").unwrap();
        let lt_len = self.builder.build_int_compare(IntPredicate::SLT, idx, len, "ltlen").unwrap();
        let in_bounds = self.builder.build_and(ge_zero, lt_len, "inbounds").unwrap();
        let ok_bb = self.context.append_basic_block(ctx.function, "vecidx_ok");
        let panic_bb = self.context.append_basic_block(ctx.function, "vecidx_panic");
        self.builder.build_conditional_branch(in_bounds, ok_bb, panic_bb).unwrap();

        self.builder.position_at_end(panic_bb);
        self.gen_panic("Kernel panic: Vec index out of bounds");

        self.builder.position_at_end(ok_bb);
        idx
    }

    // vec_push's full grow-if-needed-then-append logic. Growth
    // (len==cap) reallocs via realloc(old_data, new_cap*elem_size) --
    // well-defined as malloc-equivalent when old_data is NULL (cap 0),
    // so there's no separate first-allocation special case. The
    // reallocated buffer is NEVER freed on drop (no destructors/GC/arena
    // in v1.x of either backend -- a named leak, not a silent one, same
    // as gen_concat's string allocations).
    fn gen_vec_push(&self, addr: PointerValue<'ctx>, elem_ty: &str, val: BasicValueEnum<'ctx>, ctx: &FnCtx<'ctx>) {
        let vec_ty = self.vec_struct_type();
        let i64_ty = self.context.i64_type();
        let data_field = self.builder.build_struct_gep(vec_ty, addr, 0, "datafield").unwrap();
        let len_field = self.builder.build_struct_gep(vec_ty, addr, 1, "lenfield").unwrap();
        let cap_field = self.builder.build_struct_gep(vec_ty, addr, 2, "capfield").unwrap();

        let len = self.builder.build_load(i64_ty, len_field, "len").unwrap().into_int_value();
        let cap = self.builder.build_load(i64_ty, cap_field, "cap").unwrap().into_int_value();
        let needs_grow = self.builder.build_int_compare(IntPredicate::EQ, len, cap, "needsgrow").unwrap();

        let grow_bb = self.context.append_basic_block(ctx.function, "vec_grow");
        let after_bb = self.context.append_basic_block(ctx.function, "vec_grow_done");
        self.builder.build_conditional_branch(needs_grow, grow_bb, after_bb).unwrap();

        self.builder.position_at_end(grow_bb);
        let cap_is_zero = self.builder.build_int_compare(IntPredicate::EQ, cap, i64_ty.const_zero(), "capzero").unwrap();
        let doubled = self.builder.build_int_mul(cap, i64_ty.const_int(2, false), "doubled").unwrap();
        let new_cap = self.builder.build_select(cap_is_zero, i64_ty.const_int(4, false), doubled, "newcap").unwrap().into_int_value();
        let elem_size = i64_ty.const_int(self.vec_elem_byte_size(elem_ty), false);
        let new_size = self.builder.build_int_mul(new_cap, elem_size, "newsize").unwrap();
        let old_data = self.builder.build_load(self.ptr_type(), data_field, "olddata").unwrap().into_pointer_value();
        let realloc_fn = self.module.get_function("realloc").unwrap();
        let new_data = self.builder.build_call(realloc_fn, &[old_data.into(), new_size.into()], "newdata").unwrap().try_as_basic_value().basic().unwrap().into_pointer_value();
        self.gen_null_check(new_data, "Kernel panic: vec_push() allocation failed", ctx);
        self.builder.build_store(data_field, new_data).unwrap();
        self.builder.build_store(cap_field, new_cap).unwrap();
        self.builder.build_unconditional_branch(after_bb).unwrap();

        self.builder.position_at_end(after_bb);
        // Reload data (may have just changed in grow_bb) rather than
        // carrying the old SSA value across the branch -- simpler and
        // correct even though it costs one extra load on the no-grow
        // path.
        let data = self.builder.build_load(self.ptr_type(), data_field, "data").unwrap().into_pointer_value();
        let elem_llvm_ty = self.llvm_type(elem_ty);
        let elem_addr = unsafe { self.builder.build_gep(elem_llvm_ty, data, &[len], "newelem").unwrap() };
        self.builder.build_store(elem_addr, val).unwrap();
        let new_len = self.builder.build_int_add(len, i64_ty.const_int(1, false), "newlen").unwrap();
        self.builder.build_store(len_field, new_len).unwrap();
    }

    fn gen_bounds_check(&self, idx: inkwell::values::IntValue<'ctx>, n: i64, ctx: &FnCtx<'ctx>) -> inkwell::values::IntValue<'ctx> {
        let zero = self.context.i64_type().const_zero();
        let n_val = self.context.i64_type().const_int(n as u64, false);
        let ge_zero = self.builder.build_int_compare(IntPredicate::SGE, idx, zero, "ge0").unwrap();
        let lt_n = self.builder.build_int_compare(IntPredicate::SLT, idx, n_val, "ltn").unwrap();
        let in_bounds = self.builder.build_and(ge_zero, lt_n, "inbounds").unwrap();
        let ok_bb = self.context.append_basic_block(ctx.function, "idx_ok");
        let panic_bb = self.context.append_basic_block(ctx.function, "idx_panic");
        self.builder.build_conditional_branch(in_bounds, ok_bb, panic_bb).unwrap();

        self.builder.position_at_end(panic_bb);
        self.gen_panic("Kernel panic: array index out of bounds");

        self.builder.position_at_end(ok_bb);
        idx
    }

    // Prints `msg` and terminates the process (exit code 1) -- real
    // libc's `puts`+`exit`, never returns (ends the current basic block
    // with `unreachable`, matching exit()'s real noreturn semantics, not
    // just a convention). The native-backend equivalent of
    // kernel_runtime/runtime.c's kernel_panic, just backed by a real OS
    // process exit instead of a freestanding halt loop.
    fn gen_panic(&self, msg: &str) {
        let msg_ptr = self.builder.build_global_string_ptr(msg, "panicmsg").unwrap().as_pointer_value();
        let puts_fn = self.module.get_function("puts").unwrap();
        let exit_fn = self.module.get_function("exit").unwrap();
        self.builder.build_call(puts_fn, &[msg_ptr.into()], "puts_call").unwrap();
        self.builder.build_call(exit_fn, &[self.context.i32_type().const_int(1, false).into()], "exit_call").unwrap();
        self.builder.build_unreachable().unwrap();
    }

    // Branches to a panic (see gen_panic) if `ptr` is NULL, otherwise
    // falls through -- shared by read_file/write_file's fopen() result,
    // since a failed fopen (bad path, permissions, etc.) returning NULL
    // and getting silently dereferenced anyway would be a real crash,
    // not a clear Kernel-level error.
    fn gen_null_check(&self, ptr: PointerValue<'ctx>, msg: &str, ctx: &FnCtx<'ctx>) {
        let is_null = self.builder.build_is_null(ptr, "isnull").unwrap();
        let ok_bb = self.context.append_basic_block(ctx.function, "notnull");
        let panic_bb = self.context.append_basic_block(ctx.function, "isnull_panic");
        self.builder.build_conditional_branch(is_null, panic_bb, ok_bb).unwrap();
        self.builder.position_at_end(panic_bb);
        self.gen_panic(msg);
        self.builder.position_at_end(ok_bb);
    }

    // read_file(path) -> str -- real file I/O via libc stdio, since this
    // backend targets a normal OS process with a real filesystem (unlike
    // codegen_c.rs's freestanding kernel target, which has none at all
    // and rejects both builtins outright). Reads the WHOLE file into one
    // malloc'd, NUL-terminated buffer: fopen -> fseek to the end -> ftell
    // for the size -> rewind -> malloc(size+1) -> fread -> manually write
    // the NUL terminator (fread doesn't null-terminate on its own) ->
    // fclose. Like gen_concat's allocations, this buffer is never freed
    // (no GC/arena in v1.x of either backend) -- a named memory-leak gap,
    // not a silent one.
    fn gen_read_file(&self, path: PointerValue<'ctx>, ctx: &FnCtx<'ctx>) -> PointerValue<'ctx> {
        let fopen_fn = self.module.get_function("fopen").unwrap();
        let fseek_fn = self.module.get_function("fseek").unwrap();
        let ftell_fn = self.module.get_function("ftell").unwrap();
        let rewind_fn = self.module.get_function("rewind").unwrap();
        let fread_fn = self.module.get_function("fread").unwrap();
        let fclose_fn = self.module.get_function("fclose").unwrap();
        let malloc_fn = self.module.get_function("malloc").unwrap();

        let mode = self.builder.build_global_string_ptr("r", "read_mode").unwrap().as_pointer_value();
        let file = self.builder.build_call(fopen_fn, &[path.into(), mode.into()], "file").unwrap().try_as_basic_value().basic().unwrap().into_pointer_value();
        self.gen_null_check(file, "Kernel panic: read_file() could not open the file", ctx);

        let i32_ty = self.context.i32_type();
        let i64_ty = self.context.i64_type();
        let seek_end = i32_ty.const_int(2, false); // SEEK_END
        self.builder.build_call(fseek_fn, &[file.into(), i64_ty.const_zero().into(), seek_end.into()], "seek_end").unwrap();
        let size = self.builder.build_call(ftell_fn, &[file.into()], "size").unwrap().try_as_basic_value().basic().unwrap().into_int_value();
        self.builder.build_call(rewind_fn, &[file.into()], "rewind_call").unwrap();

        let size_plus_nul = self.builder.build_int_add(size, i64_ty.const_int(1, false), "size_nul").unwrap();
        let buf = self.builder.build_call(malloc_fn, &[size_plus_nul.into()], "readbuf").unwrap().try_as_basic_value().basic().unwrap().into_pointer_value();
        self.builder.build_call(fread_fn, &[buf.into(), i64_ty.const_int(1, false).into(), size.into(), file.into()], "fread_call").unwrap();

        let nul_ptr = unsafe { self.builder.build_gep(self.context.i8_type(), buf, &[size], "nul_ptr").unwrap() };
        self.builder.build_store(nul_ptr, self.context.i8_type().const_zero()).unwrap();

        self.builder.build_call(fclose_fn, &[file.into()], "fclose_call").unwrap();
        buf
    }

    // write_file(path, content) -- the write twin of gen_read_file.
    fn gen_write_file(&self, path: PointerValue<'ctx>, content: PointerValue<'ctx>, ctx: &FnCtx<'ctx>) {
        let fopen_fn = self.module.get_function("fopen").unwrap();
        let fwrite_fn = self.module.get_function("fwrite").unwrap();
        let fclose_fn = self.module.get_function("fclose").unwrap();
        let strlen_fn = self.module.get_function("strlen").unwrap();

        let mode = self.builder.build_global_string_ptr("w", "write_mode").unwrap().as_pointer_value();
        let file = self.builder.build_call(fopen_fn, &[path.into(), mode.into()], "file").unwrap().try_as_basic_value().basic().unwrap().into_pointer_value();
        self.gen_null_check(file, "Kernel panic: write_file() could not open the file", ctx);

        let len = self.builder.build_call(strlen_fn, &[content.into()], "content_len").unwrap().try_as_basic_value().basic().unwrap().into_int_value();
        let i64_ty = self.context.i64_type();
        self.builder.build_call(fwrite_fn, &[content.into(), i64_ty.const_int(1, false).into(), len.into(), file.into()], "fwrite_call").unwrap();
        self.builder.build_call(fclose_fn, &[file.into()], "fclose_call").unwrap();
    }

    fn gen_expr(&self, e: &Expr, ctx: &FnCtx<'ctx>) -> BasicValueEnum<'ctx> {
        match e {
            Expr::Int(n) => self.context.i64_type().const_int(*n as u64, true).into(),
            Expr::Float(x) => self.context.f64_type().const_float(*x).into(),
            Expr::Bool(b) => self.context.bool_type().const_int(*b as u64, false).into(),
            Expr::Str(s) => self.builder.build_global_string_ptr(s, "strlit").unwrap().as_pointer_value().into(),
            Expr::Ident(name) => {
                let addr = self.var_addr(name, ctx);
                let kernel_ty = self.var_kernel_type(name, ctx);
                // A pointer-typed variable (a borrowed PARAMETER, an
                // ARRAY parameter -- always decayed, see is_ptr's doc
                // comment -- or a locally `let`-bound borrow like `let
                // r1 = &origin`) always has a `ptr`-typed alloca --
                // loading it with any other LLVM type (e.g. the pointee's
                // struct type) would read the wrong number of bytes
                // starting at the wrong place, not just produce the
                // wrong VALUE (a real bug this exact check exists to
                // prevent, caught by multi_shared_borrow_ok.kn printing a
                // garbage pointer bit-pattern instead of 5 before this
                // fix). `is_ptr` alone would miss a LOCAL borrow (never
                // tracked there, see FnCtx::is_ptr's doc comment), hence
                // checking both.
                let needs_ptr_load = kernel_ty.starts_with('&') || *ctx.is_ptr.get(name).unwrap_or(&false);
                let llvm_ty = if needs_ptr_load { self.ptr_type().into() } else { self.llvm_type(&kernel_ty) };
                self.builder.build_load(llvm_ty, addr, name).unwrap()
            }
            Expr::Unary(UnaryOp::Neg, inner) => {
                let v = self.gen_expr(inner, ctx);
                match v {
                    BasicValueEnum::IntValue(i) => self.builder.build_int_neg(i, "neg").unwrap().into(),
                    BasicValueEnum::FloatValue(f) => self.builder.build_float_neg(f, "fneg").unwrap().into(),
                    _ => panic!("Kernel LLVM codegen: unary `-` on a non-numeric value"),
                }
            }
            Expr::Unary(UnaryOp::Not, inner) => {
                let v = self.gen_expr(inner, ctx).into_int_value();
                self.builder.build_not(v, "not").unwrap().into()
            }
            Expr::Binary(l, BinOp::And, r) => self.gen_short_circuit(l, r, ctx, true).into(),
            Expr::Binary(l, BinOp::Or, r) => self.gen_short_circuit(l, r, ctx, false).into(),
            Expr::Binary(l, BinOp::Add, r)
                if self.infer_kernel_type(l, ctx) == "str" && self.infer_kernel_type(r, ctx) == "str" =>
            {
                self.gen_concat(l, r, ctx).into()
            }
            Expr::Binary(l, op, r) => self.gen_binop(l, *op, r, ctx),
            Expr::Field(..) => {
                let obj = match e {
                    Expr::Field(obj, _) => obj,
                    _ => unreachable!(),
                };
                let field = match e {
                    Expr::Field(_, f) => f,
                    _ => unreachable!(),
                };
                let addr = self.gen_addr(e, ctx);
                let obj_ty = self.infer_kernel_type(obj, ctx);
                let struct_name = self.struct_name_of(&obj_ty).unwrap();
                let field_ty = self.field_kernel_type(struct_name, field);
                self.builder.build_load(self.llvm_type(&field_ty), addr, "fieldval").unwrap()
            }
            Expr::Borrow(_, _) => self.gen_addr(e, ctx).into(),
            Expr::Call { name, args } => self.gen_call(name, args, ctx),
            Expr::ArrayLit(elems) => {
                let elem_ty = elems.first().map(|el| self.infer_kernel_type(el, ctx)).unwrap_or_else(|| "int".to_string());
                let arr_llvm_ty = self.llvm_type(&elem_ty).array_type(elems.len() as u32);
                let mut agg = arr_llvm_ty.get_undef();
                for (i, el) in elems.iter().enumerate() {
                    let val = self.gen_expr(el, ctx);
                    agg = self.builder.build_insert_value(agg, val, i as u32, "arrayinit").unwrap().into_array_value();
                }
                agg.into()
            }
            Expr::Index(arr, idx) => {
                let (addr, elem_ty) = self.array_elem_addr(arr, idx, ctx);
                self.builder.build_load(self.llvm_type(&elem_ty), addr, "idxval").unwrap()
            }
            Expr::Range(..) => panic!("Kernel LLVM codegen: a range is only valid as a for-loop iterator"),
            Expr::Closure { .. } => unreachable!("Kernel LLVM codegen: Expr::Closure should always be erased by lambda_lift.rs before codegen runs"),
        }
    }

    // `a + b` where both are `str` -- real heap allocation via libc
    // malloc (unlike codegen_c.rs's kernel_strcat, which writes into a
    // fixed-size rotating pool of static buffers because there's no
    // malloc/heap in a -ffreestanding environment at all). Correctly
    // handles any length with no truncation and no "an older still-live
    // result gets silently overwritten" risk the rotating pool has --
    // the real tradeoff the other direction: this never frees the
    // allocation (no GC/arena exists in v1.x of EITHER backend), a named
    // memory-leak gap, not a silent one.
    fn gen_concat(&self, l: &Expr, r: &Expr, ctx: &FnCtx<'ctx>) -> PointerValue<'ctx> {
        let a = self.gen_expr(l, ctx).into_pointer_value();
        let b = self.gen_expr(r, ctx).into_pointer_value();
        self.gen_concat_ptrs(a, b)
    }

    fn gen_concat_ptrs(&self, a: PointerValue<'ctx>, b: PointerValue<'ctx>) -> PointerValue<'ctx> {
        let strlen_fn = self.module.get_function("strlen").unwrap();
        let malloc_fn = self.module.get_function("malloc").unwrap();
        let strcpy_fn = self.module.get_function("strcpy").unwrap();
        let strcat_fn = self.module.get_function("strcat").unwrap();
        let len_a = self.builder.build_call(strlen_fn, &[a.into()], "len_a").unwrap().try_as_basic_value().basic().unwrap().into_int_value();
        let len_b = self.builder.build_call(strlen_fn, &[b.into()], "len_b").unwrap().try_as_basic_value().basic().unwrap().into_int_value();
        let total = self.builder.build_int_add(len_a, len_b, "total_len").unwrap();
        let total_plus_nul = self.builder.build_int_add(total, self.context.i64_type().const_int(1, false), "total_nul").unwrap();
        let buf = self.builder.build_call(malloc_fn, &[total_plus_nul.into()], "concat_buf").unwrap().try_as_basic_value().basic().unwrap().into_pointer_value();
        self.builder.build_call(strcpy_fn, &[buf.into(), a.into()], "strcpy_call").unwrap();
        self.builder.build_call(strcat_fn, &[buf.into(), b.into()], "strcat_call").unwrap();
        buf
    }

    fn gen_short_circuit(&self, l: &Expr, r: &Expr, ctx: &FnCtx<'ctx>, is_and: bool) -> inkwell::values::IntValue<'ctx> {
        // Real branching short-circuit, matching C's `&&`/`||` semantics
        // exactly (what codegen_c.rs emits) -- see this module's doc
        // comment for why a simpler eager and/or would be a real,
        // dangerous cross-backend divergence, not a harmless shortcut.
        let lhs = self.gen_expr(l, ctx).into_int_value();
        let entry_bb = self.builder.get_insert_block().unwrap();
        let rhs_bb = self.context.append_basic_block(ctx.function, if is_and { "and_rhs" } else { "or_rhs" });
        let merge_bb = self.context.append_basic_block(ctx.function, if is_and { "and_merge" } else { "or_merge" });
        if is_and {
            self.builder.build_conditional_branch(lhs, rhs_bb, merge_bb).unwrap();
        } else {
            self.builder.build_conditional_branch(lhs, merge_bb, rhs_bb).unwrap();
        }
        self.builder.position_at_end(rhs_bb);
        let rhs = self.gen_expr(r, ctx).into_int_value();
        let rhs_end_bb = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(merge_bb).unwrap();
        self.builder.position_at_end(merge_bb);
        let phi = self.builder.build_phi(self.context.bool_type(), if is_and { "and_result" } else { "or_result" }).unwrap();
        phi.add_incoming(&[(&lhs, entry_bb), (&rhs, rhs_end_bb)]);
        phi.as_basic_value().into_int_value()
    }

    fn gen_binop(&self, l: &Expr, op: BinOp, r: &Expr, ctx: &FnCtx<'ctx>) -> BasicValueEnum<'ctx> {
        let lv = self.gen_expr(l, ctx);
        let rv = self.gen_expr(r, ctx);
        let is_float = matches!(lv, BasicValueEnum::FloatValue(_)) || matches!(rv, BasicValueEnum::FloatValue(_));
        if is_float {
            let lf = self.to_float(lv);
            let rf = self.to_float(rv);
            match op {
                BinOp::Add => self.builder.build_float_add(lf, rf, "fadd").unwrap().into(),
                BinOp::Sub => self.builder.build_float_sub(lf, rf, "fsub").unwrap().into(),
                BinOp::Mul => self.builder.build_float_mul(lf, rf, "fmul").unwrap().into(),
                BinOp::Div => self.builder.build_float_div(lf, rf, "fdiv").unwrap().into(),
                BinOp::Mod => self.builder.build_float_rem(lf, rf, "frem").unwrap().into(),
                BinOp::Eq => self.builder.build_float_compare(FloatPredicate::OEQ, lf, rf, "feq").unwrap().into(),
                BinOp::NotEq => self.builder.build_float_compare(FloatPredicate::ONE, lf, rf, "fne").unwrap().into(),
                BinOp::Lt => self.builder.build_float_compare(FloatPredicate::OLT, lf, rf, "flt").unwrap().into(),
                BinOp::Gt => self.builder.build_float_compare(FloatPredicate::OGT, lf, rf, "fgt").unwrap().into(),
                BinOp::LtEq => self.builder.build_float_compare(FloatPredicate::OLE, lf, rf, "fle").unwrap().into(),
                BinOp::GtEq => self.builder.build_float_compare(FloatPredicate::OGE, lf, rf, "fge").unwrap().into(),
                BinOp::And | BinOp::Or => unreachable!("handled by gen_short_circuit before reaching gen_binop"),
            }
        } else {
            let li = lv.into_int_value();
            let ri = rv.into_int_value();
            match op {
                BinOp::Add => self.builder.build_int_add(li, ri, "add").unwrap().into(),
                BinOp::Sub => self.builder.build_int_sub(li, ri, "sub").unwrap().into(),
                BinOp::Mul => self.builder.build_int_mul(li, ri, "mul").unwrap().into(),
                BinOp::Div => self.builder.build_int_signed_div(li, ri, "div").unwrap().into(),
                BinOp::Mod => self.builder.build_int_signed_rem(li, ri, "rem").unwrap().into(),
                BinOp::Eq => self.builder.build_int_compare(IntPredicate::EQ, li, ri, "eq").unwrap().into(),
                BinOp::NotEq => self.builder.build_int_compare(IntPredicate::NE, li, ri, "ne").unwrap().into(),
                BinOp::Lt => self.builder.build_int_compare(IntPredicate::SLT, li, ri, "lt").unwrap().into(),
                BinOp::Gt => self.builder.build_int_compare(IntPredicate::SGT, li, ri, "gt").unwrap().into(),
                BinOp::LtEq => self.builder.build_int_compare(IntPredicate::SLE, li, ri, "le").unwrap().into(),
                BinOp::GtEq => self.builder.build_int_compare(IntPredicate::SGE, li, ri, "ge").unwrap().into(),
                BinOp::And | BinOp::Or => unreachable!("handled by gen_short_circuit before reaching gen_binop"),
            }
        }
    }

    fn to_float(&self, v: BasicValueEnum<'ctx>) -> inkwell::values::FloatValue<'ctx> {
        match v {
            BasicValueEnum::FloatValue(f) => f,
            BasicValueEnum::IntValue(i) => self.builder.build_signed_int_to_float(i, self.context.f64_type(), "itof").unwrap(),
            _ => panic!("Kernel LLVM codegen: expected a numeric value"),
        }
    }

    fn gen_call(&self, name: &str, args: &[CallArg], ctx: &FnCtx<'ctx>) -> BasicValueEnum<'ctx> {
        match name {
            "abs" => {
                let e = arg_expr(args.first().unwrap_or_else(|| panic!("Kernel LLVM codegen: abs() requires exactly one argument")));
                let v = self.gen_expr(e, ctx);
                match v {
                    BasicValueEnum::FloatValue(f) => {
                        let zero = self.context.f64_type().const_zero();
                        let is_neg = self.builder.build_float_compare(FloatPredicate::OLT, f, zero, "isneg").unwrap();
                        let negated = self.builder.build_float_neg(f, "negf").unwrap();
                        self.builder.build_select(is_neg, negated, f, "fabs").unwrap()
                    }
                    BasicValueEnum::IntValue(i) => {
                        let zero = self.context.i64_type().const_zero();
                        let is_neg = self.builder.build_int_compare(IntPredicate::SLT, i, zero, "isneg").unwrap();
                        let negated = self.builder.build_int_neg(i, "negi").unwrap();
                        self.builder.build_select(is_neg, negated, i, "abs").unwrap()
                    }
                    _ => panic!("Kernel LLVM codegen: abs() on a non-numeric value"),
                }
            }
            "len" => {
                let e = arg_expr(args.first().unwrap_or_else(|| panic!("Kernel LLVM codegen: len() requires exactly one argument")));
                let ty = self.infer_kernel_type(e, ctx);
                if let Some((_, n)) = kernel_array_info(&ty) {
                    // Compile-time-known constant, zero runtime cost --
                    // matching codegen_c.rs's len() on arrays exactly.
                    self.context.i64_type().const_int(n as u64, false).into()
                } else if ty == "str" {
                    let s = self.gen_expr(e, ctx);
                    let strlen_fn = self.module.get_function("strlen").unwrap();
                    self.builder.build_call(strlen_fn, &[s.into()], "strlen_call").unwrap().try_as_basic_value().basic().unwrap()
                } else {
                    panic!("Kernel LLVM codegen: len() only supports arrays and strings in v1.x, found a `{}`", ty);
                }
            }
            "read_file" => {
                let path_expr = arg_expr(args.first().unwrap_or_else(|| panic!("Kernel LLVM codegen: read_file() requires exactly one argument")));
                let path = self.gen_expr(path_expr, ctx).into_pointer_value();
                self.gen_read_file(path, ctx).into()
            }
            "write_file" => {
                let path_expr = arg_expr(args.first().unwrap_or_else(|| panic!("Kernel LLVM codegen: write_file() requires a path and content argument")));
                let content_expr = arg_expr(args.get(1).unwrap_or_else(|| panic!("Kernel LLVM codegen: write_file() requires a path and content argument")));
                let path = self.gen_expr(path_expr, ctx).into_pointer_value();
                let content = self.gen_expr(content_expr, ctx).into_pointer_value();
                self.gen_write_file(path, content, ctx);
                self.context.i64_type().const_zero().into() // Unit -- discarded, see gen_stmt's Stmt::Expr
            }
            "raw_write" => panic!(
                "Kernel LLVM codegen: `raw_write` isn't supported in the LLVM backend -- there's no MMIO \
                 on a normal userspace process -- this only makes sense for the freestanding C backend"
            ),
            "vec_new" => self.vec_struct_type().const_zero().into(),
            "vec_len" => {
                let (addr, _elem_ty) = self.vec_name_and_elem_ty(arg_expr(args.first().unwrap_or_else(|| panic!("Kernel LLVM codegen: vec_len() requires one argument"))), ctx);
                let len_ptr = self.builder.build_struct_gep(self.vec_struct_type(), addr, 1, "lenfield").unwrap();
                self.builder.build_load(self.context.i64_type(), len_ptr, "len").unwrap()
            }
            "vec_get" => {
                let (addr, elem_ty) = self.vec_name_and_elem_ty(arg_expr(args.first().unwrap_or_else(|| panic!("Kernel LLVM codegen: vec_get() requires a Vec and an index"))), ctx);
                let idx_expr = arg_expr(args.get(1).unwrap_or_else(|| panic!("Kernel LLVM codegen: vec_get() requires a Vec and an index")));
                let idx = self.gen_expr(idx_expr, ctx).into_int_value();
                let data_ptr = self.builder.build_struct_gep(self.vec_struct_type(), addr, 0, "datafield").unwrap();
                let data = self.builder.build_load(self.ptr_type(), data_ptr, "data").unwrap().into_pointer_value();
                let len_ptr = self.builder.build_struct_gep(self.vec_struct_type(), addr, 1, "lenfield").unwrap();
                let len = self.builder.build_load(self.context.i64_type(), len_ptr, "len").unwrap().into_int_value();
                let checked_idx = self.gen_bounds_check_dynamic(idx, len, ctx);
                let elem_llvm_ty = self.llvm_type(&elem_ty);
                let elem_addr = unsafe { self.builder.build_gep(elem_llvm_ty, data, &[checked_idx], "vecelem").unwrap() };
                self.builder.build_load(elem_llvm_ty, elem_addr, "vecval").unwrap()
            }
            "vec_push" => {
                let (addr, elem_ty) = self.vec_name_and_elem_ty(arg_expr(args.first().unwrap_or_else(|| panic!("Kernel LLVM codegen: vec_push() requires a Vec and a value"))), ctx);
                let val_expr = arg_expr(args.get(1).unwrap_or_else(|| panic!("Kernel LLVM codegen: vec_push() requires a Vec and a value")));
                let val = self.gen_expr(val_expr, ctx);
                self.gen_vec_push(addr, &elem_ty, val, ctx);
                self.context.i64_type().const_zero().into() // Unit -- discarded, see gen_stmt's Stmt::Expr
            }
            "vec_set" => {
                let (addr, elem_ty) = self.vec_name_and_elem_ty(arg_expr(args.first().unwrap_or_else(|| panic!("Kernel LLVM codegen: vec_set() requires a Vec, an index, and a value"))), ctx);
                let idx_expr = arg_expr(args.get(1).unwrap_or_else(|| panic!("Kernel LLVM codegen: vec_set() requires a Vec, an index, and a value")));
                let val_expr = arg_expr(args.get(2).unwrap_or_else(|| panic!("Kernel LLVM codegen: vec_set() requires a Vec, an index, and a value")));
                let idx = self.gen_expr(idx_expr, ctx).into_int_value();
                let val = self.gen_expr(val_expr, ctx);
                let data_ptr = self.builder.build_struct_gep(self.vec_struct_type(), addr, 0, "datafield").unwrap();
                let data = self.builder.build_load(self.ptr_type(), data_ptr, "data").unwrap().into_pointer_value();
                let len_ptr = self.builder.build_struct_gep(self.vec_struct_type(), addr, 1, "lenfield").unwrap();
                let len = self.builder.build_load(self.context.i64_type(), len_ptr, "len").unwrap().into_int_value();
                let checked_idx = self.gen_bounds_check_dynamic(idx, len, ctx);
                let elem_llvm_ty = self.llvm_type(&elem_ty);
                let elem_addr = unsafe { self.builder.build_gep(elem_llvm_ty, data, &[checked_idx], "vecelem").unwrap() };
                self.builder.build_store(elem_addr, val).unwrap();
                self.context.i64_type().const_zero().into() // Unit -- discarded, see gen_stmt's Stmt::Expr
            }
            _ if self.structs.contains(name) => {
                let struct_ty = self.struct_types[name];
                let fields = &self.struct_fields[name];
                let mut agg = struct_ty.get_undef();
                for a in args {
                    if let CallArg::Named(fname, e) = a {
                        let idx = fields.iter().position(|(n, _)| n == fname).unwrap_or_else(|| {
                            panic!("Kernel LLVM codegen: struct `{}` has no field `{}`", name, fname)
                        }) as u32;
                        let val = self.gen_expr(e, ctx);
                        agg = self.builder.build_insert_value(agg, val, idx, "structinit").unwrap().into_struct_value();
                    }
                }
                agg.into()
            }
            _ => {
                let callee = self.module.get_function(name).unwrap();
                // No special-casing by the callee's declared borrowedness
                // here -- matching codegen_c.rs's call_arg exactly: the
                // ARGUMENT EXPRESSION itself already produces the right
                // pointer value when the Kernel source calls for one
                // (`peek(&origin)` -> Expr::Borrow computes the address
                // directly; `peek(r1)` where `r1: &Point` -> Expr::Ident
                // loads r1's own (pointer-typed) storage, which already
                // holds that same address) -- gen_expr handles both
                // correctly on its own. Using gen_addr here instead (an
                // earlier version of this function did) is a real, subtle
                // bug: for a plain Ident argument it would take the
                // address of the ARGUMENT VARIABLE's own storage slot
                // instead of the pointer VALUE already stored there.
                //
                // ARRAYS are the one real exception: they always decay to
                // a pointer at the call site (matching codegen_c.rs),
                // which gen_expr does NOT do on its own for a plain array
                // Ident (that loads the FULL array VALUE, correct for
                // e.g. a `let` but wrong for a call argument) -- so this
                // is the one place argument codegen still has to look at
                // the ARGUMENT'S OWN inferred type, not special-case the
                // callee.
                let call_args: Vec<BasicMetadataValueEnum> = args
                    .iter()
                    .map(|a| {
                        let e = arg_expr(a);
                        if kernel_array_info(&self.infer_kernel_type(e, ctx)).is_some() {
                            self.array_arg_ptr(e, ctx).into()
                        } else {
                            self.gen_expr(e, ctx).into()
                        }
                    })
                    .collect();
                let result = self.builder.build_call(callee, &call_args, "call").unwrap();
                result.try_as_basic_value().basic().unwrap_or_else(|| self.context.i64_type().const_zero().into())
            }
        }
    }

    // A pointer to the FIRST ELEMENT of an array argument expression --
    // either an existing array's base pointer (array_base_ptr), or, for a
    // bare literal passed directly (`sum3([1, 2, 3])`), a fresh temporary
    // alloca to give the freshly-built value an address at all. Mirrors
    // codegen_c.rs's call_arg's C99 compound-literal-cast trick for
    // exactly the same bare-array-literal-as-argument shape, just via a
    // real temporary stack slot instead of a C expression-level cast.
    fn array_arg_ptr(&self, e: &Expr, ctx: &FnCtx<'ctx>) -> PointerValue<'ctx> {
        match e {
            Expr::ArrayLit(_) => {
                let kernel_ty = self.infer_kernel_type(e, ctx);
                let val = self.gen_expr(e, ctx);
                let alloca = self.builder.build_alloca(self.llvm_type(&kernel_ty), "arraylit_tmp").unwrap();
                self.builder.build_store(alloca, val).unwrap();
                alloca
            }
            _ => self.array_base_ptr(e, ctx),
        }
    }

    fn gen_stmt(&self, s: &Stmt, ctx: &mut FnCtx<'ctx>) {
        match s {
            Stmt::Let { name, ty, value, .. } => {
                // `vec_new()` is the one expression in this language
                // whose Kernel type can't be inferred from itself at
                // all (no call-site argument to infer the element type
                // from -- see RFC-002's Vec<T> section) -- it REQUIRES
                // the `let`'s own explicit `: Vec<ElemType>` annotation,
                // used directly here instead of going through the
                // normal infer_kernel_type path (which would otherwise
                // silently default to "int" and build the wrong alloca
                // type entirely).
                let kernel_ty = if let Expr::Call { name: cname, .. } = value {
                    if cname == "vec_new" {
                        ty.clone().unwrap_or_else(|| {
                            panic!(
                                "Kernel LLVM codegen: `let {} = vec_new()` needs an explicit `: Vec<ElemType>` \
                                 annotation -- there's no call-site argument to infer the element type from",
                                name
                            )
                        })
                    } else {
                        self.infer_kernel_type(value, ctx)
                    }
                } else {
                    self.infer_kernel_type(value, ctx)
                };
                let val = self.gen_expr(value, ctx);
                let llvm_ty = if matches!(value, Expr::Borrow(..)) { self.ptr_type().into() } else { self.llvm_type(&kernel_ty) };
                let alloca = self.builder.build_alloca(llvm_ty, name).unwrap();
                self.builder.build_store(alloca, val).unwrap();
                ctx.vars.insert(name.clone(), alloca);
                ctx.var_types.insert(name.clone(), kernel_ty);
                ctx.is_ptr.insert(name.clone(), false); // see FnCtx::is_ptr's doc comment: only params track this
            }
            Stmt::Assign { name, op, value } => {
                let addr = self.var_addr(name, ctx);
                let kernel_ty = self.var_kernel_type(name, ctx);
                let llvm_ty = self.llvm_type(&kernel_ty);
                let new_val = match op {
                    AssignOp::Set => self.gen_expr(value, ctx),
                    AssignOp::Add if kernel_ty == "str" => {
                        // `msg += "more"` -- same real strcat-via-malloc
                        // path as `+` (gen_concat), just reading the
                        // current value back out of its own storage
                        // first instead of taking it as an already-
                        // evaluated operand.
                        let cur = self.builder.build_load(llvm_ty, addr, "cur").unwrap().into_pointer_value();
                        let rhs = self.gen_expr(value, ctx).into_pointer_value();
                        self.gen_concat_ptrs(cur, rhs).into()
                    }
                    AssignOp::Add | AssignOp::Sub => {
                        let cur = self.builder.build_load(llvm_ty, addr, "cur").unwrap();
                        let rhs = self.gen_expr(value, ctx);
                        match (cur, rhs) {
                            (BasicValueEnum::IntValue(a), BasicValueEnum::IntValue(b)) => {
                                if *op == AssignOp::Add { self.builder.build_int_add(a, b, "add").unwrap().into() }
                                else { self.builder.build_int_sub(a, b, "sub").unwrap().into() }
                            }
                            _ => {
                                let a = self.to_float(cur);
                                let b = self.to_float(rhs);
                                if *op == AssignOp::Add { self.builder.build_float_add(a, b, "fadd").unwrap().into() }
                                else { self.builder.build_float_sub(a, b, "fsub").unwrap().into() }
                            }
                        }
                    }
                };
                self.builder.build_store(addr, new_val).unwrap();
            }
            Stmt::FieldAssign { object, field, value } => {
                let addr = self.gen_addr(&Expr::Field(Box::new(object.clone()), field.clone()), ctx);
                let val = self.gen_expr(value, ctx);
                self.builder.build_store(addr, val).unwrap();
            }
            Stmt::IndexAssign { array, index, value } => {
                let (addr, _elem_ty) = self.array_elem_addr(array, index, ctx);
                let val = self.gen_expr(value, ctx);
                self.builder.build_store(addr, val).unwrap();
            }
            Stmt::If { arms, else_body } => self.gen_if(arms, else_body, ctx),
            Stmt::While { cond, body } => self.gen_while(cond, body, ctx),
            Stmt::For { var, iter, body } => self.gen_for(var, iter, body, ctx),
            Stmt::Return(expr) => {
                match expr {
                    Some(e) => {
                        let val = self.gen_expr(e, ctx);
                        if ctx.function.get_name().to_str().unwrap() == "main" {
                            // Kernel main's declared return value becomes
                            // the process's real exit code -- truncate to
                            // i32 since that's the real C ABI's `main`
                            // return width (the OS masks it to one byte
                            // regardless, same as any native `exit()`).
                            let as_i64 = val.into_int_value();
                            let as_i32 = self.builder.build_int_truncate(as_i64, self.context.i32_type(), "exitcode").unwrap();
                            self.builder.build_return(Some(&as_i32)).unwrap();
                        } else {
                            self.builder.build_return(Some(&val)).unwrap();
                        }
                    }
                    None => {
                        if ctx.function.get_name().to_str().unwrap() == "main" {
                            self.builder.build_return(Some(&self.context.i32_type().const_int(0, false))).unwrap();
                        } else {
                            self.builder.build_return(None).unwrap();
                        }
                    }
                }
            }
            Stmt::Unsafe(_) => panic!(
                "Kernel LLVM codegen: `unsafe` blocks (raw_write/MMIO) aren't supported in the LLVM backend -- \
                 there's no MMIO on a normal userspace process; use `kernel build` (the C/freestanding backend) instead"
            ),
            Stmt::Expr(e) => {
                if let Expr::Call { name, args } = e {
                    if name == "print" {
                        self.gen_print(args, ctx);
                        return;
                    }
                }
                self.gen_expr(e, ctx);
            }
        }
    }

    fn gen_print(&self, args: &[CallArg], ctx: &FnCtx<'ctx>) {
        let mut fmt = String::new();
        let mut vals: Vec<BasicValueEnum<'ctx>> = Vec::new();
        for (i, a) in args.iter().enumerate() {
            if i > 0 {
                fmt.push(' ');
            }
            let e = arg_expr(a);
            let ty = self.infer_kernel_type(e, ctx);
            match ty.as_str() {
                "str" => {
                    fmt.push_str("%s");
                    vals.push(self.gen_expr(e, ctx));
                }
                "float" => {
                    fmt.push_str("%.2f");
                    vals.push(self.gen_expr(e, ctx));
                }
                "bool" => {
                    fmt.push_str("%s");
                    let v = self.gen_expr(e, ctx).into_int_value();
                    let true_s = self.builder.build_global_string_ptr("true", "truestr").unwrap().as_pointer_value();
                    let false_s = self.builder.build_global_string_ptr("false", "falsestr").unwrap().as_pointer_value();
                    let selected = self.builder.build_select(v, true_s, false_s, "boolstr").unwrap();
                    vals.push(selected);
                }
                _ => {
                    fmt.push_str("%ld");
                    vals.push(self.gen_expr(e, ctx));
                }
            }
        }
        fmt.push('\n');
        let fmt_ptr = self.builder.build_global_string_ptr(&fmt, "fmt").unwrap().as_pointer_value();
        let mut call_args: Vec<BasicMetadataValueEnum> = vec![fmt_ptr.into()];
        call_args.extend(vals.into_iter().map(BasicMetadataValueEnum::from));
        self.builder.build_call(self.printf_fn, &call_args, "printf_call").unwrap();
    }

    fn gen_if(&self, arms: &[(Expr, Vec<Stmt>)], else_body: &Option<Vec<Stmt>>, ctx: &mut FnCtx<'ctx>) {
        let merge_bb = self.context.append_basic_block(ctx.function, "if_merge");
        self.gen_if_chain(arms, else_body, 0, merge_bb, ctx);
        self.builder.position_at_end(merge_bb);
    }

    fn gen_if_chain(&self, arms: &[(Expr, Vec<Stmt>)], else_body: &Option<Vec<Stmt>>, i: usize, merge_bb: inkwell::basic_block::BasicBlock<'ctx>, ctx: &mut FnCtx<'ctx>) {
        if i >= arms.len() {
            if let Some(body) = else_body {
                let else_bb = self.context.append_basic_block(ctx.function, "else_body");
                self.builder.build_unconditional_branch(else_bb).unwrap();
                self.builder.position_at_end(else_bb);
                let mut branch_ctx = FnCtx { function: ctx.function, vars: ctx.vars.clone(), var_types: ctx.var_types.clone(), is_ptr: ctx.is_ptr.clone() };
                for st in body {
                    self.gen_stmt(st, &mut branch_ctx);
                }
                if !self.block_terminated() {
                    self.builder.build_unconditional_branch(merge_bb).unwrap();
                }
            } else {
                self.builder.build_unconditional_branch(merge_bb).unwrap();
            }
            return;
        }
        let (cond, body) = &arms[i];
        let cond_val = self.gen_expr(cond, ctx).into_int_value();
        let then_bb = self.context.append_basic_block(ctx.function, "if_then");
        let next_bb = self.context.append_basic_block(ctx.function, "if_next");
        self.builder.build_conditional_branch(cond_val, then_bb, next_bb).unwrap();

        self.builder.position_at_end(then_bb);
        let mut branch_ctx = FnCtx { function: ctx.function, vars: ctx.vars.clone(), var_types: ctx.var_types.clone(), is_ptr: ctx.is_ptr.clone() };
        for st in body {
            self.gen_stmt(st, &mut branch_ctx);
        }
        if !self.block_terminated() {
            self.builder.build_unconditional_branch(merge_bb).unwrap();
        }

        self.builder.position_at_end(next_bb);
        self.gen_if_chain(arms, else_body, i + 1, merge_bb, ctx);
    }

    fn gen_while(&self, cond: &Expr, body: &[Stmt], ctx: &mut FnCtx<'ctx>) {
        let header_bb = self.context.append_basic_block(ctx.function, "while_header");
        let body_bb = self.context.append_basic_block(ctx.function, "while_body");
        let after_bb = self.context.append_basic_block(ctx.function, "while_after");
        self.builder.build_unconditional_branch(header_bb).unwrap();

        self.builder.position_at_end(header_bb);
        let cond_val = self.gen_expr(cond, ctx).into_int_value();
        self.builder.build_conditional_branch(cond_val, body_bb, after_bb).unwrap();

        self.builder.position_at_end(body_bb);
        for st in body {
            self.gen_stmt(st, ctx);
        }
        if !self.block_terminated() {
            self.builder.build_unconditional_branch(header_bb).unwrap();
        }

        self.builder.position_at_end(after_bb);
    }

    fn gen_for(&self, var: &str, iter: &Expr, body: &[Stmt], ctx: &mut FnCtx<'ctx>) {
        let (lo, hi) = match iter {
            Expr::Range(lo, hi) => (lo, hi),
            _ => panic!("Kernel LLVM codegen: for-loop requires a range (a..b)"),
        };
        let lo_val = self.gen_expr(lo, ctx).into_int_value();
        let hi_val = self.gen_expr(hi, ctx).into_int_value();
        let alloca = self.builder.build_alloca(self.context.i64_type(), var).unwrap();
        self.builder.build_store(alloca, lo_val).unwrap();

        let header_bb = self.context.append_basic_block(ctx.function, "for_header");
        let body_bb = self.context.append_basic_block(ctx.function, "for_body");
        let after_bb = self.context.append_basic_block(ctx.function, "for_after");
        self.builder.build_unconditional_branch(header_bb).unwrap();

        self.builder.position_at_end(header_bb);
        let cur = self.builder.build_load(self.context.i64_type(), alloca, var).unwrap().into_int_value();
        let cond = self.builder.build_int_compare(IntPredicate::SLT, cur, hi_val, "forcond").unwrap();
        self.builder.build_conditional_branch(cond, body_bb, after_bb).unwrap();

        self.builder.position_at_end(body_bb);
        let mut loop_ctx = FnCtx { function: ctx.function, vars: ctx.vars.clone(), var_types: ctx.var_types.clone(), is_ptr: ctx.is_ptr.clone() };
        loop_ctx.vars.insert(var.to_string(), alloca);
        loop_ctx.var_types.insert(var.to_string(), "int".to_string());
        for st in body {
            self.gen_stmt(st, &mut loop_ctx);
        }
        if !self.block_terminated() {
            let cur2 = self.builder.build_load(self.context.i64_type(), alloca, var).unwrap().into_int_value();
            let next = self.builder.build_int_add(cur2, self.context.i64_type().const_int(1, false), "forinc").unwrap();
            self.builder.build_store(alloca, next).unwrap();
            self.builder.build_unconditional_branch(header_bb).unwrap();
        }

        self.builder.position_at_end(after_bb);
    }
}

fn arg_expr(a: &CallArg) -> &Expr {
    match a {
        CallArg::Positional(e) | CallArg::Named(_, e) => e,
    }
}

/// Compiles `program` straight to a native object file at `out_obj_path`
/// for the HOST's own target triple (no cross-compilation support in
/// v1.x -- a named, deliberately out-of-scope-for-now gap: cross-
/// targeting is a real, separate feature, not a side effect of "just
/// works" the way it might be for an interpreter).
pub fn compile_to_object(program: &Program, out_obj_path: &Path) {
    Target::initialize_native(&InitializationConfig::default())
        .unwrap_or_else(|e| panic!("Kernel LLVM codegen: failed to initialize the native LLVM target: {}", e));

    // Created BEFORE codegen.generate() runs (not after, as originally
    // written) specifically so its TargetData is available DURING
    // codegen -- vec_elem_byte_size needs a real ABI-size query for
    // Vec<T> element types beyond the hand-known primitives (structs,
    // fixed-size arrays, nested Vecs), and that requires a real
    // TargetMachine/TargetData, not just a guess.
    let triple = TargetMachine::get_default_triple();
    let target = Target::from_triple(&triple).unwrap_or_else(|e| panic!("Kernel LLVM codegen: no LLVM target for {}: {}", triple, e));
    let target_machine = target
        .create_target_machine(
            &triple,
            TargetMachine::get_host_cpu_name().to_str().unwrap_or("generic"),
            TargetMachine::get_host_cpu_features().to_str().unwrap_or(""),
            OptimizationLevel::Default,
            RelocMode::PIC,
            CodeModel::Default,
        )
        .unwrap_or_else(|| panic!("Kernel LLVM codegen: could not create a target machine for {}", triple));
    let target_data = target_machine.get_target_data();

    let context = Context::create();
    let mut codegen = LlvmCodegen::new(&context, "kernel_module", target_data);
    codegen.generate(program);

    if let Err(e) = codegen.module.verify() {
        panic!(
            "Kernel LLVM codegen: the generated LLVM module failed verification -- this is a real \
             codegen bug (not a Kernel-level error in the input program):\n{}",
            e.to_string()
        );
    }

    target_machine
        .write_to_file(&codegen.module, FileType::Object, out_obj_path)
        .unwrap_or_else(|e| panic!("Kernel LLVM codegen: failed writing the object file: {}", e));
}
