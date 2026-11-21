// Kernel (.kn) closures -- v1.x, via LAMBDA LIFTING, not a runtime closure
// value. Runs as its own AST-to-AST pass between monomorphize and the
// checker (same philosophy as monomorphize.rs / match-desugaring: nothing
// downstream -- checker, interpreter, codegen -- ever needs to know
// closures exist). See ast.rs's Expr::Closure doc comment for the full
// honest scope (non-escaping, primitive captures only, no recursion).
//
// How it works: a closure bound via `let name = |params| -> ret: body` is
// converted into a brand-new top-level function (its captured free
// variables become extra LEADING parameters, named identically so the
// body's references to them keep resolving correctly unchanged), the
// `let` itself is deleted, and every call `name(...)` within its lexical
// scope is rewritten to pass the captured values explicitly as extra
// leading arguments. Any OTHER reference to `name` (passed as a value,
// returned, stored, etc.) is a loud lift-time error -- there's no closure
// value to represent it with, by design (see ast.rs).
//
// Named limitation not specifically guarded against: a closure that calls
// an unsafe operation, or is itself written inside an `unsafe:` block,
// loses that unsafe context once lifted to a top-level (non-unsafe)
// function -- this surfaces as the checker's ordinary "unsafe call
// outside an unsafe block" error rather than a dedicated lambda-lift
// error, which is still a loud failure, just a less specific one.

use crate::ast::*;
use std::collections::{HashMap, HashSet};

pub fn lambda_lift(program: Program) -> Program {
    let known_names: HashSet<String> = program
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Function(f) => Some(f.name.clone()),
            Item::Struct(s) => Some(s.name.clone()),
            Item::Static { name, .. } | Item::Const { name, .. } => Some(name.clone()),
            Item::Enum(e) => Some(e.name.clone()),
            Item::Import(_) => unreachable!("Kernel lambda-lift: Item::Import should always be resolved by modules.rs before this runs"),
        })
        .collect();

    let mut lifted: Vec<Item> = Vec::new();
    let mut counter: usize = 0;
    let mut new_items: Vec<Item> = Vec::new();

    for item in program.items {
        match item {
            Item::Function(mut f) => {
                let mut var_types: HashMap<String, String> =
                    f.params.iter().map(|p| (p.name.clone(), p.ty.clone())).collect();
                let mut lifter = Lifter {
                    known_names: &known_names,
                    counter: &mut counter,
                    enclosing_fn: f.name.clone(),
                    lifted: &mut lifted,
                };
                f.body = lifter.lift_block(f.body, &mut var_types, &mut HashMap::new());
                new_items.push(Item::Function(f));
            }
            other => new_items.push(other),
        }
    }

    new_items.extend(lifted);
    Program { items: new_items }
}

// name -> (lifted top-level function name, captured free-variable names in
// the order they became that function's leading parameters).
type ClosureScope = HashMap<String, (String, Vec<String>)>;

struct Lifter<'a> {
    known_names: &'a HashSet<String>,
    counter: &'a mut usize,
    enclosing_fn: String,
    lifted: &'a mut Vec<Item>,
}

