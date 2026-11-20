// Kernel (.kn) C codegen — v1 backend.
//
// Lowers the checked AST directly to C (no separate IR yet — that's a
// deliberate v1 simplification: a formal pluggable-backend IR only earns
// its keep once there's a second backend, LLVM, to validate it against.
// v2 is when this gets refactored behind a real IR).
//
// Scope: the same core subset the interpreter/checker support — functions,
// structs, if/elif/else, while, for-in-range, the `unsafe` block, and the
// `print`/`abs`/`raw_write` builtins. `print` is special-cased at the
// statement level since C has no variadic-by-type dispatch.
//
// Type inference is a flat per-function variable-type map (`var_types`)
// threaded through statement/expression generation, not full lexical
// scoping — sufficient for v1's subset (see ARD-003 in
// architecture/ARDs/: the checker itself doesn't do control-flow-merged
// scoping yet either, so this matches the project's existing rigor level,
// not a new corner cut specifically for this fix).

use crate::ast::*;
use std::collections::{HashMap, HashSet};

// Parses a C array-type string built by `c_type` below, e.g. "long[5]" or
// "struct Point[3]", back into (element C type, size). Array C types are
// always exactly "<elem>[<N>]" (c_type never nests brackets -- v1.x has no
// nested arrays), so a single rfind('[') is sufficient and unambiguous even
// when <elem> is itself "struct Foo".
fn array_info(c_ty: &str) -> Option<(String, i64)> {
    let open = c_ty.rfind('[')?;
    if !c_ty.ends_with(']') {
        return None;
    }
    let elem = c_ty[..open].to_string();
    let n: i64 = c_ty[open + 1..c_ty.len() - 1].parse().ok()?;
    Some((elem, n))
}

pub struct Codegen {
    structs: HashSet<String>,
    functions: HashMap<String, FnDecl>,
    // struct name -> (field name -> field's Kernel-level type string), so
    // Expr::Field can resolve the real field type instead of guessing --
    // needed because the AST stores a struct's own type as a plain string
    // (see ast.rs), the same representation c_type() already converts.
    struct_fields: HashMap<String, HashMap<String, String>>,
    // Top-level `static`/`const` names -> their declared Kernel type, so
    // Expr::Ident can resolve a GLOBAL's real type instead of falling
    // back to "long" -- `var_types` (threaded through every statement/
    // expression call) only ever covers function-LOCAL names (params and
    // `let`s), so a bare global reference like `print(GREETING)` had no
    // way to know GREETING is a `str` and silently called
    // kernel_print_long on a pointer instead (a real, pre-existing bug,
    // invisible until a program actually printed a global string
    // directly -- caught by the LLVM backend's regression sweep cross-
    // checking against this backend, not something anyone wrote a test
    // for on purpose).
    global_types: HashMap<String, String>,
}

impl Codegen {
    pub fn new(program: &Program) -> Self {
        let mut structs = HashSet::new();
        let mut functions = HashMap::new();
        let mut struct_fields = HashMap::new();
        let mut global_types = HashMap::new();
        for item in &program.items {
            match item {
                Item::Struct(s) => {
                    structs.insert(s.name.clone());
                    struct_fields.insert(
                        s.name.clone(),
                        s.fields.iter().map(|(n, t)| (n.clone(), t.clone())).collect(),
                    );
                }
                Item::Function(f) => {
                    functions.insert(f.name.clone(), f.clone());
                }
                Item::Static { name, ty, .. } | Item::Const { name, ty, .. } => {
                    global_types.insert(name.clone(), ty.clone());
                }
                _ => {}
            }
        }
        Codegen { structs, functions, struct_fields, global_types }
    }

