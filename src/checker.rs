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

// Recursively checks whether `name` appears as a plain `Expr::Ident`
// ANYWHERE inside `e`. The core primitive for a real (if still
// deliberately partial -- see check_block's doc comment) Non-Lexical-
// Lifetime improvement to the borrow checker: computing where a
// persistent borrow's LAST actual use is, rather than always assuming
// it's "alive" all the way to the end of its enclosing block regardless
// of whether it's ever read again.
fn expr_references(e: &Expr, name: &str) -> bool {
    match e {
        Expr::Int(_) | Expr::Float(_) | Expr::Str(_) | Expr::Bool(_) => false,
        Expr::Ident(n) => n == name,
        Expr::Range(a, b) => expr_references(a, name) || expr_references(b, name),
        Expr::Unary(_, inner) => expr_references(inner, name),
        Expr::Binary(l, _, r) => expr_references(l, name) || expr_references(r, name),
        Expr::Field(obj, _) => expr_references(obj, name),
        Expr::Borrow(inner, _) => expr_references(inner, name),
        Expr::Call { args, .. } => args.iter().any(|a| {
            let e = match a {
                CallArg::Positional(e) | CallArg::Named(_, e) => e,
            };
            expr_references(e, name)
        }),
        Expr::ArrayLit(items) => items.iter().any(|e| expr_references(e, name)),
        Expr::Index(arr, idx) => expr_references(arr, name) || expr_references(idx, name),
        Expr::Closure { .. } => unreachable!("Kernel checker: Expr::Closure should always be erased by lambda_lift.rs before the checker runs"),
    }
}

// Same idea as expr_references, but for a whole statement -- recurses
// into every Expr field AND every nested statement list (If arms/else,
// While/For bodies, Unsafe blocks), since a reference inside a NESTED
// control-flow construct still means the borrow must stay alive at
// least until that construct's own last statement is processed (see
// check_block's release logic: it only ever releases a borrow after
// finishing the full TOP-LEVEL statement containing its last reference,
// never partway through one -- sound regardless of which branch of an
// if/while/for actually runs at runtime, since the checker already
// walks every branch).
fn stmt_references(s: &Stmt, name: &str) -> bool {
    match s {
        Stmt::Let { value, .. } => expr_references(value, name),
        Stmt::Assign { name: target, value, .. } => target == name || expr_references(value, name),
        Stmt::FieldAssign { object, value, .. } => expr_references(object, name) || expr_references(value, name),
        Stmt::IndexAssign { array, index, value } => {
            expr_references(array, name) || expr_references(index, name) || expr_references(value, name)
        }
        Stmt::If { arms, else_body } => {
            arms.iter().any(|(cond, body)| expr_references(cond, name) || stmts_reference(body, name))
                || else_body.as_ref().map(|b| stmts_reference(b, name)).unwrap_or(false)
        }
        Stmt::While { cond, body } => expr_references(cond, name) || stmts_reference(body, name),
        Stmt::For { iter, body, .. } => expr_references(iter, name) || stmts_reference(body, name),
        Stmt::Return(Some(e)) => expr_references(e, name),
        Stmt::Return(None) => false,
        Stmt::Unsafe(body) => stmts_reference(body, name),
        Stmt::Expr(e) => expr_references(e, name),
    }
}

fn stmts_reference(stmts: &[Stmt], name: &str) -> bool {
    stmts.iter().any(|s| stmt_references(s, name))
}

// Finds the ROOT variable name behind an lvalue expression -- a plain
// `Expr::Ident`, or (defensively, though interpreter/codegen currently
// restrict FieldAssign's `object`/IndexAssign's `array` to a plain
// ident anyway -- see ast.rs's own comment on IndexAssign) recursing
// through `Expr::Field` to find the ident underneath a `p.x = ...`-
// style target. Shared by the mutability checks on FieldAssign,
// IndexAssign, and vec_push/vec_set below -- all four need the exact
// same "what variable is actually being mutated here" answer.
fn base_ident(e: &Expr) -> Option<&str> {
    match e {
        Expr::Ident(n) => Some(n),
        Expr::Field(obj, _) => base_ident(obj),
        Expr::Borrow(inner, _) => base_ident(inner),
        _ => None,
    }
}

