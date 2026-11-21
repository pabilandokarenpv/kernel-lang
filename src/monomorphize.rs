// Kernel (.kn) generics — v1.x, via monomorphization, matching how C++
// templates and Rust generics both actually compile under the hood: a
// generic function/struct is never itself emitted into the final program.
// Every DISTINCT concrete type it's ever used with gets its own fully
// concrete, specialized copy generated instead, with the type parameter
// substituted everywhere. This runs as one AST-to-AST pass between parsing
// and checking (matching ARD-002's "no separate IR yet, keep each stage
// simple and auditable" philosophy) -- the checker, interpreter, and
// codegen never need to know generics exist at all; they only ever see
// fully concrete, already-specialized code, exactly as if it had been
// hand-written that way.
//
// Scope, stated honestly (v1.x):
//   - Exactly ONE type parameter per generic function/struct is supported
//     (`fn identity<T>(...)`, `struct Box<T>: ...`) -- multiple simultaneous
//     type parameters (`fn pair<T, U>(...)`) parse (parse_optional_type_params
//     accepts a list), but this pass only infers and substitutes the FIRST
//     one; a second parameter is left as a literal, uninferred type name,
//     which will surface as a clear "struct/type not found" error further
//     down the pipeline rather than silently miscompiling. Named future
//     work, not a silent gap.
//   - Type inference is a simple, local heuristic (literals, struct
//     constructions, variable types tracked via `let`/parameter
//     declarations, and simple pass-through for borrows/unary/binary
//     expressions) -- not a full Hindley-Milner-style type system. If
//     inference fails, the call/construction is left as-is, which fails
//     later with a clear error instead of silently miscompiling.

use crate::ast::*;
use std::collections::HashMap;

pub fn monomorphize(program: Program) -> Program {
    let mut generic_fns: HashMap<String, FnDecl> = HashMap::new();
    let mut generic_structs: HashMap<String, StructDecl> = HashMap::new();
    let mut concrete_items: Vec<Item> = Vec::new();

    for item in program.items {
        match item {
            Item::Function(f) if !f.type_params.is_empty() => {
                generic_fns.insert(f.name.clone(), f);
            }
            Item::Struct(s) if !s.type_params.is_empty() => {
                generic_structs.insert(s.name.clone(), s);
            }
            other => concrete_items.push(other),
        }
    }

    if generic_fns.is_empty() && generic_structs.is_empty() {
        return Program { items: concrete_items };
    }

    let mut mono = Monomorphizer {
        generic_fns,
        generic_structs,
        instantiated: HashMap::new(),
        struct_field_types: HashMap::new(),
        newly_generated: Vec::new(),
    };
    // Needed so infer_expr_type can resolve "a struct-construction call
    // returns that struct's own name" for ALREADY-concrete structs too,
    // not just generic ones.
    for item in &concrete_items {
        if let Item::Struct(s) = item {
            mono.struct_field_types.insert(
                s.name.clone(),
                s.fields.iter().map(|(n, t)| (n.clone(), t.clone())).collect(),
            );
        }
    }

    // Fixed-point loop: a freshly-generated monomorphized item might itself
    // call another generic item, so keep sweeping until one full pass finds
    // no new instantiations.
    loop {
        let before = mono.instantiated.len();
        for item in concrete_items.iter_mut() {
            mono.rewrite_item(item);
        }
        let mut newly_added = mono.take_newly_generated();
        for item in &newly_added {
            if let Item::Struct(s) = item {
                mono.struct_field_types.insert(
                    s.name.clone(),
                    s.fields.iter().map(|(n, t)| (n.clone(), t.clone())).collect(),
                );
            }
        }
        concrete_items.append(&mut newly_added);
        if mono.instantiated.len() == before {
            break;
        }
    }

    Program { items: concrete_items }
}

struct Monomorphizer {
    generic_fns: HashMap<String, FnDecl>,
    generic_structs: HashMap<String, StructDecl>,
    // (generic name, concrete type) -> mangled name, plus tracks which
    // instantiations have already had their concrete item generated.
    instantiated: HashMap<(String, String), String>,
    // name -> (field name -> field type), for every struct seen so far
    // (concrete ones from the program, plus freshly monomorphized ones) --
    // lets infer_expr_type resolve a struct-construction call's type.
    struct_field_types: HashMap<String, HashMap<String, String>>,
    // Concrete items generated THIS pass, drained after each sweep.
    newly_generated: Vec<Item>,
}

impl Monomorphizer {
    fn take_newly_generated(&mut self) -> Vec<Item> {
        std::mem::take(&mut self.newly_generated)
    }

    fn mangle(name: &str, concrete_ty: &str) -> String {
        format!("{}__{}", name, concrete_ty.replace(' ', "_"))
    }