    fn c_type(&self, ty: &str) -> String {
        let trimmed = ty.trim_start_matches('&').trim();
        // Array type `[ElemType;N]` (see ast.rs's Expr::ArrayLit doc
        // comment) converts to the C "<elem>[<N>]" shape array_info expects
        // -- note this is NOT valid as a bare declaration prefix (C needs
        // `elem name[N]`, not `elem[N] name`); every call site that
        // declares a name with this type (Stmt::Let, struct fields,
        // fn_signature's params) special-cases it via array_info instead
        // of blindly doing `{ty} {name}`.
        if let Some(rest) = trimmed.strip_prefix('[') {
            if let Some(end) = rest.rfind(';') {
                if rest.ends_with(']') {
                    let elem_ty = &rest[..end];
                    let size_str = &rest[end + 1..rest.len() - 1];
                    return format!("{}[{}]", self.c_type(elem_ty), size_str);
                }
            }
        }
        match trimmed {
            "int" => "long".to_string(),
            "float" => "double".to_string(),
            "str" => "const char*".to_string(),
            "bool" => "bool".to_string(),
            other if self.structs.contains(other) => format!("struct {}", other),
            _ => "long".to_string(),
        }
    }

    /// Infers an expression's C type. `var_types` resolves identifiers to
    /// the type recorded when they were declared (by `Stmt::Let` or a
    /// function parameter) — without this, `Expr::Ident` type inference
    /// would have no way to know a variable holds a string vs an int,
    /// which is exactly the bug this map exists to fix (string
    /// concatenation via `+` silently mis-typed as `long` until a real
    /// variable-type environment was threaded through codegen).
    fn infer_c_type(&self, expr: &Expr, var_types: &HashMap<String, String>) -> String {
        match expr {
            Expr::Int(_) => "long".to_string(),
            Expr::Float(_) => "double".to_string(),
            Expr::Str(_) => "const char*".to_string(),
            Expr::Bool(_) => "bool".to_string(),
            Expr::Binary(l, BinOp::Add, r) => {
                let lt = self.infer_c_type(l, var_types);
                let rt = self.infer_c_type(r, var_types);
                if lt == "const char*" && rt == "const char*" {
                    "const char*".to_string()
                } else if lt == "double" || rt == "double" {
                    "double".to_string()
                } else {
                    "long".to_string()
                }
            }
            Expr::Binary(l, BinOp::Sub | BinOp::Mul | BinOp::Div, r) => {
                let lt = self.infer_c_type(l, var_types);
                let rt = self.infer_c_type(r, var_types);
                if lt == "double" || rt == "double" {
                    "double".to_string()
                } else {
                    "long".to_string()
                }
            }
            Expr::Binary(_, BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::Gt | BinOp::LtEq | BinOp::GtEq | BinOp::And | BinOp::Or, _) => {
                // Comparisons and logical and/or always produce a bool in
                // both C and Kernel -- this fell through to the generic
                // "long" catch-all below until this fix, so a `let`-bound
                // comparison/logical RESULT later printed directly (e.g.
                // `let ok = true && false; print(ok)`) emitted
                // kernel_print_long on a real C `bool`, printing a raw
                // 0/1 instead of "false"/"true" -- invisible until a
                // program actually printed one directly rather than just
                // using it as an `if` condition (where the 0/1-vs-bool
                // distinction never surfaces), caught by the LLVM
                // backend's cross-backend regression sweep producing a
                // real output MISMATCH against this exact case.
                "bool".to_string()
            }
            Expr::Binary(..) => "long".to_string(),
            Expr::Unary(UnaryOp::Neg, inner) => self.infer_c_type(inner, var_types),
            Expr::Unary(UnaryOp::Not, _) => "bool".to_string(),
            Expr::Unary(..) => "long".to_string(),
            Expr::Field(obj, field) => {
                // Resolve the base's struct type, then look up the field's
                // declared Kernel type and convert it -- falls back to
                // "long" only if the base isn't a recognized struct type
                // (e.g. inference already failed upstream), never silently
                // guessing for a struct we do know about.
                let obj_ty = self.infer_c_type(obj, var_types);
                obj_ty
                    .strip_prefix("struct ")
                    .and_then(|struct_name| self.struct_fields.get(struct_name))
                    .and_then(|fields| fields.get(field.as_str()))
                    .map(|kernel_ty| self.c_type(kernel_ty))
                    .unwrap_or_else(|| "long".to_string())
            }
            Expr::ArrayLit(elems) => {
                let elem_ty = elems
                    .first()
                    .map(|e| self.infer_c_type(e, var_types))
                    .unwrap_or_else(|| "long".to_string());
                format!("{}[{}]", elem_ty, elems.len())
            }
            Expr::Index(arr, _) => {
                let arr_ty = self.infer_c_type(arr, var_types);
                array_info(&arr_ty).map(|(elem, _)| elem).unwrap_or_else(|| "long".to_string())
            }
            Expr::Closure { .. } => unreachable!("Kernel codegen: Expr::Closure should always be erased by lambda_lift.rs before codegen runs"),
            Expr::Ident(name) => var_types.get(name).cloned().unwrap_or_else(|| {
                // var_types holds already-CONVERTED C type strings (e.g.
                // "long", "const char*"); global_types (a GLOBAL
                // static/const's name -> its RAW Kernel type string, see
                // this struct's doc comment) needs c_type() applied here
                // instead -- calling c_type() on var_types' values too
                // would double-convert an already-C string and silently
                // fall through to the "long" default for everything,
                // which is exactly the class of bug this fix exists to
                // remove, not reintroduce.
                self.global_types.get(name).map(|t| self.c_type(t)).unwrap_or_else(|| "long".to_string())
            }),
            // A borrow's C type is a pointer to whatever's being borrowed
            // (e.g. `&origin` where origin is `struct Point` -> "struct
            // Point*") -- previously this always returned "long" regardless
            // of the inner type, so a `let`-bound borrow like `let r1 =
            // &origin` got mistyped, and a later `peek(r1)` call then
            // passed that mistyped `long` where `peek`'s pointer parameter
            // expected an actual pointer (a real, pre-existing bug, caught
            // by batch-compiling every example through the C backend after
            // adding arrays -- see examples/multi_shared_borrow_ok.kn,
            // unrelated to arrays themselves but surfaced by the same
            // "compile everything" sweep).
            Expr::Borrow(inner, _) => format!("{}*", self.infer_c_type(inner, var_types)),
            Expr::Range(..) => "long".to_string(),
            Expr::Call { name, args } => {
                if name == "abs" {
                    return args.first().map(|a| match a {
                        CallArg::Positional(e) | CallArg::Named(_, e) => self.infer_c_type(e, var_types),
                    }).unwrap_or_else(|| "long".to_string());
                }
                if self.structs.contains(name) {
                    format!("struct {}", name)
                } else if let Some(f) = self.functions.get(name) {
                    match &f.return_ty {
                        Some(t) => self.c_type(t),
                        None => "void".to_string(),
                    }
                } else {
                    "long".to_string()
                }
            }
        }
    }

