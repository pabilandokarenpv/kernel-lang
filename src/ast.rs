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
    pub params: Vec<Param>,
    pub return_ty: Option<String>,
    pub body: Vec<Stmt>,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub ty: String,
    pub borrowed: bool, // &T
    pub mutable: bool,  // mut T / self mut
}

#[derive(Debug, Clone)]
pub struct StructDecl {
    pub name: String,
    pub fields: Vec<(String, String)>, // (field name, type)
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Let { name: String, mutable: bool, ty: Option<String>, value: Expr },
    Assign { name: String, op: AssignOp, value: Expr },
    FieldAssign { object: Expr, field: String, value: Expr },
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
    Borrow(Box<Expr>),           // &expr
    Call { name: String, args: Vec<CallArg> },
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