    // Finds or creates the monomorphized name for (generic_name, concrete_ty),
    // generating the specialized FnDecl/StructDecl the first time.
    fn get_or_instantiate(&mut self, generic_name: &str, concrete_ty: &str) -> Option<String> {
        let key = (generic_name.to_string(), concrete_ty.to_string());
        if let Some(existing) = self.instantiated.get(&key) {
            return Some(existing.clone());
        }
        if let Some(f) = self.generic_fns.get(generic_name).cloned() {
            let mangled = Self::mangle(generic_name, concrete_ty);
            let type_param = f.type_params[0].clone();
            let specialized = FnDecl {
                name: mangled.clone(),
                is_unsafe: f.is_unsafe,
                type_params: Vec::new(),
                params: f
                    .params
                    .iter()
                    .map(|p| Param {
                        name: p.name.clone(),
                        ty: substitute_ty(&p.ty, &type_param, concrete_ty),
                        borrowed: p.borrowed,
                        exclusive: p.exclusive,
                        mutable: p.mutable,
                    })
                    .collect(),
                return_ty: f.return_ty.as_ref().map(|t| substitute_ty(t, &type_param, concrete_ty)),
                body: f.body.clone(),
            };
            self.instantiated.insert(key, mangled.clone());
            self.newly_generated.push(Item::Function(specialized));
            return Some(mangled);
        }
        if let Some(s) = self.generic_structs.get(generic_name).cloned() {
            let mangled = Self::mangle(generic_name, concrete_ty);
            let type_param = s.type_params[0].clone();
            let specialized = StructDecl {
                name: mangled.clone(),
                type_params: Vec::new(),
                fields: s
                    .fields
                    .iter()
                    .map(|(n, t)| (n.clone(), substitute_ty(t, &type_param, concrete_ty)))
                    .collect(),
            };
            self.instantiated.insert(key, mangled.clone());
            self.newly_generated.push(Item::Struct(specialized));
            return Some(mangled);
        }
        None
    }