    pub fn generate(&self, program: &Program) -> String {
        let mut out = String::new();
        out.push_str("/* AUTO-GENERATED by the Kernel (.kn) v1 compiler. Do not edit by hand. */\n");
        out.push_str("#include <stdint.h>\n#include <stdbool.h>\n\n");
        out.push_str("void kernel_print_char(char c);\n");
        out.push_str("void kernel_print_str(const char *s);\n");
        out.push_str("void kernel_print_long(long n);\n");
        out.push_str("void kernel_print_float(double x);\n");
        out.push_str("void kernel_print_bool(bool b);\n");
        out.push_str("void kernel_print_space(void);\n");
        out.push_str("void kernel_print_newline(void);\n");
        out.push_str("long kernel_abs(long x);\n");
        out.push_str("double kernel_fabs(double x);\n");
        out.push_str("const char *kernel_strcat(const char *a, const char *b);\n");
        out.push_str("long kernel_strlen(const char *s);\n");
        out.push_str("_Noreturn void kernel_panic(const char *msg);\n\n");

        // struct definitions
        for item in &program.items {
            if let Item::Struct(s) = item {
                out.push_str(&format!("struct {} {{\n", s.name));
                for (fname, fty) in &s.fields {
                    let c_ty = self.c_type(fty);
                    // Array fields need C's "elem name[N]" declaration
                    // order, not "elem[N] name" -- see c_type's doc comment.
                    match array_info(&c_ty) {
                        Some((elem, n)) => out.push_str(&format!("    {} {}[{}];\n", elem, fname, n)),
                        None => out.push_str(&format!("    {} {};\n", c_ty, fname)),
                    }
                }
                out.push_str("};\n\n");
            }
        }

        // globals (static/const)
        for item in &program.items {
            match item {
                Item::Static { name, ty, value, .. } => {
                    out.push_str(&format!(
                        "static {} {} = {};\n",
                        self.c_type(ty),
                        name,
                        self.expr(value, &HashMap::new(), &HashMap::new())
                    ));
                }
                Item::Const { name, ty, value } => {
                    // c_type("str") already returns "const char*" -- a
                    // second "const" prefix (one `static const` too many)
                    // compiles fine (a harmless duplicate qualifier, not a
                    // real bug) but is sloppy generated C; skip the extra
                    // "const" specifically for that case.
                    let c_ty = self.c_type(ty);
                    let qualifier = if c_ty.starts_with("const ") { "static" } else { "static const" };
                    out.push_str(&format!(
                        "{} {} {} = {};\n",
                        qualifier,
                        c_ty,
                        name,
                        self.expr(value, &HashMap::new(), &HashMap::new())
                    ));
                }
                _ => {}
            }
        }
        out.push('\n');

        // forward declarations so call order / recursion never matters
        for item in &program.items {
            if let Item::Function(f) = item {
                out.push_str(&self.fn_signature(f));
                out.push_str(";\n");
            }
        }
        out.push('\n');

        // function bodies — Kernel's `main` becomes `kmain`, the symbol
        // boot.s calls into after setting up the stack.
        for item in &program.items {
            if let Item::Function(f) = item {
                out.push_str(&self.fn_signature(f));
                out.push_str(" {\n");
                let is_ptr = self.param_pointer_map(f);
                let mut var_types: HashMap<String, String> = f
                    .params
                    .iter()
                    .map(|p| (p.name.clone(), self.c_type(&p.ty)))
                    .collect();
                for stmt in &f.body {
                    out.push_str(&self.stmt(stmt, &is_ptr, 1, &mut var_types));
                }
                out.push_str("}\n\n");
            }
        }

        out
    }

