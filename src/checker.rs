// Kernel (.kn) ownership/safety checker — v1.x.
//
// Scope, stated honestly:
//   1. Move checking for struct-typed values. Primitives (int/bool/str) are
//      always Copy in v1 — only struct values are tracked, matching the
//      canonical `take(origin)` / `peek(&origin)` example from the README.
//      A bare `name` passed as a call argument MOVES it; `&name` BORROWS it
//      (no state change). Using a moved variable again is a hard error.
//      Control-flow is fully merged: a move inside just one `if`/`elif`/
//      `else` arm, inside a `while`/`for` body, or reused on the NEXT pass
//      through that same loop body, is all correctly caught (each is
//      checked against its own scope clone, then merged back; loops are
//      additionally checked twice — once normally, once more seeded with
//      the first pass's end state — to catch re-move-on-next-iteration).
//   2. Unsafe enforcement: calling a known-unsafe operation (the `raw_write`
//      builtin, or any `unsafe fn`) outside an `unsafe:` block or an
//      `unsafe fn` body is a hard error.
//   3. Borrow-conflict detection (v1.x addition, struct-typed values only,
//      matching move-checking's existing scope boundary). Kernel now
//      distinguishes `&T` (shared borrow — any number may coexist) from
//      `&mut T` (exclusive borrow — must be the only live borrow of that
//      value). Two kinds of borrow are tracked differently:
//        - Ephemeral (call-argument) borrows, e.g. `foo(&x, &mut x)`: live
//          only for the duration of evaluating that one call's arguments;
//          conflicts are checked only AMONG that call's own arguments plus
//          whatever's already persistently outstanding (see below), and
//          nothing is left registered afterward.
//        - Persistent (`let`-bound) borrows, e.g. `let r = &mut x`: tracked
//          on `x`'s own VarState via a push onto `borrow_stack`, and
//          deliberately released using LEXICAL SCOPING, not full Non-
//          Lexical-Lifetime analysis — a borrow is considered "alive" from
//          its `let` to the end of its own enclosing block (`if`/`elif`/
//          `else` arm, `while`/`for` body, or the function body itself),
//          not just until its last actual use the way Rust's modern
//          checker computes it. This is a real, deliberate simplification:
//          it will reject some programs a full NLL-style checker would
//          accept, but it will never silently ACCEPT a program with an
//          actual live aliasing conflict — the correct direction for a
//          safety checker to err in, matching the same philosophy already
//          used for move-checking's conservative branch-merge. Full NLL
//          (computing a borrow's actual last-use point via a real
//          control-flow graph, not just "end of its block") remains
//          explicitly deferred future work — see Rust's own RFC 2094 for
//          why that's a substantially harder problem on its own.
//      Moving a struct while it has any live borrow (shared or exclusive)
//      against it is also now a hard error — you can't give away a value
//      something else still has a reference into.

use crate::ast::*;
use std::collections::{HashMap, HashSet};

// `[ElemType;N]` array types are a plain string ("[int;5]") in the AST, same
// representation structs/primitives use -- this is the one place that needs
// to recognize that shape to classify an array as a non-Copy (move-tracked)
// type, same treatment as a struct.
fn is_array_type(ty: &str) -> bool {
    ty.starts_with('[')
}

#[derive(Debug, Clone)]
struct VarState {
    copy: bool,
    moved: bool,
    shared_borrows: u32,
    exclusive_borrow: bool,
}