impl<'a> Lifter<'a> {
    // Processes one block: strips out every closure-binding `let`
    // (replacing it with nothing, after generating its lifted function),
    // and rewrites every statement that follows -- in this block and any
    // nested block within it -- to call the lifted function instead.
    // `closures` is cloned into each nested block (If/While/For/Unsafe)
    // rather than threaded by reference, so a closure defined inside one
    // branch never leaks into a sibling branch or past its own block,
    // matching ordinary variable scoping.
    fn lift_block(
        &mut self,
        stmts: Vec<Stmt>,
        var_types: &mut HashMap<String, String>,
        closures: &mut ClosureScope,
    ) -> Vec<Stmt> {
        let mut out = Vec::new();
        for stmt in stmts {
            if let Stmt::Let { name, value: Expr::Closure { params, return_ty, body }, .. } = stmt {
                // Lift any closures nested INSIDE this closure's own body
                // first, so the free-variable walk below sees already-
                // erased code (and so a transitively-captured variable from
                // an inner closure's own capture list is correctly counted
                // as free in THIS closure too).
                let mut inner_var_types: HashMap<String, String> =
                    params.iter().map(|p| (p.name.clone(), p.ty.clone())).collect();
                let mut inner_closures = closures.clone();
                let body = self.lift_block(body, &mut inner_var_types, &mut inner_closures);

                let bound: HashSet<String> = params.iter().map(|p| p.name.clone()).collect();
                let mut free: Vec<String> = Vec::new();
                let mut seen: HashSet<String> = HashSet::new();
                self.find_free_vars(&body, &bound, &mut seen, &mut free);

                let mut captured_params = Vec::new();
                for fv in &free {
                    let ty = var_types.get(fv).cloned().unwrap_or_else(|| {
                        panic!(
                            "Kernel lambda-lift: closure `{}` references `{}`, which isn't a known typed \
                             variable in scope at this point (e.g. its type couldn't be inferred)",
                            name, fv
                        )
                    });
                    if !matches!(ty.as_str(), "int" | "float" | "str" | "bool") {
                        panic!(
                            "Kernel lambda-lift: closure `{}` captures `{}` of type `{}` -- v1.x closures may \
                             only capture primitive values (int/float/str/bool); capturing a struct or array \
                             isn't supported yet (see ast.rs's Expr::Closure doc comment)",
                            name, fv, ty
                        );
                    }
                    captured_params.push(Param { name: fv.clone(), ty, borrowed: false, exclusive: false, mutable: false });
                }
                captured_params.extend(params);

                *self.counter += 1;
                let lifted_name = format!("__closure_{}_{}", self.enclosing_fn, self.counter);
                self.lifted.push(Item::Function(FnDecl {
                    name: lifted_name.clone(),
                    is_unsafe: false,
                    type_params: Vec::new(),
                    params: captured_params,
                    return_ty,
                    body,
                }));
                closures.insert(name, (lifted_name, free));
                // the `let` itself is erased -- nothing pushed to `out`.
                continue;
            }

            let mut stmt = stmt;
            if let Stmt::Let { name, value, ty, .. } = &stmt {
                // Keep var_types current for ordinary lets too (same
                // lightweight literal/ident inference as monomorphize.rs),
                // so a LATER closure in this same block can still resolve
                // this variable's type if it captures it.
                let inferred = ty.clone().or_else(|| infer_simple_type(value, var_types));
                if let Some(t) = inferred {
                    var_types.insert(name.clone(), t);
                }
            }
            self.rewrite_and_recurse_stmt(&mut stmt, var_types, closures);
            out.push(stmt);
        }
        out
    }

    fn rewrite_and_recurse_stmt(&mut self, stmt: &mut Stmt, var_types: &mut HashMap<String, String>, closures: &mut ClosureScope) {
        match stmt {
            Stmt::Let { value, .. } => self.rewrite_expr(value, closures),
            Stmt::Assign { value, .. } => self.rewrite_expr(value, closures),
            Stmt::FieldAssign { object, value, .. } => {
                self.rewrite_expr(object, closures);
                self.rewrite_expr(value, closures);
            }
            Stmt::IndexAssign { array, index, value } => {
                self.rewrite_expr(array, closures);
                self.rewrite_expr(index, closures);
                self.rewrite_expr(value, closures);
            }
            Stmt::If { arms, else_body } => {
                for (cond, body) in arms.iter_mut() {
                    self.rewrite_expr(cond, closures);
                    let mut branch_vars = var_types.clone();
                    let mut branch_closures = closures.clone();
                    *body = self.lift_block(std::mem::take(body), &mut branch_vars, &mut branch_closures);
                }
                if let Some(body) = else_body {
                    let mut branch_vars = var_types.clone();
                    let mut branch_closures = closures.clone();
                    *body = self.lift_block(std::mem::take(body), &mut branch_vars, &mut branch_closures);
                }
            }
            Stmt::While { cond, body } => {
                self.rewrite_expr(cond, closures);
                let mut branch_vars = var_types.clone();
                let mut branch_closures = closures.clone();
                *body = self.lift_block(std::mem::take(body), &mut branch_vars, &mut branch_closures);
            }
            Stmt::For { var, iter, body } => {
                self.rewrite_expr(iter, closures);
                let mut branch_vars = var_types.clone();
                branch_vars.insert(var.clone(), "int".to_string());
                let mut branch_closures = closures.clone();
                *body = self.lift_block(std::mem::take(body), &mut branch_vars, &mut branch_closures);
            }
            Stmt::Return(Some(e)) => self.rewrite_expr(e, closures),
            Stmt::Return(None) => {}
            Stmt::Unsafe(body) => {
                let mut branch_vars = var_types.clone();
                let mut branch_closures = closures.clone();
                *body = self.lift_block(std::mem::take(body), &mut branch_vars, &mut branch_closures);
            }
            Stmt::Expr(e) => self.rewrite_expr(e, closures),
        }
    }