    fn fn_signature(&self, f: &FnDecl) -> String {
        let name = if f.name == "main" { "kmain".to_string() } else { f.name.clone() };
        let ret = match &f.return_ty {
            Some(t) => {
                let c_ty = self.c_type(t);
                if array_info(&c_ty).is_some() {
                    // Plain C has no by-value array return (and no caller-
                    // visible "borrow" to return either) -- fail loudly at
                    // codegen time rather than emit invalid/misleading C.
                    // Named v1.x gap: wrap the array in a struct, or take
                    // an output array parameter, instead.
                    panic!(
                        "Kernel codegen: function `{}` returns an array type ({}) directly -- \
                         not supported in the C backend for v1.x; wrap it in a struct or take \
                         an output parameter instead",
                        f.name, t
                    );
                }
                c_ty
            }
            None => "void".to_string(),
        };
        let params: Vec<String> = f
            .params
            .iter()
            .map(|p| {
                let base = self.c_type(&p.ty);
                if let Some((elem, n)) = array_info(&base) {
                    // Array parameters always decay to a pointer in C no
                    // matter how they're declared -- so unlike scalar/
                    // struct params there's no `borrowed` distinction to
                    // render here; see ast.rs's Expr::ArrayLit doc comment
                    // for why this is still sound at the Kernel level.
                    format!("{} {}[{}]", elem, p.name, n)
                } else if p.borrowed {
                    format!("{}* {}", base, p.name)
                } else {
                    format!("{} {}", base, p.name)
                }
            })
            .collect();
        format!("{} {}({})", ret, name, params.join(", "))
    }