pub struct Checker {
    structs: HashSet<String>,
    functions: HashMap<String, FnDecl>,
    errors: Vec<String>,
    // Stack of (borrowed variable name, is_exclusive) for every currently-
    // live PERSISTENT (`let`-bound) borrow, in the order they were taken.
    // Released back-to-front at the end of the block each entry was
    // created in — see release_borrows_to().
    borrow_stack: Vec<(String, bool)>,
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
        Checker { structs, functions, errors: Vec::new(), borrow_stack: Vec::new() }
    }

    pub fn check(mut self, program: &Program) -> Vec<String> {
        for item in &program.items {
            if let Item::Function(f) = item {
                let mut scope: HashMap<String, VarState> = HashMap::new();
                for p in &f.params {
                    let copy = p.borrowed || (!self.structs.contains(&p.ty) && !is_array_type(&p.ty));
                    scope.insert(p.name.clone(), VarState { copy, moved: false, shared_borrows: 0, exclusive_borrow: false });
                }
                let mark = self.borrow_stack.len();
                self.check_block(&f.body, &mut scope, f.is_unsafe, &f.name);
                self.release_borrows_to(mark, &mut scope);
            }
        }
        self.errors
    }

    fn is_copy_expr(&self, expr: &Expr, scope: &HashMap<String, VarState>) -> bool {
        match expr {
            Expr::Int(_) | Expr::Float(_) | Expr::Str(_) | Expr::Bool(_) => true,
            Expr::Ident(name) => scope.get(name).map(|s| s.copy).unwrap_or(true),
            Expr::Binary(..) | Expr::Unary(..) | Expr::Field(..) | Expr::Borrow(..) | Expr::Range(..) | Expr::Index(..) => true,
            // A fresh array literal is a newly-constructed owned compound
            // value, same treatment as struct construction below -- moving
            // it moves the whole array, matching real ownership semantics.
            Expr::ArrayLit(_) => false,
            Expr::Closure { .. } => unreachable!("Kernel checker: Expr::Closure should always be erased by lambda_lift.rs before the checker runs"),
            Expr::Call { name, .. } => {
                if self.structs.contains(name) {
                    false
                } else if let Some(f) = self.functions.get(name) {
                    match &f.return_ty {
                        Some(t) if self.structs.contains(t) || is_array_type(t) => false,
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

    // Registers a PERSISTENT borrow (from a `let` binding) against `target`,
    // pushing onto borrow_stack so it can be released at the end of its
    // enclosing block. Reports and refuses to register on conflict.
    fn try_persistent_borrow(&mut self, target: &str, exclusive: bool, scope: &mut HashMap<String, VarState>, fn_name: &str) {
        let Some(state) = scope.get_mut(target) else { return };
        if state.moved {
            return; // use-after-move is already reported elsewhere; don't pile on
        }
        let conflict_kind = if exclusive {
            if state.exclusive_borrow {
                Some("mutably (&mut)")
            } else if state.shared_borrows > 0 {
                Some("immutably (&)")
            } else {
                None
            }
        } else if state.exclusive_borrow {
            Some("mutably (&mut)")
        } else {
            None
        };
        if let Some(existing_kind) = conflict_kind {
            self.errors.push(format!(
                "[{}] cannot borrow `{}` {} because it is already borrowed {} and that borrow is still alive in this scope",
                fn_name, target, if exclusive { "mutably (&mut)" } else { "immutably (&)" }, existing_kind
            ));
            return;
        }
        if exclusive {
            state.exclusive_borrow = true;
        } else {
            state.shared_borrows += 1;
        }
        self.borrow_stack.push((target.to_string(), exclusive));
    }

    // Releases every persistent borrow taken since `mark` (the borrow_stack
    // length recorded on entry to some block) — called when that block
    // exits, implementing the lexical-scope release policy described above.
    fn release_borrows_to(&mut self, mark: usize, scope: &mut HashMap<String, VarState>) {
        while self.borrow_stack.len() > mark {
            let (name, exclusive) = self.borrow_stack.pop().unwrap();
            if let Some(state) = scope.get_mut(&name) {
                if exclusive {
                    state.exclusive_borrow = false;
                } else {
                    state.shared_borrows = state.shared_borrows.saturating_sub(1);
                }
            }
        }
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
                if let Expr::Borrow(inner, exclusive) = value {
                    if let Expr::Ident(target) = inner.as_ref() {
                        self.try_persistent_borrow(target, *exclusive, scope, fn_name);
                    }
                }
                let copy = self.is_copy_expr(value, scope);
                scope.insert(name.clone(), VarState { copy, moved: false, shared_borrows: 0, exclusive_borrow: false });
            }
            Stmt::Assign { name, value, .. } => {
                self.check_expr(value, scope, unsafe_ctx, fn_name);
                let copy = self.is_copy_expr(value, scope);
                scope.insert(name.clone(), VarState { copy, moved: false, shared_borrows: 0, exclusive_borrow: false });
            }
            Stmt::FieldAssign { object, value, .. } => {
                self.check_expr(object, scope, unsafe_ctx, fn_name);
                self.check_expr(value, scope, unsafe_ctx, fn_name);
            }
            Stmt::IndexAssign { array, index, value } => {
                self.check_expr(array, scope, unsafe_ctx, fn_name);
                self.check_expr(index, scope, unsafe_ctx, fn_name);
                self.check_expr(value, scope, unsafe_ctx, fn_name);
            }
            Stmt::If { arms, else_body } => {
                let mut branch_outputs: Vec<HashMap<String, VarState>> = Vec::new();
                for (cond, body) in arms {
                    self.check_expr(cond, scope, unsafe_ctx, fn_name);
                    let mut branch_scope = scope.clone();
                    let mark = self.borrow_stack.len();
                    self.check_block(body, &mut branch_scope, unsafe_ctx, fn_name);
                    self.release_borrows_to(mark, &mut branch_scope);
                    branch_outputs.push(branch_scope);
                }
                if let Some(body) = else_body {
                    let mut branch_scope = scope.clone();
                    let mark = self.borrow_stack.len();
                    self.check_block(body, &mut branch_scope, unsafe_ctx, fn_name);
                    self.release_borrows_to(mark, &mut branch_scope);
                    branch_outputs.push(branch_scope);
                }
                // Merge branch outcomes back into the parent scope: a
                // variable that existed before the `if` is treated as
                // moved afterward if it was moved on ANY reachable branch
                // — we don't statically know which branch ran, so "might
                // have been moved" is conservatively treated as "is
                // moved." This holds even without an `else`, since the
                // implicit skip-path never moves anything but an explicit
                // arm might have. This is the control-flow merge
                // previously missing (see ARD-003) — without it, a move
                // inside just one `if` arm was invisible to every
                // statement after the `if`. Borrows don't need this same
                // merge: each branch's borrows are already fully released
                // (above) before this point, since only one arm actually
                // runs at a time and a borrow can't outlive its own arm.
                for (name, state) in scope.iter_mut() {
                    if branch_outputs.iter().any(|b| b.get(name).map(|s| s.moved).unwrap_or(false)) {
                        state.moved = true;
                    }
                }
            }
            Stmt::While { cond, body } => {
                self.check_expr(cond, scope, unsafe_ctx, fn_name);
                // Pass 1: check the body once, starting from the scope on
                // entry — this is "iteration 1," and is also how a single
                // pass through the loop gets checked normally.
                let mut branch_scope = scope.clone();
                let mark = self.borrow_stack.len();
                self.check_block(body, &mut branch_scope, unsafe_ctx, fn_name);
                self.release_borrows_to(mark, &mut branch_scope);

                // Pass 2: re-check the SAME body starting from iteration
                // 1's end state, simulating "iteration 2" — a loop can run
                // more than once, so anything moved during one pass must be
                // treated as already-moved on the next pass through the
                // same body. Any error that appears ONLY in this second
                // pass (not already reported in pass 1) is exactly a
                // re-move-on-next-iteration bug; errors that appear in both
                // passes are the same root cause already reported once, not
                // duplicated.
                let pass1_len = self.errors.len();
                let first_pass_errors: HashSet<String> = self.errors[..pass1_len].iter().cloned().collect();
                let mut branch_scope2 = branch_scope.clone();
                let mark2 = self.borrow_stack.len();
                self.check_block(body, &mut branch_scope2, unsafe_ctx, fn_name);
                self.release_borrows_to(mark2, &mut branch_scope2);
                let new_errors = self.errors.split_off(pass1_len);
                for e in new_errors {
                    if !first_pass_errors.contains(&e) {
                        self.errors.push(format!(
                            "{} (only on the loop's next iteration — this value was moved on an earlier pass through the same loop body)",
                            e
                        ));
                    }
                }

                for (name, state) in scope.iter_mut() {
                    if branch_scope.get(name).map(|s| s.moved).unwrap_or(false) {
                        state.moved = true;
                    }
                }
            }
            Stmt::For { var, iter, body } => {
                self.check_expr(iter, scope, unsafe_ctx, fn_name);
                let mut branch_scope = scope.clone();
                branch_scope.insert(var.clone(), VarState { copy: true, moved: false, shared_borrows: 0, exclusive_borrow: false });
                let mark = self.borrow_stack.len();
                self.check_block(body, &mut branch_scope, unsafe_ctx, fn_name);
                self.release_borrows_to(mark, &mut branch_scope);

                // Same two-pass re-iteration check as While (see above).
                let pass1_len = self.errors.len();
                let first_pass_errors: HashSet<String> = self.errors[..pass1_len].iter().cloned().collect();
                let mut branch_scope2 = branch_scope.clone();
                let mark2 = self.borrow_stack.len();
                self.check_block(body, &mut branch_scope2, unsafe_ctx, fn_name);
                self.release_borrows_to(mark2, &mut branch_scope2);
                let new_errors = self.errors.split_off(pass1_len);
                for e in new_errors {
                    if !first_pass_errors.contains(&e) {
                        self.errors.push(format!(
                            "{} (only on the loop's next iteration — this value was moved on an earlier pass through the same loop body)",
                            e
                        ));
                    }
                }

                for (name, state) in scope.iter_mut() {
                    if branch_scope.get(name).map(|s| s.moved).unwrap_or(false) {
                        state.moved = true;
                    }
                }
            }
            Stmt::Return(expr) => {
                if let Some(e) = expr {
                    self.check_expr(e, scope, unsafe_ctx, fn_name);
                }
            }
            Stmt::Unsafe(body) => {
                let mark = self.borrow_stack.len();
                self.check_block(body, scope, true, fn_name);
                self.release_borrows_to(mark, scope);
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
            Expr::Index(arr, idx) => {
                self.check_expr(arr, scope, unsafe_ctx, fn_name);
                self.check_expr(idx, scope, unsafe_ctx, fn_name);
            }
            Expr::ArrayLit(elems) => {
                for e in elems {
                    self.check_expr(e, scope, unsafe_ctx, fn_name);
                }
            }
            Expr::Closure { .. } => unreachable!("Kernel checker: Expr::Closure should always be erased by lambda_lift.rs before the checker runs"),
            Expr::Borrow(inner, _exclusive) => {
                // A bare borrow expression outside a call-argument or `let`
                // position (both handled specially below/in check_stmt) --
                // still just reads the inner value; nothing persists.
                self.check_expr(inner, scope, unsafe_ctx, fn_name);
            }
            Expr::Call { name, args } => {
                if self.is_unsafe_call(name) && !unsafe_ctx {
                    self.errors.push(format!(
                        "[{}] call to unsafe operation `{}` outside an `unsafe` block",
                        fn_name, name
                    ));
                }
                // `len(x)` only ever reads `x` to report its size -- it
                // never consumes ownership the way passing a non-Copy
                // value to an ordinary function/struct-construction call
                // does. Handled here, separately from the general move-
                // tracking loop below, so `len(my_array)` doesn't force
                // callers to write `len(&my_array)` just to avoid an
                // unintended "move."
                if name == "len" {
                    for arg in args {
                        let inner = match arg {
                            CallArg::Positional(e) => e,
                            CallArg::Named(_, e) => e,
                        };
                        self.check_expr(inner, scope, unsafe_ctx, fn_name);
                    }
                    return;
                }
                // Ephemeral borrows taken as THIS call's own arguments --
                // live only for evaluating this one call, so conflicts are
                // checked against each other (e.g. foo(&x, &mut x)) and
                // against whatever's already persistently outstanding, but
                // nothing here is pushed onto borrow_stack: it's released
                // implicitly the moment this Call finishes, by simply never
                // having been registered as persistent in the first place.
                let mut call_shared: HashMap<String, u32> = HashMap::new();
                let mut call_exclusive: HashSet<String> = HashSet::new();
                for arg in args {
                    let inner = match arg {
                        CallArg::Positional(e) => e,
                        CallArg::Named(_, e) => e,
                    };
                    match inner {
                        Expr::Borrow(e, exclusive) => {
                            self.check_expr(e, scope, unsafe_ctx, fn_name);
                            if let Expr::Ident(target) = e.as_ref() {
                                let persistent_shared = scope.get(target).map(|s| s.shared_borrows).unwrap_or(0);
                                let persistent_exclusive = scope.get(target).map(|s| s.exclusive_borrow).unwrap_or(false);
                                let this_call_shared = *call_shared.get(target).unwrap_or(&0);
                                let this_call_exclusive = call_exclusive.contains(target);
                                let conflict = if *exclusive {
                                    persistent_exclusive || persistent_shared > 0 || this_call_exclusive || this_call_shared > 0
                                } else {
                                    persistent_exclusive || this_call_exclusive
                                };
                                if conflict {
                                    self.errors.push(format!(
                                        "[{}] cannot borrow `{}` {} because it is already borrowed elsewhere at the same time",
                                        fn_name, target, if *exclusive { "mutably (&mut)" } else { "immutably (&)" }
                                    ));
                                } else if *exclusive {
                                    call_exclusive.insert(target.clone());
                                } else {
                                    *call_shared.entry(target.clone()).or_insert(0) += 1;
                                }
                            }
                        }
                        Expr::Ident(varname) => {
                            // check for use-after-move first
                            self.check_expr(inner, scope, unsafe_ctx, fn_name);
                            let is_struct_move = scope.get(varname).map(|s| !s.copy).unwrap_or(false);
                            if is_struct_move {
                                let currently_borrowed = scope
                                    .get(varname)
                                    .map(|s| s.shared_borrows > 0 || s.exclusive_borrow)
                                    .unwrap_or(false);
                                if currently_borrowed {
                                    self.errors.push(format!(
                                        "[{}] cannot move `{}` because it is currently borrowed elsewhere",
                                        fn_name, varname
                                    ));
                                }
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