    // Rewrites every call to a now-lifted closure (prepends its captured
    // values as extra leading positional args, renames the callee), and
    // rejects any OTHER reference to a closure's name as a loud escape
    // error -- see this module's and ast.rs's doc comments for why there's
    // no value representation to fall back to instead.
    fn rewrite_expr(&mut self, expr: &mut Expr, closures: &ClosureScope) {
        match expr {
            Expr::Int(_) | Expr::Float(_) | Expr::Str(_) | Expr::Bool(_) => {}
            Expr::Ident(name) => {
                if closures.contains_key(name) {
                    panic!(
                        "Kernel lambda-lift: `{}` is a closure and can only be CALLED (`{}(...)`), not used as a \
                         value -- v1.x closures can't be passed to another function, returned, or stored (see \
                         ast.rs's Expr::Closure doc comment)",
                        name, name
                    );
                }
            }
            Expr::Range(a, b) => {
                self.rewrite_expr(a, closures);
                self.rewrite_expr(b, closures);
            }
            Expr::Unary(_, e) => self.rewrite_expr(e, closures),
            Expr::Binary(l, _, r) => {
                self.rewrite_expr(l, closures);
                self.rewrite_expr(r, closures);
            }
            Expr::Field(obj, _) => self.rewrite_expr(obj, closures),
            Expr::Borrow(inner, _) => self.rewrite_expr(inner, closures),
            Expr::EnumTag(e) => self.rewrite_expr(e, closures),
            Expr::EnumVariantTag(_) => {}
            Expr::EnumField(e, _, _) => self.rewrite_expr(e, closures),
            Expr::Index(arr, idx) => {
                self.rewrite_expr(arr, closures);
                self.rewrite_expr(idx, closures);
            }
            Expr::ArrayLit(elems) => {
                for e in elems.iter_mut() {
                    self.rewrite_expr(e, closures);
                }
            }
            Expr::Closure { .. } => {
                // Only reachable if a closure literal somehow appears
                // outside the sole-supported `let` position -- the parser
                // already restricts this at parse time, so this is purely
                // defensive.
                panic!("Kernel lambda-lift: closure literal found in an unsupported position (not a `let` value)");
            }
            Expr::Call { name, args } => {
                for arg in args.iter_mut() {
                    let inner = match arg {
                        CallArg::Positional(e) => e,
                        CallArg::Named(_, e) => e,
                    };
                    self.rewrite_expr(inner, closures);
                }
                if let Some((lifted_name, captured)) = closures.get(name.as_str()) {
                    let mut new_args: Vec<CallArg> =
                        captured.iter().map(|c| CallArg::Positional(Expr::Ident(c.clone()))).collect();
                    new_args.extend(args.drain(..));
                    *name = lifted_name.clone();
                    *args = new_args;
                }
            }
        }
    }

    fn find_free_vars(&self, stmts: &[Stmt], bound: &HashSet<String>, seen: &mut HashSet<String>, free: &mut Vec<String>) {
        let mut bound = bound.clone();
        for stmt in stmts {
            match stmt {
                Stmt::Let { name, value, .. } => {
                    self.find_free_in_expr(value, &bound, seen, free);
                    bound.insert(name.clone());
                }
                Stmt::Assign { value, .. } => self.find_free_in_expr(value, &bound, seen, free),
                Stmt::FieldAssign { object, value, .. } => {
                    self.find_free_in_expr(object, &bound, seen, free);
                    self.find_free_in_expr(value, &bound, seen, free);
                }
                Stmt::IndexAssign { array, index, value } => {
                    self.find_free_in_expr(array, &bound, seen, free);
                    self.find_free_in_expr(index, &bound, seen, free);
                    self.find_free_in_expr(value, &bound, seen, free);
                }
                Stmt::If { arms, else_body } => {
                    for (cond, body) in arms {
                        self.find_free_in_expr(cond, &bound, seen, free);
                        self.find_free_vars(body, &bound, seen, free);
                    }
                    if let Some(body) = else_body {
                        self.find_free_vars(body, &bound, seen, free);
                    }
                }
                Stmt::While { cond, body } => {
                    self.find_free_in_expr(cond, &bound, seen, free);
                    self.find_free_vars(body, &bound, seen, free);
                }
                Stmt::For { var, iter, body } => {
                    self.find_free_in_expr(iter, &bound, seen, free);
                    let mut inner_bound = bound.clone();
                    inner_bound.insert(var.clone());
                    self.find_free_vars(body, &inner_bound, seen, free);
                }
                Stmt::Return(Some(e)) => self.find_free_in_expr(e, &bound, seen, free),
                Stmt::Return(None) => {}
                Stmt::Unsafe(body) => self.find_free_vars(body, &bound, seen, free),
                Stmt::Expr(e) => self.find_free_in_expr(e, &bound, seen, free),
            }
        }
    }