    // Best-effort, local type inference -- literals, struct constructions,
    // and variable types tracked via `var_types`; deliberately not a full
    // type system (see the module-level doc comment for the honest scope).
    fn infer_expr_type(&self, expr: &Expr, var_types: &HashMap<String, String>) -> Option<String> {
        match expr {
            Expr::Int(_) => Some("int".to_string()),
            Expr::Float(_) => Some("float".to_string()),
            Expr::Str(_) => Some("str".to_string()),
            Expr::Bool(_) => Some("bool".to_string()),
            Expr::Ident(name) => var_types.get(name).cloned(),
            Expr::Borrow(inner, _) => self.infer_expr_type(inner, var_types),
            Expr::Unary(_, inner) => self.infer_expr_type(inner, var_types),
            Expr::Binary(l, _, _) => self.infer_expr_type(l, var_types),
            // Arrays aren't generic-type-aware in v1.x -- monomorphization
            // never needs to resolve an array's element type, only whether
            // a CALL/CONSTRUCTION argument's type matches a generic's sole
            // type parameter, and an array literal/index is never that.
            Expr::ArrayLit(_) | Expr::Index(..) => None,
            // A closure literal isn't itself a value monomorphize needs to
            // type -- it's erased by lambda_lift.rs before anything
            // downstream (including this pass's own second sweep, since
            // lambda-lift runs after monomorphize) ever needs its type.
            Expr::Closure { .. } => None,
            Expr::Call { name, .. } => {
                if self.struct_field_types.contains_key(name) {
                    Some(name.clone())
                } else if let Some(f) = self.generic_fns.get(name) {
                    f.return_ty.clone() // still generic -- caller can't use this directly, but avoids a crash
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn rewrite_item(&mut self, item: &mut Item) {
        if let Item::Function(f) = item {
            let mut var_types: HashMap<String, String> = HashMap::new();
            for p in &f.params {
                var_types.insert(p.name.clone(), p.ty.clone());
            }
            self.rewrite_block(&mut f.body, &mut var_types);
        }
    }

    fn rewrite_block(&mut self, stmts: &mut [Stmt], var_types: &mut HashMap<String, String>) {
        for stmt in stmts.iter_mut() {
            self.rewrite_stmt(stmt, var_types);
        }
    }

    fn rewrite_stmt(&mut self, stmt: &mut Stmt, var_types: &mut HashMap<String, String>) {
        match stmt {
            Stmt::Let { name, ty, value, .. } => {
                self.rewrite_expr(value, var_types);
                let inferred = ty.clone().or_else(|| self.infer_expr_type(value, var_types));
                if let Some(t) = inferred {
                    var_types.insert(name.clone(), t);
                }
            }
            Stmt::Assign { value, .. } => self.rewrite_expr(value, var_types),
            Stmt::FieldAssign { object, value, .. } => {
                self.rewrite_expr(object, var_types);
                self.rewrite_expr(value, var_types);
            }
            Stmt::IndexAssign { array, index, value } => {
                self.rewrite_expr(array, var_types);
                self.rewrite_expr(index, var_types);
                self.rewrite_expr(value, var_types);
            }
            Stmt::If { arms, else_body } => {
                for (cond, body) in arms {
                    self.rewrite_expr(cond, var_types);
                    let mut branch_vars = var_types.clone();
                    self.rewrite_block(body, &mut branch_vars);
                }
                if let Some(body) = else_body {
                    let mut branch_vars = var_types.clone();
                    self.rewrite_block(body, &mut branch_vars);
                }
            }
            Stmt::While { cond, body } => {
                self.rewrite_expr(cond, var_types);
                let mut branch_vars = var_types.clone();
                self.rewrite_block(body, &mut branch_vars);
            }
            Stmt::For { var, iter, body } => {
                self.rewrite_expr(iter, var_types);
                let mut branch_vars = var_types.clone();
                branch_vars.insert(var.clone(), "int".to_string());
                self.rewrite_block(body, &mut branch_vars);
            }
            Stmt::Return(Some(e)) => self.rewrite_expr(e, var_types),
            Stmt::Return(None) => {}
            Stmt::Unsafe(body) => self.rewrite_block(body, var_types),
            Stmt::Expr(e) => self.rewrite_expr(e, var_types),
        }
    }

    fn rewrite_expr(&mut self, expr: &mut Expr, var_types: &mut HashMap<String, String>) {
        match expr {
            Expr::Int(_) | Expr::Float(_) | Expr::Str(_) | Expr::Bool(_) | Expr::Ident(_) => {}
            Expr::Range(a, b) => {
                self.rewrite_expr(a, var_types);
                self.rewrite_expr(b, var_types);
            }
            Expr::Unary(_, e) => self.rewrite_expr(e, var_types),
            Expr::Binary(l, _, r) => {
                self.rewrite_expr(l, var_types);
                self.rewrite_expr(r, var_types);
            }
            Expr::Field(obj, _) => self.rewrite_expr(obj, var_types),
            Expr::Index(arr, idx) => {
                self.rewrite_expr(arr, var_types);
                self.rewrite_expr(idx, var_types);
            }
            Expr::ArrayLit(elems) => {
                for e in elems.iter_mut() {
                    self.rewrite_expr(e, var_types);
                }
            }
            // The closure's OWN body can still call a generic function
            // that needs monomorphizing (lambda-lift hasn't erased the
            // closure yet at this point in the pipeline -- it runs after
            // this pass) -- recurse into it with a scope seeded by the
            // closure's own params, same pattern as rewrite_item for a
            // top-level function.
            Expr::Closure { params, body, .. } => {
                let mut closure_vars = var_types.clone();
                for p in params.iter() {
                    closure_vars.insert(p.name.clone(), p.ty.clone());
                }
                self.rewrite_block(body, &mut closure_vars);
            }
            Expr::Borrow(inner, _) => self.rewrite_expr(inner, var_types),
            Expr::EnumTag(e) => self.rewrite_expr(e, var_types),
            Expr::EnumVariantTag(_) => {}
            Expr::EnumField(e, _, _) => self.rewrite_expr(e, var_types),
            Expr::Call { name, args } => {
                for arg in args.iter_mut() {
                    let inner = match arg {
                        CallArg::Positional(e) => e,
                        CallArg::Named(_, e) => e,
                    };
                    self.rewrite_expr(inner, var_types);
                }
                if let Some(mangled) = self.try_instantiate_call(name, args, var_types) {
                    *name = mangled;
                }
            }
        }
    }

    // If `name` refers to a generic function/struct, infers its (single)
    // type parameter from the matching argument/field and rewrites the
    // call to the monomorphized name -- returns None (leaving the call
    // untouched) if `name` isn't generic, or if inference fails.
    fn try_instantiate_call(&mut self, name: &str, args: &[CallArg], var_types: &HashMap<String, String>) -> Option<String> {
        if let Some(f) = self.generic_fns.get(name).cloned() {
            let type_param = f.type_params.first()?;
            let param_index = f.params.iter().position(|p| &p.ty == type_param)?;
            let arg_expr = args.get(param_index).map(|a| match a {
                CallArg::Positional(e) => e,
                CallArg::Named(_, e) => e,
            })?;
            let concrete_ty = self.infer_expr_type(arg_expr, var_types)?;
            return self.get_or_instantiate(name, &concrete_ty);
        }
        if let Some(s) = self.generic_structs.get(name).cloned() {
            let type_param = s.type_params.first()?;
            let field_index = s.fields.iter().position(|(_, t)| t == type_param)?;
            let (field_name, _) = &s.fields[field_index];
            let arg_expr = args.iter().find_map(|a| match a {
                CallArg::Named(n, e) if n == field_name => Some(e),
                _ => None,
            })?;
            let concrete_ty = self.infer_expr_type(arg_expr, var_types)?;
            return self.get_or_instantiate(name, &concrete_ty);
        }
        None
    }
}

// Replaces every exact occurrence of `type_param` within a type string with
// `concrete_ty` -- Kernel's types are plain strings (see ast.rs), so this is
// just a scoped find-replace, not a structured substitution. Handles the
// bare case (`ty == "T"` -> `ty == concrete_ty`) which covers every pattern
// v1.x's grammar actually produces (no nested generics like `Box<T>` inside
// another generic's fields yet -- a real, named next increment).
fn substitute_ty(ty: &str, type_param: &str, concrete_ty: &str) -> String {
    if ty == type_param {
        concrete_ty.to_string()
    } else {
        ty.to_string()
    }
}
