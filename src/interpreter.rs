// Kernel (.kn) tree-walking interpreter — v1 core subset.
// This exists for two reasons: (1) fast dev-mode execution before the C
// codegen backend exists, matching the `kernel run` vs `kernel build` split
// from the design doc, and (2) it IS the ground-truth oracle for the LLM
// interpretability corpus later — run a random program through this, record
// every variable/branch decision, that's the labeled training data.

use crate::ast::*;
use std::collections::HashMap;
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    Struct(String, HashMap<String, Value>),
    Array(Vec<Value>),
    // `Vec<T>` (v1.x, INTERPRETER-ONLY so far -- real codegen support in
    // codegen_c.rs/codegen_llvm.rs is named future work, not implemented
    // yet). A growable collection, unlike Array's fixed compile-time
    // size -- built via vec_new()/vec_push()/vec_get()/vec_len()/
    // vec_set() (see eval_call). Mutation is modeled the SAME way
    // Stmt::IndexAssign already mutates an Array: get the variable's
    // current value, clone+mutate the clone, write the result back
    // under the same name -- never real aliasing (this interpreter
    // doesn't model aliasing at all, see Expr::Borrow's eval arm).
    Vec(Vec<Value>),
    Unit,
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Int(n) => write!(f, "{}", n),
            // fixed 2-decimal-place formatting — chosen specifically so it
            // can be matched exactly by the freestanding C runtime's
            // kernel_print_float (no printf/libm available there), rather
            // than Rust's shortest-roundtrip float formatting which has no
            // simple freestanding-C equivalent.
            Value::Float(x) => write!(f, "{:.2}", x),
            Value::Str(s) => write!(f, "{}", s),
            Value::Bool(b) => write!(f, "{}", b),
            Value::Struct(name, fields) => {
                write!(f, "{}(", name)?;
                let mut first = true;
                for (k, v) in fields {
                    if !first {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}: {}", k, v)?;
                    first = false;
                }
                write!(f, ")")
            }
            Value::Unit => write!(f, "()"),
            // Not used by `print()` -- eval_call's "print" builtin rejects a
            // whole-array argument loudly (see its own comment) so both
            // backends stay consistent, since the C codegen backend has no
            // generic array-printing support. Still implemented here for
            // Debug/completeness (e.g. a future caller formatting one for
            // diagnostics), matching standard array-literal notation.
            Value::Array(items) => {
                write!(f, "[")?;
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", v)?;
                }
                write!(f, "]")
            }
            // Not used by `print()` either, same restriction and same
            // reason as Value::Array above -- codegen has no generic
            // collection-formatter, so index/vec_get into it instead.
            Value::Vec(items) => {
                write!(f, "[")?;
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", v)?;
                }
                write!(f, "]")
            }
        }
    }
}

enum Flow {
    Normal,
    Return(Value),
}

pub struct Interpreter {
    functions: HashMap<String, FnDecl>,
    structs: HashMap<String, StructDecl>,
    globals: HashMap<String, Value>,
    scopes: Vec<HashMap<String, Value>>,
    // Tracks, per currently-live scope (parallel to `scopes` -- pushed/
    // popped at every one of the same sites), which local names are
    // bound to a persistent borrow (`let r = &x`/`let r = &mut x`) and
    // what variable they alias. Closes a real gap: this interpreter
    // never modeled aliasing at the VALUE level at all (see
    // Expr::Borrow's eval arm -- `&x` just evaluates `x`, producing an
    // independent clone with no memory of where it came from), so
    // mutating THROUGH a borrowed reference (`let r = &mut p; r.x =
    // 5;`) silently mutated only `r`'s own disconnected copy, never
    // `p` -- caught for real: `print(p.x)` after exactly that sequence
    // printed the ORIGINAL value, not the mutated one, while the C and
    // LLVM native backends (which have real addresses) both mutate `p`
    // correctly. Scoped deliberately narrow: only resolves a DIRECT
    // `let NAME = &TARGET` alias (one level, TARGET itself a plain
    // ident) -- consulted by Stmt::FieldAssign/Stmt::IndexAssign's
    // resolve_alias helper before mutating. Borrowed FUNCTION
    // PARAMETERS aliasing back to the CALLER's variable remain a
    // separate, narrower, still-real gap, not fixed here (would need
    // threading alias info across the call boundary itself).
    ref_aliases: Vec<HashMap<String, String>>,
    pub trace: Vec<String>,  // execution trace: lets, assigns, branches taken
    pub output: Vec<String>, // captured `print` lines (one entry per print statement)
}

