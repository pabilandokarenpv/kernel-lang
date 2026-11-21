// Kernel (.kn) AST — v1 core subset.
// Deliberately excludes generics/traits/enums/pattern-matching/closures for
// v1 (those were introduced for LLM-corpus syntax variety, not required to
// prove the language can boot a kernel). They get added to the interpreter
// incrementally when we build the LLM corpus generator.

#[derive(Debug, Clone)]
pub struct Program {
    pub items: Vec<Item>,
}

#[derive(Debug, Clone)]
pub enum Item {
    Function(FnDecl),
    Struct(StructDecl),
    Static { name: String, mutable: bool, ty: String, value: Expr },
    Const { name: String, ty: String, value: Expr },
    // `import foo` / `import utils.math` -- v1.x module system (Option C).
    // Resolved and ERASED entirely by modules.rs, the very first pass run
    // (before monomorphize/lambda_lift/check): it reads the target file
    // (dotted segments become path components, e.g. "utils.math" ->
    // "utils/math.kn", resolved relative to the IMPORTING file's own
    // directory -- no project-root/manifest concept needed for this),
    // recursively resolves ITS imports too, and splices its top-level
    // items directly into the importing program. No qualified names, no
    // pub/private visibility -- every imported item becomes a plain,
    // unqualified top-level name, exactly as if it had been pasted in.
    // A module imported from multiple places in the same program is only
    // included once (cycle/duplicate-safe, like a C header guard).
    // checker.rs/interpreter.rs/codegen_c.rs/monomorphize.rs/
    // lambda_lift.rs never see this variant -- it's always gone by the
    // time any of them run.
    Import(String),
    // A REAL tagged-union enum (v1.x, added as a follow-up to the earlier
    // C-style-only `enum` -- see EnumDecl's own doc comment for the full
    // design). Unlike Item::Import/the old flat-int-const desugaring,
    // this is NOT erased before the checker/interpreter/codegen run --
    // it's a genuinely new runtime type, same "real value, not an
    // erasure" treatment Vec<T> got.
    Enum(EnumDecl),
}

#[derive(Debug, Clone)]
// A tagged-union enum: a fixed, named set of VARIANTS, each optionally
// carrying its own typed payload fields (tuple-style, positional --
// matches Rust's `Variant(T, U)` shape, not a struct-style `Variant { x:
// T }` one, since Kernel has no need for named fields on something this
// small and it keeps construction syntax uniform with everything else
// that already uses positional/named call-style construction).
//
// Honest v1.x scope, stated once here rather than repeated at every call
// site downstream:
//   - Payload fields are PRIMITIVES ONLY (int/float/bool/str) -- no
//     struct/array/Vec/nested-enum payloads. Every backend's runtime
//     representation (a fixed-width tagged union -- literally a C
//     `union` in codegen_c.rs, a raw `[N x i64]` bit-storage array in
//     codegen_llvm.rs) depends on this; lifting it to arbitrary payload
//     types would need real size/alignment-aware layout, not just a
//     wider union member, and is named future work, not silently
//     miscompiled.
//   - Variant names must be GLOBALLY unique across every enum in the
//     whole program (checker-enforced, a clear error on collision) --
//     Kernel has no `::`-qualified paths to disambiguate `Enum1::X` from
//     `Enum2::X` the way Rust can, so a bare `X` must unambiguously mean
//     one specific variant of one specific enum everywhere it's written.
//   - If EVERY variant of an enum has zero fields, it's handled by the
//     OLD, pre-existing flat-int-const desugar (parser.rs) completely
//     unchanged -- `enum Color: Red / Green / Blue` with bare `Red`/
//     `Green` usage (examples/enums.kn, match_test.kn, predate this
//     feature) keeps working byte-for-byte identically; this EnumDecl
//     type is never even constructed for them.
//   - Construction for a MIXED enum (at least one variant carries a
//     field) is ALWAYS call syntax, for EVERY variant regardless of its
//     own arity -- `Circle(5)` AND `Empty()`, never a bare `Empty`.
//     Deliberately uniform rather than Rust's own "bare unit variant"
//     convention: keeps every mixed-enum variant construction/pattern
//     the same shape (tag+field comparison, see Expr::EnumTag/
//     Expr::EnumField in ast.rs) with NO need for enum-value
//     structural equality (`==` between two full enum values) anywhere
//     in checker/interpreter/codegen -- a real simplification, not an
//     oversight, named here once rather than re-derived at every call
//     site downstream.
//   - No exhaustiveness checking on `match` (same honest gap as
//     struct/array field access generally -- v1.x's checker doesn't do
//     full control-flow-sensitive analysis anywhere yet, see ARD-003) --
//     an unmatched variant at runtime simply falls through to the
//     wildcard arm if present, or executes no arm's body at all if not
//     (never a panic, same as a `match` that falls through today).
//   - Enum values are move-by-default (non-Copy), exactly like structs
//     and arrays -- moving one moves the whole tagged value.
pub struct EnumDecl {
    pub name: String,
    pub variants: Vec<(String, Vec<String>)>, // (variant name, field types, in order)
}

