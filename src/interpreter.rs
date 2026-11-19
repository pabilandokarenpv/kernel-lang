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
        let result = match self.exec_block(&main.body) {
            Flow::Return(v) => v,
            Flow::Normal => Value::Unit,
        };
        self.scopes.pop();
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
        // new binding in the innermost scope
        self.scopes.last_mut().unwrap().insert(name.to_string(), value);
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
                    let v = self.eval(value);
                    let mut s = self.get_var(obj_name);
                    if let Value::Struct(_, ref mut fields) = s {
                        fields.insert(field.clone(), v);
                    }
                    self.set_var(obj_name, s);
                }
                Flow::Normal
            }
            Stmt::If { arms, else_body } => {
                for (cond, body) in arms {
                    if truthy(&self.eval(cond)) {
                        self.trace.push(format!("branch taken: if {:?}", cond));
                        self.scopes.push(HashMap::new());
                        let f = self.exec_block(body);
                        self.scopes.pop();
                        return f;
                    }
                }
                if let Some(body) = else_body {
                    self.trace.push("branch taken: else".to_string());
                    self.scopes.push(HashMap::new());
                    let f = self.exec_block(body);
                    self.scopes.pop();
                    return f;
                }
                Flow::Normal
            }
            Stmt::While { cond, body } => {
                while truthy(&self.eval(cond)) {
                    self.scopes.push(HashMap::new());
                    let f = self.exec_block(body);
                    self.scopes.pop();
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
                        self.scopes.last_mut().unwrap().insert(var.clone(), Value::Int(i));
                        let f = self.exec_block(body);
                        self.scopes.pop();
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
            Expr::Borrow(e) => self.eval(e), // interpreter doesn't model aliasing, just the value
            Expr::Call { name, args } => self.eval_call(name, args),
        }
    }

    fn eval_call(&mut self, name: &str, args: &[CallArg]) -> Value {
        // builtins
        match name {
            "print" => {
                let parts: Vec<String> = args
                    .iter()
                    .map(|a| match a {
                        CallArg::Positional(e) => self.eval(e).to_string(),
                        CallArg::Named(_, e) => self.eval(e).to_string(),
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
        let result = match self.exec_block(&func.body) {
            Flow::Return(v) => v,
            Flow::Normal => Value::Unit,
        };
        self.scopes.pop();
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
