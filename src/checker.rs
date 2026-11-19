// Kernel (.kn) ownership/safety checker — v1.
//
// Scope, stated honestly:
//   1. Move checking for struct-typed values. Primitives (int/bool/str) are
//      always Copy in v1 — only struct values are tracked, matching the
//      canonical `take(origin)` / `peek(&origin)` example from the README.
//      A bare `name` passed as a call argument MOVES it; `&name` BORROWS it
//      (no state change). Using a moved variable again is a hard error.
//   2. Unsafe enforcement: calling a known-unsafe operation (the `raw_write`
//      builtin, or any `unsafe fn`) outside an `unsafe:` block or an
//      `unsafe fn` body is a hard error.
//
// NOT implemented in v1 (documented limitation, not a silent gap):
//   - cross-branch move merging (if/elif/else, while, for bodies are checked
//     each in their own scope snapshot; a move inside a branch does not
//     propagate to the parent scope after the block exits). This can miss
//     real use-after-move bugs that only occur on certain branches. Real
//     Rust-style control-flow-merged move analysis is a v1.x refinement.
//   - simultaneous mutable/immutable borrow conflicts (the hard 20% of a
//     real borrow checker). v1 only tracks move-vs-borrow at the call site.

use crate::ast::*;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
struct VarState {
    copy: bool,
    moved: bool,
}

pub struct Checker {
    structs: HashSet<String>,
    functions: HashMap<String, FnDecl>,
    errors: Vec<String>,
}