impl Interpreter {
    pub fn new() -> Self {
        Interpreter {
            functions: HashMap::new(),
            structs: HashMap::new(),
            globals: HashMap::new(),
            scopes: Vec::new(),
            ref_aliases: Vec::new(),
            trace: Vec::new(),
            output: Vec::new(),
        }
    }

    pub fn load(&mut self, program: &Program) {
        for item in &program.items {
            match item {
                Item::Function(f) => {
                    self.functions.insert(f.name.clone(), f.clone());
                }
                Item::Struct(s) => {
                    self.structs.insert(s.name.clone(), s.clone());
                }
                Item::Static { name, value, .. } | Item::Const { name, value, .. } => {
                    let v = self.eval(value);
                    self.globals.insert(name.clone(), v);
                }
                Item::Import(_) => unreachable!("Kernel interpreter: Item::Import should always be resolved by modules.rs before this runs"),
            }
        }
    }

    pub fn run_main(&mut self) -> Value {
        let main = self
            .functions
            .get("main")
            .cloned()
            .expect("Kernel interpreter: no `main` function defined");
        self.scopes.push(HashMap::new());
        self.ref_aliases.push(HashMap::new());
        let result = match self.exec_block(&main.body) {
            Flow::Return(v) => v,
            Flow::Normal => Value::Unit,
        };
        self.scopes.pop();
        self.ref_aliases.pop();
        result
    }

    fn get_var(&self, name: &str) -> Value {
        for scope in self.scopes.iter().rev() {
            if let Some(v) = scope.get(name) {
                return v.clone();
            }
        }
        if let Some(v) = self.globals.get(name) {
            return v.clone();
        }
        panic!("Kernel interpreter: undefined variable `{}`", name);
    }

    fn set_var(&mut self, name: &str, value: Value) {
        for scope in self.scopes.iter_mut().rev() {
            if scope.contains_key(name) {
                scope.insert(name.to_string(), value);
                return;
            }
        }
        // Falls back to a REAL global write when `name` isn't any
        // currently-live local -- a genuine, previously-real bug fix:
        // this used to unconditionally create a brand-new LOCAL binding
        // here instead, which meant `static mut COUNTER` mutations
        // (e.g. `COUNTER += 1` inside a function) never actually
        // persisted back to the global at all -- each call started
        // fresh from the global's ORIGINAL value, incremented a
        // throwaway local shadow, and returned that, silently
        // discarding it when the function returned. Caught for real:
        // calling the same `bump()` function three times in a row
        // printed `1, 1, 1` on the interpreter while the C and LLVM
        // native backends correctly printed `1, 2, 3` (both of those
        // resolve a `static` identifier to one real, persistent memory
        // location at compile time, so they never had this bug) -- a
        // genuine cross-backend disagreement this project's own
        // regression discipline exists to catch. Every current call
        // site of set_var (Stmt::Assign/FieldAssign/IndexAssign/
        // vec_push/vec_set) only ever targets a name that already
        // exists as either a local (caught by the loop above) or a
        // global (`let` is the only thing that introduces a BRAND NEW
        // binding, and it writes directly into `self.scopes` itself,
        // never through this function) -- so there is no legitimate
        // case left that this fallback needs to treat as "a new local"
        // instead.
        if self.globals.contains_key(name) {
            self.globals.insert(name.to_string(), value);
            return;
        }
        // new binding in the innermost scope (defensive fallback only --
        // see the reasoning above for why every real call site should
        // already be caught by one of the two branches above it)
        self.scopes.last_mut().unwrap().insert(name.to_string(), value);
    }

    // Records that local `name` is a persistent-borrow alias of
    // `target` -- called from Stmt::Let whenever `value` is a direct
    // `&TARGET`/`&mut TARGET` (TARGET itself a plain ident). See
    // `ref_aliases`'s own doc comment for the real bug this exists to
    // fix.
    fn set_alias(&mut self, name: &str, target: &str) {
        self.ref_aliases.last_mut().unwrap().insert(name.to_string(), target.to_string());
    }