    fn param_pointer_map(&self, f: &FnDecl) -> HashMap<String, bool> {
        f.params.iter().map(|p| (p.name.clone(), p.borrowed)).collect()
    }

    fn accessor<'a>(&self, base: &Expr, is_ptr: &HashMap<String, bool>) -> &'a str {
        if let Expr::Ident(name) = base {
            if *is_ptr.get(name).unwrap_or(&false) {
                return "->";
            }
        }
        "."
    }

    fn binop(&self, op: BinOp) -> &'static str {
        use BinOp::*;
        match op {
            Add => "+", Sub => "-", Mul => "*", Div => "/", Mod => "%",
            Eq => "==", NotEq => "!=", Lt => "<", Gt => ">", LtEq => "<=", GtEq => ">=",
            And => "&&", Or => "||",
        }
    }

    fn expr(&self, e: &Expr, is_ptr: &HashMap<String, bool>, var_types: &HashMap<String, String>) -> String {
        match e {
            Expr::Int(n) => n.to_string(),
            Expr::Float(x) => format!("{:.2}", x),
            Expr::Str(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
            Expr::Bool(b) => (if *b { "true" } else { "false" }).to_string(),
            Expr::Ident(name) => name.clone(),
            Expr::Range(..) => panic!("Kernel codegen: range only valid as a for-loop iterator"),
            Expr::Unary(UnaryOp::Neg, inner) => format!("(-{})", self.expr(inner, is_ptr, var_types)),
            Expr::Unary(UnaryOp::Not, inner) => format!("(!{})", self.expr(inner, is_ptr, var_types)),
            Expr::Binary(l, op, r) => {
                if *op == BinOp::Add
                    && self.infer_c_type(l, var_types) == "const char*"
                    && self.infer_c_type(r, var_types) == "const char*"
                {
                    return format!(
                        "kernel_strcat({}, {})",
                        self.expr(l, is_ptr, var_types),
                        self.expr(r, is_ptr, var_types)
                    );
                }
                format!(
                    "({} {} {})",
                    self.expr(l, is_ptr, var_types),
                    self.binop(*op),
                    self.expr(r, is_ptr, var_types)
                )
            }
            Expr::Field(obj, field) => {
                format!("{}{}{}", self.expr(obj, is_ptr, var_types), self.accessor(obj, is_ptr), field)
            }
            Expr::ArrayLit(elems) => {
                // Bare C brace-init -- valid directly in a `let`/struct-
                // field initializer position (the two call sites that
                // special-case array declarations), invalid as a general
                // expression; see ast.rs's Expr::ArrayLit doc comment for
                // why that's an accepted v1.x rough edge rather than a
                // Kernel-level check.
                let rendered: Vec<String> = elems.iter().map(|e| self.expr(e, is_ptr, var_types)).collect();
                format!("{{{}}}", rendered.join(", "))
            }
            Expr::Index(arr, idx) => {
                let arr_ty = self.infer_c_type(arr, var_types);
                let size = array_info(&arr_ty).map(|(_, n)| n);
                let arr_c = self.expr(arr, is_ptr, var_types);
                let idx_c = self.expr(idx, is_ptr, var_types);
                match size {
                    // Runtime bounds check via a GNU statement expression so
                    // this can appear in any expression position, not just
                    // a standalone statement -- halts via kernel_panic (see
                    // runtime.c) on an out-of-range index instead of
                    // reading/corrupting adjacent memory (the actual C
                    // array-index UB this exists to prevent).
                    Some(n) => format!(
                        "({{ long __kn_idx = ({}); if (__kn_idx < 0 || __kn_idx >= {}) {{ kernel_panic(\"array index out of bounds\"); }} {}[__kn_idx]; }})",
                        idx_c, n, arr_c
                    ),
                    None => panic!(
                        "Kernel codegen: couldn't determine the array size for `{}[...]` to emit a bounds check -- \
                         this indicates inference failed upstream (see infer_c_type), a real bug, not a silent skip",
                        arr_c
                    ),
                }
            }
            Expr::Borrow(inner, _exclusive) => format!("(&{})", self.expr(inner, is_ptr, var_types)),
            Expr::Closure { .. } => unreachable!("Kernel codegen: Expr::Closure should always be erased by lambda_lift.rs before codegen runs"),
            Expr::Call { name, args } => self.call_expr(name, args, is_ptr, var_types),
        }
    }

    fn call_expr(&self, name: &str, args: &[CallArg], is_ptr: &HashMap<String, bool>, var_types: &HashMap<String, String>) -> String {
        match name {
            "abs" => {
                let a = args.first().map(|a| self.call_arg(a, is_ptr, var_types)).unwrap_or_default();
                let arg_ty = args.first().and_then(|a| match a {
                    CallArg::Positional(e) | CallArg::Named(_, e) => Some(self.infer_c_type(e, var_types)),
                }).unwrap_or_default();
                if arg_ty == "double" {
                    format!("kernel_fabs({})", a)
                } else {
                    format!("kernel_abs({})", a)
                }
            }
            "len" => {
                let arg = args.first().unwrap_or_else(|| panic!("Kernel codegen: len() requires exactly one argument"));
                let arg_expr = match arg {
                    CallArg::Positional(e) | CallArg::Named(_, e) => e,
                };
                let ty = self.infer_c_type(arg_expr, var_types);
                if let Some((_, n)) = array_info(&ty) {
                    // The array's size is a compile-time constant already
                    // known from its type -- no runtime computation needed
                    // at all, unlike the string case below.
                    n.to_string()
                } else if ty == "const char*" {
                    format!("kernel_strlen({})", self.call_arg(arg, is_ptr, var_types))
                } else {
                    panic!(
                        "Kernel codegen: len() only supports arrays and strings in v1.x, found a `{}`",
                        ty
                    );
                }
            }
            "read_file" | "write_file" => panic!(
                "Kernel codegen: `{}` is interpreter-only (dev-mode file I/O) -- there's no filesystem \
                 in the C backend's target (a freestanding kernel booting under QEMU/real hardware), \
                 so this can't be compiled. Use `kernel run` for programs that need it.",
                name
            ),
            "vec_new" | "vec_push" | "vec_get" | "vec_len" | "vec_set" => panic!(
                "Kernel codegen: `{}` (Vec<T>) is interpreter-only in v1.x -- it needs a real heap \
                 allocator (malloc/realloc), and the C backend's target (a freestanding kernel booting \
                 under QEMU/real hardware) has no heap yet. Use `kernel run` for programs that need a \
                 Vec, or a fixed-size array (`[T;N]`) if the size is known at compile time.",
                name
            ),
            "raw_write" => {
                let addr = args.get(0).map(|a| self.call_arg(a, is_ptr, var_types)).unwrap_or_default();
                let byte = args.get(1).map(|a| self.call_arg(a, is_ptr, var_types)).unwrap_or_default();
                format!("(*(volatile unsigned char*)({}) = (unsigned char)({}))", addr, byte)
            }
            _ if self.structs.contains(name) => {
                let fields: Vec<String> = args
                    .iter()
                    .filter_map(|a| match a {
                        CallArg::Named(fname, e) => Some(format!(".{} = {}", fname, self.expr(e, is_ptr, var_types))),
                        CallArg::Positional(_) => None,
                    })
                    .collect();
                format!("(struct {}){{{}}}", name, fields.join(", "))
            }
            _ => {
                let joined: Vec<String> = args.iter().map(|a| self.call_arg(a, is_ptr, var_types)).collect();
                format!("{}({})", name, joined.join(", "))
            }
        }
    }

    fn call_arg(&self, a: &CallArg, is_ptr: &HashMap<String, bool>, var_types: &HashMap<String, String>) -> String {
        let e = match a {
            CallArg::Positional(e) => e,
            CallArg::Named(_, e) => e,
        };
        if let Expr::ArrayLit(elems) = e {
            // Bare brace-init (what self.expr() renders for Expr::ArrayLit)
            // is only valid C directly in a declaration/designated-field
            // initializer -- a bare function-call ARGUMENT needs a C99
            // compound-literal cast instead (`(long[]){1, 2, 3}`), which is
            // valid in any general expression position and decays to the
            // same pointer the callee's array parameter expects.
            let elem_ty = elems
                .first()
                .map(|el| self.infer_c_type(el, var_types))
                .unwrap_or_else(|| "long".to_string());
            let rendered: Vec<String> = elems.iter().map(|el| self.expr(el, is_ptr, var_types)).collect();
            return format!("({}[]){{{}}}", elem_ty, rendered.join(", "));
        }
        self.expr(e, is_ptr, var_types)
    }

    fn indent(n: usize) -> String {
        "    ".repeat(n)
    }

    fn stmt(&self, s: &Stmt, is_ptr: &HashMap<String, bool>, depth: usize, var_types: &mut HashMap<String, String>) -> String {
        let pad = Self::indent(depth);
        match s {
            Stmt::Let { name, value, .. } => {
                let ty = self.infer_c_type(value, var_types);
                let rendered = match array_info(&ty) {
                    // C array declarations put the size after the name
                    // (`long name[5] = {...};`), not before it like every
                    // other type -- see c_type's doc comment.
                    Some((elem, n)) => format!(
                        "{}{} {}[{}] = {};\n",
                        pad, elem, name, n, self.expr(value, is_ptr, var_types)
                    ),
                    None => format!("{}{} {} = {};\n", pad, ty, name, self.expr(value, is_ptr, var_types)),
                };
                var_types.insert(name.clone(), ty);
                rendered
            }
            Stmt::Assign { name, op, value } => {
                let op_str = match op {
                    AssignOp::Set => "=",
                    AssignOp::Add => "+=",
                    AssignOp::Sub => "-=",
                };
                if *op == AssignOp::Add && var_types.get(name).map(|t| t.as_str()) == Some("const char*") {
                    // string += string has the same "no native C operator"
                    // problem as string + string — route through the same
                    // runtime helper instead of emitting invalid C.
                    return format!(
                        "{}{} = kernel_strcat({}, {});\n",
                        pad, name, name, self.expr(value, is_ptr, var_types)
                    );
                }
                format!("{}{} {} {};\n", pad, name, op_str, self.expr(value, is_ptr, var_types))
            }
            Stmt::FieldAssign { object, field, value } => {
                format!(
                    "{}{}{}{} = {};\n",
                    pad,
                    self.expr(object, is_ptr, var_types),
                    self.accessor(object, is_ptr),
                    field,
                    self.expr(value, is_ptr, var_types)
                )
            }
            Stmt::IndexAssign { array, index, value } => {
                let arr_ty = self.infer_c_type(array, var_types);
                let size = array_info(&arr_ty).map(|(_, n)| n).unwrap_or_else(|| {
                    panic!(
                        "Kernel codegen: couldn't determine the array size for an indexed assignment -- \
                         this indicates inference failed upstream (see infer_c_type), a real bug, not a silent skip"
                    )
                });
                format!(
                    "{}{{ long __kn_idx = ({}); if (__kn_idx < 0 || __kn_idx >= {}) {{ kernel_panic(\"array index out of bounds\"); }} {}[__kn_idx] = {}; }}\n",
                    pad,
                    self.expr(index, is_ptr, var_types),
                    size,
                    self.expr(array, is_ptr, var_types),
                    self.expr(value, is_ptr, var_types)
                )
            }
            Stmt::If { arms, else_body } => {
                let mut out = String::new();
                for (i, (cond, body)) in arms.iter().enumerate() {
                    let kw = if i == 0 { "if" } else { "else if" };
                    out.push_str(&format!("{}{} ({}) {{\n", pad, kw, self.expr(cond, is_ptr, var_types)));
                    let mut branch_types = var_types.clone();
                    for st in body {
                        out.push_str(&self.stmt(st, is_ptr, depth + 1, &mut branch_types));
                    }
                    out.push_str(&format!("{}}}\n", pad));
                }
                if let Some(body) = else_body {
                    out.push_str(&format!("{}else {{\n", pad));
                    let mut branch_types = var_types.clone();
                    for st in body {
                        out.push_str(&self.stmt(st, is_ptr, depth + 1, &mut branch_types));
                    }
                    out.push_str(&format!("{}}}\n", pad));
                }
                out
            }
            Stmt::While { cond, body } => {
                let mut out = format!("{}while ({}) {{\n", pad, self.expr(cond, is_ptr, var_types));
                let mut branch_types = var_types.clone();
                for st in body {
                    out.push_str(&self.stmt(st, is_ptr, depth + 1, &mut branch_types));
                }
                out.push_str(&format!("{}}}\n", pad));
                out
            }
            Stmt::For { var, iter, body } => {
                let (lo, hi) = match iter {
                    Expr::Range(lo, hi) => (self.expr(lo, is_ptr, var_types), self.expr(hi, is_ptr, var_types)),
                    _ => panic!("Kernel codegen: for-loop requires a range (a..b)"),
                };
                let mut out = format!(
                    "{}for (long {} = {}; {} < {}; {}++) {{\n",
                    pad, var, lo, var, hi, var
                );
                let mut branch_types = var_types.clone();
                branch_types.insert(var.clone(), "long".to_string());
                for st in body {
                    out.push_str(&self.stmt(st, is_ptr, depth + 1, &mut branch_types));
                }
                out.push_str(&format!("{}}}\n", pad));
                out
            }
            Stmt::Return(expr) => match expr {
                Some(e) => format!("{}return {};\n", pad, self.expr(e, is_ptr, var_types)),
                None => format!("{}return;\n", pad),
            },
            Stmt::Unsafe(body) => {
                let mut out = format!("{}{{\n", pad);
                for st in body {
                    out.push_str(&self.stmt(st, is_ptr, depth + 1, var_types));
                }
                out.push_str(&format!("{}}}\n", pad));
                out
            }
            Stmt::Expr(e) => {
                if let Expr::Call { name, args } = e {
                    if name == "print" {
                        return self.print_stmt(args, is_ptr, depth, var_types);
                    }
                }
                format!("{}{};\n", pad, self.expr(e, is_ptr, var_types))
            }
        }
    }

    fn print_stmt(&self, args: &[CallArg], is_ptr: &HashMap<String, bool>, depth: usize, var_types: &HashMap<String, String>) -> String {
        let pad = Self::indent(depth);
        let mut out = String::new();
        for (i, a) in args.iter().enumerate() {
            if i > 0 {
                out.push_str(&format!("{}kernel_print_space();\n", pad));
            }
            let e = match a {
                CallArg::Positional(e) => e,
                CallArg::Named(_, e) => e,
            };
            let ty = self.infer_c_type(e, var_types);
            let helper = match ty.as_str() {
                "const char*" => "kernel_print_str",
                "bool" => "kernel_print_bool",
                "double" => "kernel_print_float",
                _ => "kernel_print_long",
            };
            out.push_str(&format!("{}{}({});\n", pad, helper, self.expr(e, is_ptr, var_types)));
        }
        out.push_str(&format!("{}kernel_print_newline();\n", pad));
        out
    }
}