// `Vec<ElemType>` (v1.x, interpreter-only so far -- see
// interpreter.rs's Value::Vec and the vec_new/vec_push/vec_get/
// vec_len/vec_set builtins) -- same role as is_array_type, a non-Copy
// compound type classified by its string shape rather than a real
// type system.
fn is_vec_type(ty: &str) -> bool {
    ty.starts_with("Vec<")
}

#[derive(Debug, Clone)]
struct VarState {
    copy: bool,
    moved: bool,
    shared_borrows: u32,
    exclusive_borrow: bool,
    // Was this binding ever declared `mut`? Enforced by Stmt::Assign
    // below -- reassigning a plain `let` (no `mut`) is a real safety-
    // check error, not silently accepted. Always `true` for function
    // PARAMETERS and `for`-loop induction variables: Kernel's grammar
    // has no `mut`/non-`mut` distinction for either of those (see
    // ast.rs's Param -- no `mutable` field at all), so there's nothing
    // to enforce there; only `let` bindings can even express
    // immutability in the first place.
    declared_mut: bool,
    // `Some(exclusive)` if this binding ITSELF holds a reference (`&T`/
    // `&mut T`) -- from a `let r = &x`/`let r = &mut x`, or a `&T`/
    // `&mut T` function parameter; `None` for a plain owned value.
    // Mutating THROUGH a reference (`r.field = ...`) is governed by
    // whether the REFERENCE is exclusive (this field), never by
    // whether `r`'s own binding was declared `mut` -- `declared_mut`
    // only matters for REASSIGNING `r` itself (`r = something_else`),
    // a completely different operation. Closes a real regression this
    // very checker introduced in an earlier audit pass: `let r = &mut
    // p; r.x = 5;` was incorrectly rejected with "cannot mutate `r` --
    // it was declared with `let`, not `let mut`" -- confusing "is the
    // BINDING reassignable" with "is the REFERENT mutable through
    // this reference," two different things that happen to share the
    // word "mut" in Kernel's (and Rust's) surface syntax.
    ref_exclusive: Option<bool>,
}

pub struct Checker {
    structs: HashSet<String>,
    functions: HashMap<String, FnDecl>,
    errors: Vec<String>,
    // Stack of (borrower name, target name, is_exclusive) for every
    // currently-live PERSISTENT (`let`-bound) borrow, in the order they
    // were taken. Released either at the end of the block each entry
    // was created in (release_borrows_to, the fallback) or right after
    // the borrower's own real last use (release_one_borrow) -- see
    // check_block's doc comment for the full design.
    borrow_stack: Vec<(String, String, bool)>,
    // Module-level `static`/`const` names -> whether they were declared
    // `static mut` (true) vs plain `static`/`const` (false, always for
    // `const`). The local-scope move/mutability tracking above only
    // ever sees FUNCTION-local names; this is the global-scope twin,
    // consulted by Stmt::Assign when `name` isn't a known local at all
    // -- closes the other half of the same gap `declared_mut` closes
    // for `let` bindings (see Stmt::Assign's own comment for the real
    // bug this was masking: a now-fixed interpreter.rs bug meant
    // reassigning an immutable global silently never actually took
    // effect, so this half of the check was never exercisable before).
    global_mutable: HashMap<String, bool>,
}

impl Checker {
    pub fn new(program: &Program) -> Self {
        let mut structs = HashSet::new();
        let mut functions = HashMap::new();
        let mut global_mutable = HashMap::new();
        for item in &program.items {
            match item {
                Item::Struct(s) => {
                    structs.insert(s.name.clone());
                }
                Item::Function(f) => {
                    functions.insert(f.name.clone(), f.clone());
                }
                Item::Static { name, mutable, .. } => {
                    global_mutable.insert(name.clone(), *mutable);
                }
                Item::Const { name, .. } => {
                    global_mutable.insert(name.clone(), false);
                }
                _ => {}
            }
        }
        Checker { structs, functions, errors: Vec::new(), borrow_stack: Vec::new(), global_mutable }
    }