    // Resolves `name` through its alias, if it has one (searching every
    // currently-live scope, innermost first, same lookup order as
    // get_var/set_var) -- returns `name` itself unchanged if it isn't a
    // known alias (the common case: a plain owned variable).
    fn resolve_alias(&self, name: &str) -> String {
        for scope in self.ref_aliases.iter().rev() {
            if let Some(target) = scope.get(name) {
                return target.clone();
            }
        }
        name.to_string()
    }

    fn exec_block(&mut self, stmts: &[Stmt]) -> Flow {
        for stmt in stmts {
            match self.exec_stmt(stmt) {
                Flow::Normal => continue,
                Flow::Return(v) => return Flow::Return(v),
            }
        }
        Flow::Normal
    }

    fn exec_stmt(&mut self, stmt: &Stmt) -> Flow {
        match stmt {
            Stmt::Let { name, value, .. } => {
                let v = self.eval(value);
                self.trace.push(format!("let {} = {}", name, v));
                self.scopes.last_mut().unwrap().insert(name.clone(), v);
                // See ref_aliases's doc comment: a direct `&TARGET`/
                // `&mut TARGET` (TARGET a plain ident) makes `name` an
                // alias, so a later mutation THROUGH it (FieldAssign/
                // IndexAssign) resolves to TARGET instead of silently
                // mutating only `name`'s own disconnected copy.
                if let Expr::Borrow(inner, _) = value {
                    if let Expr::Ident(target) = inner.as_ref() {
                        self.set_alias(name, target);
                    }
                }
                Flow::Normal
            }
            Stmt::Assign { name, op, value } => {
                let new_val = match op {
                    AssignOp::Set => self.eval(value),
                    AssignOp::Add => {
                        let cur = self.get_var(name);
                        let rhs = self.eval(value);
                        eval_binop(&cur, BinOp::Add, &rhs)
                    }
                    AssignOp::Sub => {
                        let cur = self.get_var(name);
                        let rhs = self.eval(value);
                        eval_binop(&cur, BinOp::Sub, &rhs)
                    }
                };
                self.trace.push(format!("{} = {}", name, new_val));
                self.set_var(name, new_val);
                Flow::Normal
            }
            Stmt::FieldAssign { object, field, value } => {
                if let Expr::Ident(obj_name) = object {
                    // Resolve THROUGH a persistent-borrow alias first
                    // (see ref_aliases's doc comment) -- `r.field = x`
                    // where `r` came from `let r = &mut p` must mutate
                    // `p`, not `r`'s own disconnected copy.
                    let real_name = self.resolve_alias(obj_name);
                    let v = self.eval(value);
                    let mut s = self.get_var(&real_name);
                    if let Value::Struct(_, ref mut fields) = s {
                        fields.insert(field.clone(), v);
                    }
                    self.set_var(&real_name, s);
                }
                Flow::Normal
            }
            Stmt::IndexAssign { array, index, value } => {
                // v1.x restricts the indexed target to a plain ident (same
                // scope boundary FieldAssign already has above) -- fails
                // loudly instead of silently no-op'ing like FieldAssign's
                // existing non-ident case does, since this is new surface
                // area and the project's own convention is to fail loud.
                let Expr::Ident(arr_name) = array else {
                    panic!("Kernel interpreter: `arr[i] = value` requires `arr` to be a plain variable in v1.x, found {:?}", array);
                };
                // Resolve THROUGH a persistent-borrow alias first, same
                // reasoning as FieldAssign above.
                let real_name = self.resolve_alias(arr_name);
                let i = as_int(&self.eval(index));
                let v = self.eval(value);
                let mut arr_val = self.get_var(&real_name);
                if let Value::Array(ref mut items) = arr_val {
                    if i < 0 || i as usize >= items.len() {
                        panic!("Kernel interpreter: array index {} out of bounds (len {})", i, items.len());
                    }
                    items[i as usize] = v;
                } else {
                    panic!("Kernel interpreter: indexed assignment on a non-array value");
                }
                self.set_var(&real_name, arr_val);
                Flow::Normal
            }
            Stmt::If { arms, else_body } => {
                for (cond, body) in arms {
                    if truthy(&self.eval(cond)) {
                        self.trace.push(format!("branch taken: if {:?}", cond));
                        self.scopes.push(HashMap::new());
        self.ref_aliases.push(HashMap::new());
                        let f = self.exec_block(body);
                        self.scopes.pop();
        self.ref_aliases.pop();
                        return f;
                    }
                }
                if let Some(body) = else_body {
                    self.trace.push("branch taken: else".to_string());
                    self.scopes.push(HashMap::new());
        self.ref_aliases.push(HashMap::new());
                    let f = self.exec_block(body);
                    self.scopes.pop();
        self.ref_aliases.pop();
                    return f;
                }
                Flow::Normal
            }
            Stmt::While { cond, body } => {
                while truthy(&self.eval(cond)) {
                    self.scopes.push(HashMap::new());
        self.ref_aliases.push(HashMap::new());
                    let f = self.exec_block(body);
                    self.scopes.pop();
        self.ref_aliases.pop();
                    if let Flow::Return(v) = f {
                        return Flow::Return(v);
                    }
                }
                Flow::Normal
            }
            Stmt::For { var, iter, body } => {
                if let Expr::Range(lo, hi) = iter {
                    let lo = as_int(&self.eval(lo));
                    let hi = as_int(&self.eval(hi));
                    for i in lo..hi {
                        self.scopes.push(HashMap::new());
        self.ref_aliases.push(HashMap::new());
                        self.scopes.last_mut().unwrap().insert(var.clone(), Value::Int(i));
                        let f = self.exec_block(body);
                        self.scopes.pop();
        self.ref_aliases.pop();
                        if let Flow::Return(v) = f {
                            return Flow::Return(v);
                        }
                    }
                }
                Flow::Normal
            }
            Stmt::Return(expr) => {
                let v = match expr {
                    Some(e) => self.eval(e),
                    None => Value::Unit,
                };
                Flow::Return(v)
            }
            Stmt::Unsafe(body) => self.exec_block(body),
            Stmt::Expr(e) => {
                self.eval(e);
                Flow::Normal
            }
        }
    }

