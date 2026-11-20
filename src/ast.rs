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
