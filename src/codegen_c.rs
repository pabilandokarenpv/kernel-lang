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

pub struct Codegen {
    structs: HashSet<String>,
    functions: HashMap<String, FnDecl>,
}

impl Codegen {
    pub fn new(program: &Program) -> Self {
        let mut structs = HashSet::new();
        let mut functions = HashMap::new();
        for item in &program.items {
            match item {
                Item::Struct(s) => {
                    structs.insert(s.name.clone());
                }
                Item::Function(f) => {
                    functions.insert(f.name.clone(), f.clone());
                }
                _ => {}
            }
        }
        Codegen { structs, functions }
    }

    fn c_type(&self, ty: &str) -> String {
        match ty.trim_start_matches('&').trim() {
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
            Expr::Binary(..) => "long".to_string(),
            Expr::Unary(UnaryOp::Neg, inner) => self.infer_c_type(inner, var_types),
            Expr::Unary(..) => "long".to_string(),
            Expr::Field(..) => "long".to_string(), // v1: only primitive fields are read this way
            Expr::Ident(name) => var_types.get(name).cloned().unwrap_or_else(|| "long".to_string()),
            Expr::Borrow(_) | Expr::Range(..) => "long".to_string(),
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
        out.push_str("const char *kernel_strcat(const char *a, const char *b);\n\n");

        // struct definitions
        for item in &program.items {
            if let Item::Struct(s) = item {
                out.push_str(&format!("struct {} {{\n", s.name));
                for (fname, fty) in &s.fields {
                    out.push_str(&format!("    {} {};\n", self.c_type(fty), fname));
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
                    out.push_str(&format!(
                        "static const {} {} = {};\n",
                        self.c_type(ty),
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
            Some(t) => self.c_type(t),
            None => "void".to_string(),
        };
        let params: Vec<String> = f
            .params
            .iter()
            .map(|p| {
                let base = self.c_type(&p.ty);
                if p.borrowed {
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
            Expr::Borrow(inner) => format!("(&{})", self.expr(inner, is_ptr, var_types)),
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
        match a {
            CallArg::Positional(e) => self.expr(e, is_ptr, var_types),
            CallArg::Named(_, e) => self.expr(e, is_ptr, var_types),
        }
    }

    fn indent(n: usize) -> String {
        "    ".repeat(n)
    }

    fn stmt(&self, s: &Stmt, is_ptr: &HashMap<String, bool>, depth: usize, var_types: &mut HashMap<String, String>) -> String {
        let pad = Self::indent(depth);
        match s {
            Stmt::Let { name, value, .. } => {
                let ty = self.infer_c_type(value, var_types);
                let rendered = format!("{}{} {} = {};\n", pad, ty, name, self.expr(value, is_ptr, var_types));
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