    fn eval(&mut self, expr: &Expr) -> Value {
        match expr {
            Expr::Int(n) => Value::Int(*n),
            Expr::Float(x) => Value::Float(*x),
            Expr::Str(s) => Value::Str(s.clone()),
            Expr::Bool(b) => Value::Bool(*b),
            Expr::Ident(name) => self.get_var(name),
            Expr::Range(_, _) => panic!("Kernel interpreter: range only valid in for-loops"),
            Expr::Unary(op, e) => {
                let v = self.eval(e);
                match op {
                    UnaryOp::Neg => {
                        if is_float_val(&v) {
                            Value::Float(-as_float(&v))
                        } else {
                            Value::Int(-as_int(&v))
                        }
                    }
                    UnaryOp::Not => Value::Bool(!truthy(&v)),
                }
            }
            Expr::Binary(l, op, r) => {
                let lv = self.eval(l);
                let rv = self.eval(r);
                eval_binop(&lv, *op, &rv)
            }
            Expr::Field(obj, field) => {
                let v = self.eval(obj);
                if let Value::Struct(_, fields) = v {
                    fields.get(field).cloned().unwrap_or(Value::Unit)
                } else {
                    panic!("Kernel interpreter: field access on non-struct value");
                }
            }
            Expr::Borrow(e, _exclusive) => self.eval(e), // interpreter doesn't model aliasing, just the value
            Expr::Closure { .. } => unreachable!("Kernel interpreter: Expr::Closure should always be erased by lambda_lift.rs before the interpreter runs"),
            Expr::ArrayLit(elems) => Value::Array(elems.iter().map(|e| self.eval(e)).collect()),
            Expr::Index(arr, idx) => {
                let base = self.eval(arr);
                let i = as_int(&self.eval(idx));
                match base {
                    Value::Array(items) => {
                        if i < 0 || i as usize >= items.len() {
                            panic!("Kernel interpreter: array index {} out of bounds (len {})", i, items.len());
                        }
                        items[i as usize].clone()
                    }
                    other => panic!("Kernel interpreter: indexing into a non-array value {:?}", other),
                }
            }
            Expr::Call { name, args } => self.eval_call(name, args),
        }
    }