    fn find_free_in_expr(&self, expr: &Expr, bound: &HashSet<String>, seen: &mut HashSet<String>, free: &mut Vec<String>) {
        match expr {
            Expr::Int(_) | Expr::Float(_) | Expr::Str(_) | Expr::Bool(_) => {}
            Expr::Ident(name) => {
                if !bound.contains(name) && !self.known_names.contains(name) && seen.insert(name.clone()) {
                    free.push(name.clone());
                }
            }
            Expr::Range(a, b) => {
                self.find_free_in_expr(a, bound, seen, free);
                self.find_free_in_expr(b, bound, seen, free);
            }
            Expr::Unary(_, e) => self.find_free_in_expr(e, bound, seen, free),
            Expr::Binary(l, _, r) => {
                self.find_free_in_expr(l, bound, seen, free);
                self.find_free_in_expr(r, bound, seen, free);
            }
            Expr::Field(obj, _) => self.find_free_in_expr(obj, bound, seen, free),
            Expr::Borrow(inner, _) => self.find_free_in_expr(inner, bound, seen, free),
            Expr::EnumTag(e) => self.find_free_in_expr(e, bound, seen, free),
            Expr::EnumVariantTag(_) => {}
            Expr::EnumField(e, _, _) => self.find_free_in_expr(e, bound, seen, free),
            Expr::Index(arr, idx) => {
                self.find_free_in_expr(arr, bound, seen, free);
                self.find_free_in_expr(idx, bound, seen, free);
            }
            Expr::ArrayLit(elems) => {
                for e in elems {
                    self.find_free_in_expr(e, bound, seen, free);
                }
            }
            Expr::Closure { .. } => {} // nested closures are already lifted away before this runs
            Expr::Call { args, .. } => {
                // The callee name itself is never a free-variable
                // reference to check here -- it's either a known global
                // function/struct name, or (post-recursion) an
                // already-lifted closure call; only its ARGUMENTS can
                // reference an outer free variable.
                for arg in args {
                    let inner = match arg {
                        CallArg::Positional(e) => e,
                        CallArg::Named(_, e) => e,
                    };
                    self.find_free_in_expr(inner, bound, seen, free);
                }
            }
        }
    }
}

// Lightweight best-effort type inference, same spirit (and same honest
// "not full Hindley-Milner" caveat) as monomorphize.rs's infer_expr_type --
// only needs to recognize PRIMITIVE types accurately enough to decide
// whether a captured free variable is allowed; anything it can't resolve
// (e.g. a struct-construction or function-call result) is left untyped,
// which surfaces as a clear lambda-lift error if later captured, not a
// silent miscompile.
fn infer_simple_type(expr: &Expr, var_types: &HashMap<String, String>) -> Option<String> {
    match expr {
        Expr::Int(_) => Some("int".to_string()),
        Expr::Float(_) => Some("float".to_string()),
        Expr::Str(_) => Some("str".to_string()),
        Expr::Bool(_) => Some("bool".to_string()),
        Expr::Ident(name) => var_types.get(name).cloned(),
        Expr::Borrow(inner, _) => infer_simple_type(inner, var_types),
        Expr::Unary(_, inner) => infer_simple_type(inner, var_types),
        Expr::Binary(l, _, _) => infer_simple_type(l, var_types),
        _ => None,
    }
}