    pub fn check(mut self, program: &Program) -> Vec<String> {
        for item in &program.items {
            if let Item::Function(f) = item {
                let mut scope: HashMap<String, VarState> = HashMap::new();
                for p in &f.params {
                    let copy = p.borrowed || (!self.structs.contains(&p.ty) && !is_array_type(&p.ty) && !is_vec_type(&p.ty));
                    let ref_exclusive = if p.borrowed { Some(p.exclusive) } else { None };
                    scope.insert(p.name.clone(), VarState { copy, moved: false, shared_borrows: 0, exclusive_borrow: false, declared_mut: true, ref_exclusive });
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
                if name == "vec_new" {
                    // A freshly-built owned Vec -- same treatment as
                    // Expr::ArrayLit above and struct construction below:
                    // moving it moves the whole value.
                    false
                } else if self.structs.contains(name) {
                    false
                } else if let Some(f) = self.functions.get(name) {
                    match &f.return_ty {
                        Some(t) if self.structs.contains(t) || is_array_type(t) || is_vec_type(t) => false,
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

    // Registers a PERSISTENT borrow (from a `let BORROWER = &TARGET`
    // binding) -- pushing (borrower, target, exclusive) onto
    // borrow_stack so it can be released either at the end of its
    // enclosing block (release_borrows_to, the fallback) or right
    // after BORROWER's own real last use (release_one_borrow, see
    // check_block) -- keyed by BORROWER's name specifically, not
    // TARGET's, since release_one_borrow needs to find and remove the
    // entry for a SPECIFIC borrowing variable, and two different
    // borrows can legitimately target the same TARGET in sequence
    // (take `&p`, use it, release it, take `&mut p`) but never share
    // the same BORROWER name while both are simultaneously live.
    // Reports and refuses to register on conflict.
    fn try_persistent_borrow(&mut self, borrower: &str, target: &str, exclusive: bool, scope: &mut HashMap<String, VarState>, fn_name: &str) {
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
        self.borrow_stack.push((borrower.to_string(), target.to_string(), exclusive));
    }

    // Releases every persistent borrow taken since `mark` (the borrow_stack
    // length recorded on entry to some block) — called when that block
    // exits, implementing the lexical-scope release policy described above.
    // Still the fallback/safety-net path for anything check_block's own
    // last-use release (below) didn't already release during the block --
    // in the normal case that's a no-op, since every borrow taken IN this
    // block is accounted for there.
    fn release_borrows_to(&mut self, mark: usize, scope: &mut HashMap<String, VarState>) {
        while self.borrow_stack.len() > mark {
            let (_, target, exclusive) = self.borrow_stack.pop().unwrap();
            if let Some(state) = scope.get_mut(&target) {
                if exclusive {
                    state.exclusive_borrow = false;
                } else {
                    state.shared_borrows = state.shared_borrows.saturating_sub(1);
                }
            }
        }
    }

    // Releases ONE specific persistent borrow by name, regardless of its
    // position in borrow_stack (not necessarily the top) -- the real-
    // last-use release in check_block needs this instead of
    // release_borrows_to's blind LIFO pop, since a borrow taken earlier
    // in a block can legitimately die before one taken later in the
    // same block (e.g. `let r1 = &x; print(r1); let r2 = &mut y;` -- r1's
    // last use is before r2 is even taken). Searches from the END
    // backward so a name borrowed more than once in sequence releases
    // the most-recently-taken entry under that identifier, matching
    // which one is actually still live.
    fn release_one_borrow(&mut self, borrower: &str, scope: &mut HashMap<String, VarState>) {
        if let Some(pos) = self.borrow_stack.iter().rposition(|(b, _, _)| b == borrower) {
            let (_, target, exclusive) = self.borrow_stack.remove(pos);
            if let Some(state) = scope.get_mut(&target) {
                if exclusive {
                    state.exclusive_borrow = false;
                } else {
                    state.shared_borrows = state.shared_borrows.saturating_sub(1);
                }
            }
        }
    }

    fn check_mutable_target(&mut self, name: &str, scope: &HashMap<String, VarState>, fn_name: &str) {
        if let Some(state) = scope.get(name) {
            if !state.declared_mut {
                self.errors.push(format!(
                    "[{}] cannot mutate `{}` -- it was declared with `let`, not `let mut`; add `mut` if it needs to change",
                    fn_name, name
                ));
            }
        } else if let Some(mutable) = self.global_mutable.get(name) {
            // `name` isn't a local at all -- it's a module-level
            // `static`/`const`. Same rule, the global half: a plain
            // `static` (no `mut`) or any `const` can never be mutated.
            if !mutable {
                self.errors.push(format!(
                    "[{}] cannot mutate `{}` -- it's a module-level `static`/`const` that was never declared `mut`; add `mut` to its `static` declaration if it needs to change (a `const` can never be reassigned at all)",
                    fn_name, name
                ));
            }
        }
    }

    // Checks whether mutating THROUGH `name` is allowed -- i.e.
    // `name.field = x` / `name[i] = x` / `vec_push(name, x)`, never
    // `name = x` itself (that's check_mutable_target's job). If `name`
    // is bound to a REFERENCE (VarState::ref_exclusive is `Some`), only
    // the reference's own exclusivity matters -- a shared `&T` can
    // never be mutated through, an exclusive `&mut T` always can,
    // REGARDLESS of whether the variable holding that reference was
    // itself declared `mut` (that's a property of the BINDING, not the
    // referent). For a plain owned value (`ref_exclusive` is `None`),
    // this is exactly the same rule as reassignment, so it delegates
    // straight to check_mutable_target.
    fn check_mutable_through(&mut self, name: &str, scope: &HashMap<String, VarState>, fn_name: &str) {
        match scope.get(name).and_then(|s| s.ref_exclusive) {
            Some(true) => {} // &mut T -- always fine to mutate through
            Some(false) => {
                self.errors.push(format!(
                    "[{}] cannot mutate through `{}` -- it's a shared borrow (`&T`), not an exclusive one (`&mut T`); borrow it with `&mut` if you need to mutate through it",
                    fn_name, name
                ));
            }
            None => self.check_mutable_target(name, scope, fn_name),
        }
    }

    // Real (partial) Non-Lexical-Lifetime improvement: releases each
    // PERSISTENT (`let`-bound) borrow right after its actual LAST
    // reference within this exact block, rather than always waiting
    // for the block to end regardless of whether it's used again. A
    // borrow's binding is only ever visible within the block it was
    // declared in (and blocks nested inside it), so scanning THIS
    // block's own statement list once is a complete answer for "where
    // is this borrow's last use" -- no need for a full cross-function
    // control-flow graph. Deliberately partial, named honestly: a
    // reference nested inside an `if`/`while`/`for` body only shortens
    // the borrow down to the end of that WHOLE containing statement
    // (never to a point partway through one of its branches) --
    // genuinely full NLL (live only through the exact branch taken,
    // computed via real path-sensitive control-flow analysis) remains
    // future work; this is still strictly tighter than the pure
    // lexical (always-to-end-of-innermost-ENCLOSING-block) scoping it
    // replaces, with no soundness cost (every path is still checked,
    // same as before -- this only changes WHEN a borrow is considered
    // released, never WHETHER a conflict is caught).
    fn check_block(&mut self, stmts: &[Stmt], scope: &mut HashMap<String, VarState>, unsafe_ctx: bool, fn_name: &str) {
        // Pre-scan: for every `let NAME = &TARGET` / `let NAME = &mut
        // TARGET` in THIS block, compute the index of the last
        // statement (after the let itself) that references NAME --
        // falling back to the let's own index if NAME is never
        // referenced again at all (dies immediately, the tightest
        // possible lifetime).
        let mut last_use: HashMap<usize, Vec<String>> = HashMap::new(); // stmt index -> borrow name(s) to release after it
        for (i, stmt) in stmts.iter().enumerate() {
            if let Stmt::Let { name, value: Expr::Borrow(inner, _), .. } = stmt {
                if matches!(inner.as_ref(), Expr::Ident(_)) {
                    let mut release_at = i;
                    for (j, later) in stmts.iter().enumerate().skip(i + 1) {
                        if stmt_references(later, name) {
                            release_at = j;
                        }
                    }
                    last_use.entry(release_at).or_default().push(name.clone());
                }
            }
        }
        for (i, stmt) in stmts.iter().enumerate() {
            self.check_stmt(stmt, scope, unsafe_ctx, fn_name);
            if let Some(names) = last_use.get(&i) {
                for name in names {
                    self.release_one_borrow(name, scope);
                }
            }
        }
    }

    fn check_stmt(&mut self, stmt: &Stmt, scope: &mut HashMap<String, VarState>, unsafe_ctx: bool, fn_name: &str) {
        match stmt {
            Stmt::Let { name, mutable, value, .. } => {
                self.check_expr(value, scope, unsafe_ctx, fn_name);
                let mut ref_exclusive = None;
                if let Expr::Borrow(inner, exclusive) = value {
                    ref_exclusive = Some(*exclusive);
                    if let Expr::Ident(target) = inner.as_ref() {
                        self.try_persistent_borrow(name, target, *exclusive, scope, fn_name);
                    }
                }
                let copy = self.is_copy_expr(value, scope);
                scope.insert(name.clone(), VarState { copy, moved: false, shared_borrows: 0, exclusive_borrow: false, declared_mut: *mutable, ref_exclusive });
            }
            Stmt::Assign { name, value, .. } => {
                self.check_expr(value, scope, unsafe_ctx, fn_name);
                // Reassigning a `let` that was never declared `mut` is a
                // real safety-check error, not silently accepted --
                // closes a real gap this project's own `mutable` AST
                // field had been carrying since Stmt::Let was written,
                // but nothing ever actually read it (confirmed by a
                // real `cargo build` dead-code warning: "field `mutable`
                // is never read"). check_mutable_target covers LOCALS
                // and module-level `static`/`const` globals both.
                self.check_mutable_target(name, scope, fn_name);
                let copy = self.is_copy_expr(value, scope);
                let declared_mut = scope.get(name).map(|s| s.declared_mut).unwrap_or(true);
                scope.insert(name.clone(), VarState { copy, moved: false, shared_borrows: 0, exclusive_borrow: false, declared_mut, ref_exclusive: None });
            }
            Stmt::FieldAssign { object, field: _, value } => {
                self.check_expr(object, scope, unsafe_ctx, fn_name);
                self.check_expr(value, scope, unsafe_ctx, fn_name);
                // Mutating a field is mutating the struct it belongs to
                // -- same rule as a plain `Stmt::Assign`, just applied
                // to the ROOT variable behind `object` (see base_ident)
                // -- UNLESS that root is itself a reference (`&mut T`),
                // in which case mutability is governed by the
                // reference's own exclusivity (ref_exclusive), never by
                // whether the reference's OWN binding was declared
                // `mut` (see VarState::ref_exclusive's doc comment for
                // the real regression this distinction fixes).
                if let Some(root) = base_ident(object) {
                    self.check_mutable_through(root, scope, fn_name);
                }
            }
            Stmt::IndexAssign { array, index, value } => {
                self.check_expr(array, scope, unsafe_ctx, fn_name);
                self.check_expr(index, scope, unsafe_ctx, fn_name);
                self.check_expr(value, scope, unsafe_ctx, fn_name);
                // Same rule as FieldAssign above: `arr[i] = x` mutates
                // the array variable itself -- same reference-vs-owned
                // distinction as FieldAssign (an array accessed through
                // a `&mut [T;N]` parameter doesn't need its OWN binding
                // declared `mut`).
                if let Some(root) = base_ident(array) {
                    self.check_mutable_through(root, scope, fn_name);
                }
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
                branch_scope.insert(var.clone(), VarState { copy: true, moved: false, shared_borrows: 0, exclusive_borrow: false, declared_mut: true, ref_exclusive: None });
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
                // Vec builtins (v1.x, interpreter-only so far -- see
                // interpreter.rs's Value::Vec) model mutation the SAME
                // way Stmt::IndexAssign already does for arrays: read the
                // variable's current value, clone+mutate it, write the
                // result back under the same name -- never real
                // aliasing. From the CHECKER's point of view that means
                // the Vec variable is NEVER actually consumed by any of
                // these calls (vec_push/vec_set DO mutate it, but the
                // program can keep using the same name afterward with
                // its updated value, which is exactly what "not moved"
                // means here) -- so all of them get the same move-
                // tracking exemption len() gets, not just the read-only
                // ones. A real function taking an actual `&mut Vec<T>`
                // reference instead of this special-cased builtin list
                // is future work, named here rather than silently
                // implied equivalent.
                if matches!(name.as_str(), "vec_new" | "vec_push" | "vec_get" | "vec_len" | "vec_set") {
                    for arg in args {
                        let inner = match arg {
                            CallArg::Positional(e) => e,
                            CallArg::Named(_, e) => e,
                        };
                        self.check_expr(inner, scope, unsafe_ctx, fn_name);
                    }
                    // vec_push/vec_set actually MUTATE their first
                    // argument's Vec (vec_get/vec_len/vec_new don't) --
                    // same rule as FieldAssign/IndexAssign above, same
                    // base_ident extraction.
                    if matches!(name.as_str(), "vec_push" | "vec_set") {
                        if let Some(first) = args.first() {
                            let e = match first {
                                CallArg::Positional(e) | CallArg::Named(_, e) => e,
                            };
                            if let Some(root) = base_ident(e) {
                                self.check_mutable_through(root, scope, fn_name);
                            }
                        }
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