    fn eval_call(&mut self, name: &str, args: &[CallArg]) -> Value {
        // builtins
        match name {
            "print" => {
                let parts: Vec<String> = args
                    .iter()
                    .map(|a| {
                        let e = match a {
                            CallArg::Positional(e) => e,
                            CallArg::Named(_, e) => e,
                        };
                        let v = self.eval(e);
                        // Matches the C codegen backend's restriction (see
                        // print_stmt in codegen_c.rs): printing a WHOLE
                        // array isn't supported there (no generic
                        // array-formatter in the freestanding runtime), so
                        // the interpreter rejects it too rather than
                        // silently diverging between the two backends --
                        // index into it (`print(arr[i])`) instead.
                        if matches!(v, Value::Array(_) | Value::Vec(_)) {
                            panic!("Kernel interpreter: `print()` on a whole array/Vec isn't supported in v1.x (matches the C codegen backend's restriction) -- index into it, e.g. `print(arr[0])`/`vec_get(v, 0)`");
                        }
                        v.to_string()
                    })
                    .collect();
                self.output.push(parts.join(" "));
                return Value::Unit;
            }
            "abs" => {
                if let Some(CallArg::Positional(e)) = args.first() {
                    let v = self.eval(e);
                    return if is_float_val(&v) {
                        Value::Float(as_float(&v).abs())
                    } else {
                        Value::Int(as_int(&v).abs())
                    };
                }
            }
            "len" => {
                if let Some(CallArg::Positional(e)) = args.first() {
                    let v = self.eval(e);
                    return match v {
                        Value::Array(items) | Value::Vec(items) => Value::Int(items.len() as i64),
                        // Byte length, not Unicode codepoint count -- matches
                        // the C codegen backend's kernel_strlen exactly
                        // (strlen-style, no libc/Unicode awareness available
                        // in a freestanding environment), so both backends
                        // agree for any string this language can even
                        // represent (ASCII-range source text).
                        Value::Str(s) => Value::Int(s.len() as i64),
                        other => panic!(
                            "Kernel interpreter: len() only supports arrays, Vecs, and strings in v1.x, found {:?}",
                            other
                        ),
                    };
                }
            }
            // `Vec<T>` (v1.x, INTERPRETER-ONLY -- see Value::Vec's doc
            // comment for the full scope and why real codegen support is
            // separate future work, not silently implied here). Every
            // mutating builtin (vec_push/vec_set) requires its first
            // argument to be a plain identifier (or a `&`/`&mut` borrow
            // of one, unwrapped below) -- never an arbitrary expression
            // -- because mutation is modeled as get-clone-mutate-set-
            // back against that NAME, the same trick Stmt::IndexAssign
            // already uses for arrays; there's no real aliasing to write
            // through otherwise.
            "vec_new" => return Value::Vec(Vec::new()),
            "vec_len" => {
                if let Some(arg) = args.first() {
                    let e = match arg {
                        CallArg::Positional(e) | CallArg::Named(_, e) => e,
                    };
                    return match self.eval(e) {
                        Value::Vec(items) => Value::Int(items.len() as i64),
                        other => panic!("Kernel interpreter: vec_len() expects a Vec, found {:?}", other),
                    };
                }
            }
            "vec_get" => {
                if let (Some(va), Some(ia)) = (args.first(), args.get(1)) {
                    let ve = match va {
                        CallArg::Positional(e) | CallArg::Named(_, e) => e,
                    };
                    let ie = match ia {
                        CallArg::Positional(e) | CallArg::Named(_, e) => e,
                    };
                    let idx = as_int(&self.eval(ie));
                    return match self.eval(ve) {
                        Value::Vec(items) => {
                            if idx < 0 || idx as usize >= items.len() {
                                panic!("Kernel interpreter: vec_get() index {} out of bounds (len {})", idx, items.len());
                            }
                            items[idx as usize].clone()
                        }
                        other => panic!("Kernel interpreter: vec_get() expects a Vec, found {:?}", other),
                    };
                }
            }
            "vec_push" | "vec_set" => {
                let vec_name = args.first().and_then(|a| {
                    let e = match a {
                        CallArg::Positional(e) | CallArg::Named(_, e) => e,
                    };
                    // Unwrap an explicit &/&mut, if the caller wrote one
                    // (not required -- these builtins mutate by name
                    // regardless, same as how `print`/`len` don't
                    // require it either -- but writing `vec_push(&mut v,
                    // x)` to match this language's usual mutation-
                    // signaling convention is accepted, not rejected).
                    let unwrapped = match e {
                        Expr::Borrow(inner, _) => inner.as_ref(),
                        other => other,
                    };
                    match unwrapped {
                        Expr::Ident(n) => Some(n.clone()),
                        _ => None,
                    }
                });
                let Some(vec_name) = vec_name else {
                    panic!("Kernel interpreter: `{}`'s first argument must be a plain Vec variable name (optionally `&mut`-prefixed), not an arbitrary expression", name);
                };
                // Resolve THROUGH a persistent-borrow alias first (see
                // ref_aliases's doc comment) -- `vec_push(r, x)` where
                // `r` came from `let r = &mut v` must mutate `v`.
                let vec_name = self.resolve_alias(&vec_name);
                let mut items = match self.get_var(&vec_name) {
                    Value::Vec(items) => items,
                    other => panic!("Kernel interpreter: `{}` expects a Vec, found {:?}", name, other),
                };
                if name == "vec_push" {
                    let e = match args.get(1).unwrap_or_else(|| panic!("Kernel interpreter: vec_push() requires a value argument")) {
                        CallArg::Positional(e) | CallArg::Named(_, e) => e,
                    };
                    let val = self.eval(e);
                    items.push(val);
                } else {
                    let ie = match args.get(1).unwrap_or_else(|| panic!("Kernel interpreter: vec_set() requires an index and a value argument")) {
                        CallArg::Positional(e) | CallArg::Named(_, e) => e,
                    };
                    let ve = match args.get(2).unwrap_or_else(|| panic!("Kernel interpreter: vec_set() requires an index and a value argument")) {
                        CallArg::Positional(e) | CallArg::Named(_, e) => e,
                    };
                    let idx = as_int(&self.eval(ie));
                    let val = self.eval(ve);
                    if idx < 0 || idx as usize >= items.len() {
                        panic!("Kernel interpreter: vec_set() index {} out of bounds (len {})", idx, items.len());
                    }
                    items[idx as usize] = val;
                }
                self.set_var(&vec_name, Value::Vec(items));
                return Value::Unit;
            }
            // File I/O (v1.x): INTERPRETER-ONLY, dev-mode convenience --
            // there is no filesystem at all in the C codegen backend's
            // target (a freestanding kernel booting under QEMU/real
            // hardware), so these two builtins have no meaning there and
            // are rejected with a clear error at codegen time (see
            // codegen_c.rs's call_expr) rather than silently emitting a
            // call to a function that was never defined, which would only
            // surface as a confusing LINKER error. A real, named
            // interpreter/codegen asymmetry -- not every builtin needs to
            // exist on both backends, but it must fail LOUDLY on the one
            // it doesn't, never silently.
            "read_file" => {
                if let Some(CallArg::Positional(e)) = args.first() {
                    let path = match self.eval(e) {
                        Value::Str(s) => s,
                        other => panic!("Kernel interpreter: read_file() expects a str path, found {:?}", other),
                    };
                    let contents = std::fs::read_to_string(&path)
                        .unwrap_or_else(|err| panic!("Kernel interpreter: read_file(\"{}\") failed: {}", path, err));
                    return Value::Str(contents);
                }
            }
            "write_file" => {
                if let (Some(CallArg::Positional(p)), Some(CallArg::Positional(c))) = (args.first(), args.get(1)) {
                    let path = match self.eval(p) {
                        Value::Str(s) => s,
                        other => panic!("Kernel interpreter: write_file() expects a str path as its 1st argument, found {:?}", other),
                    };
                    let content = match self.eval(c) {
                        Value::Str(s) => s,
                        other => panic!("Kernel interpreter: write_file() expects a str content as its 2nd argument, found {:?}", other),
                    };
                    std::fs::write(&path, &content)
                        .unwrap_or_else(|err| panic!("Kernel interpreter: write_file(\"{}\", ...) failed: {}", path, err));
                    return Value::Unit;
                }
            }
            "raw_write" => {
                // unsafe escape hatch: in the interpreter this just logs the
                // intent, real MMIO happens in the compiled/kernel target.
                let vals: Vec<Value> = args
                    .iter()
                    .map(|a| match a {
                        CallArg::Positional(e) => self.eval(e),
                        CallArg::Named(_, e) => self.eval(e),
                    })
                    .collect();
                self.output.push(format!("[unsafe raw_write] {:?}", vals));
                return Value::Unit;
            }
            _ => {}
        }

        // struct literal construction, e.g. Point(x: 0, y: 0)
        if self.structs.contains_key(name) {
            let mut fields = HashMap::new();
            for a in args {
                if let CallArg::Named(fname, e) = a {
                    let v = self.eval(e);
                    fields.insert(fname.clone(), v);
                }
            }
            return Value::Struct(name.to_string(), fields);
        }

        // user-defined function call, positional args bound to params in order
        let func = self
            .functions
            .get(name)
            .cloned()
            .unwrap_or_else(|| panic!("Kernel interpreter: call to undefined function `{}`", name));

        let mut evaled = Vec::new();
        for a in args {
            match a {
                CallArg::Positional(e) => evaled.push(self.eval(e)),
                CallArg::Named(_, e) => evaled.push(self.eval(e)),
            }
        }

        let mut scope = HashMap::new();
        for (param, val) in func.params.iter().zip(evaled.into_iter()) {
            scope.insert(param.name.clone(), val);
        }
        self.scopes.push(scope);
        self.ref_aliases.push(HashMap::new());
        let result = match self.exec_block(&func.body) {
            Flow::Return(v) => v,
            Flow::Normal => Value::Unit,
        };
        self.scopes.pop();
        self.ref_aliases.pop();
        result
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Int(n) => *n != 0,
        _ => true,
    }
}