impl Checker {
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
        Checker { structs, functions, errors: Vec::new() }
    }

    pub fn check(mut self, program: &Program) -> Vec<String> {
        for item in &program.items {
            if let Item::Function(f) = item {
                let mut scope: HashMap<String, VarState> = HashMap::new();
                for p in &f.params {
                    let copy = p.borrowed || !self.structs.contains(&p.ty);
                    scope.insert(p.name.clone(), VarState { copy, moved: false });
                }
                self.check_block(&f.body, &mut scope, f.is_unsafe, &f.name);
            }
        }
        self.errors
    }

    fn is_copy_expr(&self, expr: &Expr, scope: &HashMap<String, VarState>) -> bool {
        match expr {
            Expr::Int(_) | Expr::Float(_) | Expr::Str(_) | Expr::Bool(_) => true,
            Expr::Ident(name) => scope.get(name).map(|s| s.copy).unwrap_or(true),
            Expr::Binary(..) | Expr::Unary(..) | Expr::Field(..) | Expr::Borrow(..) | Expr::Range(..) => true,
            Expr::Call { name, .. } => {
                if self.structs.contains(name) {
                    false
                } else if let Some(f) = self.functions.get(name) {
                    match &f.return_ty {
                        Some(t) if self.structs.contains(t) => false,
                        _ => true,
                    }
                } else {
                    true
                }
            }
        }
    }

    fn is_unsafe_call(&self, name: &str) -> bool {
        name == "raw_write" || self.functions.get(name).map(|f| f.is_unsafe).unwrap_or(false)
    }

    fn check_block(&mut self, stmts: &[Stmt], scope: &mut HashMap<String, VarState>, unsafe_ctx: bool, fn_name: &str) {
        for stmt in stmts {
            self.check_stmt(stmt, scope, unsafe_ctx, fn_name);
        }
    }

    fn check_stmt(&mut self, stmt: &Stmt, scope: &mut HashMap<String, VarState>, unsafe_ctx: bool, fn_name: &str) {
        match stmt {
            Stmt::Let { name, value, .. } => {
                self.check_expr(value, scope, unsafe_ctx, fn_name);
                let copy = self.is_copy_expr(value, scope);
                scope.insert(name.clone(), VarState { copy, moved: false });
            }
            Stmt::Assign { name, value, .. } => {
                self.check_expr(value, scope, unsafe_ctx, fn_name);
                let copy = self.is_copy_expr(value, scope);
                scope.insert(name.clone(), VarState { copy, moved: false });
            }
            Stmt::FieldAssign { object, value, .. } => {
                self.check_expr(object, scope, unsafe_ctx, fn_name);
                self.check_expr(value, scope, unsafe_ctx, fn_name);
            }
            Stmt::If { arms, else_body } => {
                for (cond, body) in arms {
                    self.check_expr(cond, scope, unsafe_ctx, fn_name);
                    let mut branch_scope = scope.clone();
                    self.check_block(body, &mut branch_scope, unsafe_ctx, fn_name);
                }
                if let Some(body) = else_body {
                    let mut branch_scope = scope.clone();
                    self.check_block(body, &mut branch_scope, unsafe_ctx, fn_name);
                }
            }
            Stmt::While { cond, body } => {
                self.check_expr(cond, scope, unsafe_ctx, fn_name);
                let mut branch_scope = scope.clone();
                self.check_block(body, &mut branch_scope, unsafe_ctx, fn_name);
            }
            Stmt::For { var, iter, body } => {
                self.check_expr(iter, scope, unsafe_ctx, fn_name);
                let mut branch_scope = scope.clone();
                branch_scope.insert(var.clone(), VarState { copy: true, moved: false });
                self.check_block(body, &mut branch_scope, unsafe_ctx, fn_name);
            }
            Stmt::Return(expr) => {
                if let Some(e) = expr {
                    self.check_expr(e, scope, unsafe_ctx, fn_name);
                }
            }
            Stmt::Unsafe(body) => {
                self.check_block(body, scope, true, fn_name);
            }
            Stmt::Expr(e) => {
                self.check_expr(e, scope, unsafe_ctx, fn_name);
            }
        }
    }

    fn check_expr(&mut self, expr: &Expr, scope: &mut HashMap<String, VarState>, unsafe_ctx: bool, fn_name: &str) {
        match expr {
            Expr::Int(_) | Expr::Float(_) | Expr::Str(_) | Expr::Bool(_) => {}
            Expr::Ident(name) => {
                if let Some(state) = scope.get(name) {
                    if state.moved {
                        self.errors.push(format!(
                            "[{}] use of moved value `{}` — it was already moved into an earlier call; pass `&{}` next time if you only need to borrow it",
                            fn_name, name, name
                        ));
                    }
                }
            }
            Expr::Range(a, b) => {
                self.check_expr(a, scope, unsafe_ctx, fn_name);
                self.check_expr(b, scope, unsafe_ctx, fn_name);
            }
            Expr::Unary(_, e) => self.check_expr(e, scope, unsafe_ctx, fn_name),
            Expr::Binary(l, _, r) => {
                self.check_expr(l, scope, unsafe_ctx, fn_name);
                self.check_expr(r, scope, unsafe_ctx, fn_name);
            }
            Expr::Field(obj, _) => self.check_expr(obj, scope, unsafe_ctx, fn_name),
            Expr::Borrow(inner) => {
                // explicit borrow: read the inner value, never moves it
                self.check_expr(inner, scope, unsafe_ctx, fn_name);
            }
            Expr::Call { name, args } => {
                if self.is_unsafe_call(name) && !unsafe_ctx {
                    self.errors.push(format!(
                        "[{}] call to unsafe operation `{}` outside an `unsafe` block",
                        fn_name, name
                    ));
                }
                for arg in args {
                    let inner = match arg {
                        CallArg::Positional(e) => e,
                        CallArg::Named(_, e) => e,
                    };
                    match inner {
                        Expr::Borrow(e) => {
                            self.check_expr(e, scope, unsafe_ctx, fn_name);
                        }
                        Expr::Ident(varname) => {
                            // check for use-after-move first
                            self.check_expr(inner, scope, unsafe_ctx, fn_name);
                            let is_struct_move = scope.get(varname).map(|s| !s.copy).unwrap_or(false);
                            if is_struct_move {
                                if let Some(state) = scope.get_mut(varname) {
                                    state.moved = true;
                                }
                            }
                        }
                        other => self.check_expr(other, scope, unsafe_ctx, fn_name),
                    }
                }
            }
        }
    }
}