#[derive(Debug, Clone)]
pub struct FnDecl {
    pub name: String,
    pub is_unsafe: bool,
    pub type_params: Vec<String>, // generics (v1.x): e.g. ["T"] for fn identity<T>(x: T) -> T.
    // Empty for an ordinary non-generic function. Generic functions never
    // reach the checker/interpreter/codegen directly -- monomorphize.rs
    // substitutes each distinct concrete type actually used at a call site
    // and emits a fully concrete FnDecl per instantiation before checking
    // ever runs, so nothing downstream needs to know generics exist at all.
    pub params: Vec<Param>,
    pub return_ty: Option<String>,
    pub body: Vec<Stmt>,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub ty: String,
    pub borrowed: bool, // &T or &mut T
    pub exclusive: bool, // true for &mut T, false for &T or owned T -- see
    // checker.rs's borrow-conflict tracking (v1.x addition): this is what
    // lets the checker distinguish a shared borrow (many allowed at once)
    // from an exclusive one (must be the only live borrow).
    pub mutable: bool,  // mut T / self mut -- the BINDING is reassignable;
    // unrelated to `exclusive` above, which is about reference kind, not
    // binding mutability. Two different concepts that happen to both use
    // the word "mut" in source syntax.
}

#[derive(Debug, Clone)]
pub struct StructDecl {
    pub name: String,
    pub type_params: Vec<String>, // generics (v1.x), same role as FnDecl's
    pub fields: Vec<(String, String)>, // (field name, type)
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Let { name: String, mutable: bool, ty: Option<String>, value: Expr },
    Assign { name: String, op: AssignOp, value: Expr },
    FieldAssign { object: Expr, field: String, value: Expr },
    // `arr[i] = value` (v1.x, fixed-size arrays only -- see Expr::ArrayLit's
    // doc comment). `array` is the expression being indexed (an ident or a
    // field access); interpreter/codegen restrict it to a plain ident, same
    // existing precedent as FieldAssign's `object`.
    IndexAssign { array: Expr, index: Expr, value: Expr },
    If { arms: Vec<(Expr, Vec<Stmt>)>, else_body: Option<Vec<Stmt>> },
    While { cond: Expr, body: Vec<Stmt> },
    For { var: String, iter: Expr, body: Vec<Stmt> },
    Return(Option<Expr>),
    Unsafe(Vec<Stmt>),
    Expr(Expr),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AssignOp {
    Set,    // =
    Add,    // +=
    Sub,    // -=
}

#[derive(Debug, Clone)]
pub enum Expr {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    Ident(String),
    Range(Box<Expr>, Box<Expr>), // a..b
    Unary(UnaryOp, Box<Expr>),
    Binary(Box<Expr>, BinOp, Box<Expr>),
    Field(Box<Expr>, String),
    Borrow(Box<Expr>, bool),     // &expr (false) or &mut expr (true)
    Call { name: String, args: Vec<CallArg> },
    // Fixed-size array literal `[e1, e2, ...]` -- v1.x arrays (added
    // alongside generics/match). Honest scope, stated here once rather than
    // repeated at every call site:
    //   - FIXED SIZE ONLY, known at parse time from the literal's own
    //     element count (or the `[T;N]` type annotation on a param/let).
    //     No dynamic-length slices/Vec yet -- that's a real future lift
    //     (fat pointer + length, heap or arena allocation), not just this.
    //   - Element type is inferred from the first element; mixed-type
    //     literals aren't checked for homogeneity (same "not full
    //     Hindley-Milner" caveat as monomorphize.rs's type inference).
    //   - Move/borrow semantics mirror struct construction exactly: moving
    //     an array moves the whole array, `&`/`&mut` borrow it, matching
    //     real soundness reasoning (see checker.rs) -- the C backend always
    //     passes array parameters as a decayed pointer (C has no other way
    //     to pass a fixed array), but since the checker forbids using a
    //     moved-from array again, that's invisible to the Kernel program;
    //     no Kernel-visible aliasing bug results.
    //   - Array literals render as bare C brace-init (`{1, 2, 3}`) in a
    //     `let`/struct-field initializer position, and as a C99
    //     compound-literal cast (`(long[]){1, 2, 3}`) as a bare function
    //     call argument -- both verified end to end (see
    //     examples/arrays_test.kn). Any OTHER expression position (e.g.
    //     inside arithmetic) isn't specifically handled and will surface
    //     as a real `gcc` compile error rather than a Kernel-level one, a
    //     known rough edge.
    ArrayLit(Vec<Expr>),
    // `arr[i]` (read position). Bounds-checked at RUNTIME in both the
    // interpreter (a safe Rust panic) and the C codegen backend (an
    // explicit inline check against the array's known compile-time size,
    // calling the freestanding `kernel_panic` halt on failure) -- no
    // compile-time bounds checking in v1.x.
    Index(Box<Expr>, Box<Expr>),
    // Closure literal `|x: int, ...| -> ret:` + an indented block body --
    // v1.x "basic closures," scoped deliberately narrow and erased
    // entirely by a dedicated LAMBDA-LIFTING pass (lambda_lift.rs) between
    // monomorphize and the checker, same "erase via AST-to-AST pass"
    // philosophy as match/generics -- the checker/interpreter/codegen
    // never see this variant; it's always gone before they run.
    //   - Only ever valid as the entire value of a `let` statement (see
    //     parser.rs's parse_stmt) -- no closures as call arguments, struct
    //     fields, or any other expression position.
    //   - NON-ESCAPING ONLY: the bound name may be CALLED (by name) within
    //     the same function it was defined in (including nested blocks in
    //     lexical scope), never passed as a value to another function,
    //     returned, stored in a struct, etc. Referencing the name any way
    //     other than as a call callee is a loud lambda-lift-time error --
    //     there is no closure VALUE representation at all (no fat
    //     pointer/env-struct machinery); the closure is converted straight
    //     into a real top-level function with its captured free variables
    //     as extra leading parameters, and every call site is rewritten to
    //     pass them explicitly. This is why it can't escape: nothing
    //     downstream of lambda-lifting ever represents "a closure," only
    //     ordinary functions and ordinary calls.
    //   - Captures PRIMITIVE free variables ONLY (int/float/str/bool,
    //     always Copy) -- capturing a struct or array is a lambda-lift
    //     error, not a silent miscompile; real capture-by-move semantics
    //     for compound types is named future work, not implemented here.
    //   - No recursive closures (a closure can't call itself by name).
    Closure { params: Vec<Param>, return_ty: Option<String>, body: Vec<Stmt> },
    // Synthesized ONLY by parser.rs's match-desugaring for an enum-variant
    // pattern (`match s: Circle(r): ...`) -- never written directly in
    // source. Extracts the scrutinee's tag as an int so the desugared
    // `if` can compare it against the matched variant's known index.
    // Kept as a real typed Expr (not a fake builtin-function call) so
    // every backend's existing type-inference machinery already knows
    // what to do with a plain int-returning expression.
    EnumTag(Box<Expr>),
    // The compile/interpret-time-resolved tag CONSTANT for a named
    // variant (e.g. `EnumVariantTag("Circle")` -> whatever index
    // `Circle` was declared at in its enum, an int). Synthesized ONLY
    // by match-desugaring, as the right-hand side of an `EnumTag(...)
    // == EnumVariantTag(...)` comparison -- never written directly in
    // source, and deliberately a DIFFERENT node than EnumTag (which
    // reads a VALUE's tag at runtime): this one names a variant
    // directly, with no scrutinee expression to evaluate at all. Every
    // backend resolves it via the same variant-lookup table it already
    // needs for construction validation, so this adds no new lookup
    // machinery, just a new place that calls into it.
    EnumVariantTag(String),
    // Synthesized ONLY by parser.rs's match-desugaring, one per pattern
    // binding (`Circle(r)` makes one `EnumField(scrutinee, "Circle", 0)`
    // bound to `r` via a synthesized `let`). `variant`+`index` together
    // resolve the field's real Kernel type at check/codegen time (the
    // SAME field index means a different type under a different
    // variant, so both are needed, not just the index alone).
    EnumField(Box<Expr>, String, usize),
}

#[derive(Debug, Clone)]
pub enum CallArg {
    Positional(Expr),
    Named(String, Expr),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UnaryOp {
    Neg,
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BinOp {
    Add, Sub, Mul, Div, Mod,
    Eq, NotEq, Lt, Gt, LtEq, GtEq,
    And, Or,
}