fn as_int(v: &Value) -> i64 {
    match v {
        Value::Int(n) => *n,
        other => panic!("Kernel interpreter: expected int, found {:?}", other),
    }
}

fn as_float(v: &Value) -> f64 {
    match v {
        Value::Float(n) => *n,
        Value::Int(n) => *n as f64,
        other => panic!("Kernel interpreter: expected float, found {:?}", other),
    }
}

fn is_float_val(v: &Value) -> bool {
    matches!(v, Value::Float(_))
}

fn eval_binop(l: &Value, op: BinOp, r: &Value) -> Value {
    use BinOp::*;
    let is_float_op = is_float_val(l) || is_float_val(r);
    match op {
        Add => match (l, r) {
            (Value::Str(a), Value::Str(b)) => Value::Str(format!("{}{}", a, b)),
            _ if is_float_op => Value::Float(as_float(l) + as_float(r)),
            (Value::Int(a), Value::Int(b)) => Value::Int(a + b),
            _ => panic!("Kernel interpreter: `+` type mismatch"),
        },
        Sub => if is_float_op { Value::Float(as_float(l) - as_float(r)) } else { Value::Int(as_int(l) - as_int(r)) },
        Mul => if is_float_op { Value::Float(as_float(l) * as_float(r)) } else { Value::Int(as_int(l) * as_int(r)) },
        Div => if is_float_op { Value::Float(as_float(l) / as_float(r)) } else { Value::Int(as_int(l) / as_int(r)) },
        Mod => Value::Int(as_int(l) % as_int(r)),
        Eq => Value::Bool(l == r),
        NotEq => Value::Bool(l != r),
        Lt => if is_float_op { Value::Bool(as_float(l) < as_float(r)) } else { Value::Bool(as_int(l) < as_int(r)) },
        Gt => if is_float_op { Value::Bool(as_float(l) > as_float(r)) } else { Value::Bool(as_int(l) > as_int(r)) },
        LtEq => if is_float_op { Value::Bool(as_float(l) <= as_float(r)) } else { Value::Bool(as_int(l) <= as_int(r)) },
        GtEq => if is_float_op { Value::Bool(as_float(l) >= as_float(r)) } else { Value::Bool(as_int(l) >= as_int(r)) },
        And => Value::Bool(truthy(l) && truthy(r)),
        Or => Value::Bool(truthy(l) || truthy(r)),
    }
}
