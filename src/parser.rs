// Kernel (.kn) parser — v1 core subset.
// Recursive descent, operator precedence climbing for expressions.

use crate::ast::*;
use crate::lexer::Token;

pub struct Parser {
    tokens: Vec<(Token, usize, usize)>,
    pos: usize,
}

impl Parser {
    pub fn new(tokens: Vec<(Token, usize, usize)>) -> Self {
        Parser { tokens, pos: 0 }
    }

    fn cur(&self) -> &Token {
        self.tokens.get(self.pos).map(|t| &t.0).unwrap_or(&Token::Eof)
    }

    // (line, col) of the CURRENT (not-yet-consumed) token -- for error
    // messages and, eventually, LSP diagnostics. 1-indexed, (0, 0) past
    // end of input.
    fn cur_pos(&self) -> (usize, usize) {
        self.tokens.get(self.pos).map(|t| (t.1, t.2)).unwrap_or((0, 0))
    }

    fn advance(&mut self) -> Token {
        let t = self.tokens.get(self.pos).map(|t| t.0.clone()).unwrap_or(Token::Eof);
        self.pos += 1;
        t
    }

    fn check(&self, t: &Token) -> bool {
        self.cur() == t
    }

    fn eat(&mut self, t: &Token) -> bool {
        if self.check(t) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, t: Token) -> Token {
        if self.check(&t) {
            self.advance()
        } else {
            let (line, col) = self.cur_pos();
            panic!("Kernel parser: expected {:?}, found {:?} at line {}, column {}", t, self.cur(), line, col);
        }
    }

    fn skip_newlines(&mut self) {
        while self.check(&Token::Newline) {
            self.advance();
        }
    }

    fn ident(&mut self) -> String {
        let (line, col) = self.cur_pos(); // captured BEFORE advance() below moves past it
        match self.advance() {
            Token::Ident(s) => s,
            other => panic!("Kernel parser: expected identifier, found {:?} at line {}, column {}", other, line, col),
        }
    }

    // a type annotation is, for v1, just a run of tokens up to the next
    // structural token (Colon/Comma/RParen/Assign/Arrow/Newline). We don't
    // need a structured type system yet to interpret/codegen the core subset.
    fn parse_type(&mut self) -> String {
        let mut s = String::new();
        let mut depth = 0;
        loop {
            match self.cur() {
                Token::LAngle | Token::Lt if depth >= 0 => {
                    depth += 1;
                    s.push('<');
                    self.advance();
                }
                Token::Gt if depth > 0 => {
                    depth -= 1;
                    s.push('>');
                    self.advance();
                }
                Token::Amp => {
                    s.push('&');
                    self.advance();
                }
                Token::Mut => {
                    s.push_str("mut ");
                    self.advance();
                }
                Token::Ident(name) => {
                    s.push_str(name);
                    self.advance();
                    if depth == 0 {
                        break;
                    }
                }
                Token::LBracket => {
                    // Array type `[ElemType;N]` -- self-contained (element
                    // type can itself be any type string, e.g. a struct
                    // name) and always terminal in v1.x: no nested arrays.
                    // Reached either as the whole type (`[int;5]`) or after
                    // the Amp branch above already pushed '&' (`&[int;5]`).
                    self.advance(); // consume '['
                    let elem_ty = self.parse_type();
                    self.expect(Token::Semicolon);
                    let size = match self.advance() {
                        Token::Int(n) => n,
                        other => panic!(
                            "Kernel parser: expected an integer array size after `;` in an array type, found {:?}",
                            other
                        ),
                    };
                    self.expect(Token::RBracket);
                    s.push_str(&format!("[{};{}]", elem_ty, size));
                    break;
                }
                _ => break,
            }
        }
        s
    }

    pub fn parse_program(&mut self) -> Program {
        let mut items = Vec::new();
        self.skip_newlines();
        while !self.check(&Token::Eof) {
            if self.check(&Token::Enum) {
                items.extend(self.parse_enum_desugared());
            } else {
                items.push(self.parse_item());
            }
            self.skip_newlines();
        }
        Program { items }
    }

    /// `enum Color:` / `    Red` / `    Green` desugars to plain `const`
    /// int items (`const Red: int = 0`, `const Green: int = 1`, ...) —
    /// deliberately C-style (a named integer constant, no associated
    /// data/tagged union) so it needs zero changes to the checker,
    /// interpreter, or codegen: those already handle `Item::Const`
    /// correctly, an enum variant just IS an int under the hood. Full
    /// Rust-style enums with associated data and pattern matching remain
    /// out of v1's scope (ARD-001) — this gives real enum ergonomics
    /// (named constants, no magic numbers) without that larger lift.
    fn parse_enum_desugared(&mut self) -> Vec<Item> {
        self.expect(Token::Enum);
        let _name = self.ident(); // the enum's own name isn't needed once desugared to flat consts
        self.expect(Token::Colon);
        self.expect(Token::Newline);
        self.expect(Token::Indent);
        let mut items = Vec::new();
        let mut next_value: i64 = 0;
        loop {
            self.skip_newlines();
            if self.check(&Token::Dedent) {
                break;
            }
            let variant_name = self.ident();
            items.push(Item::Const {
                name: variant_name,
                ty: "int".to_string(),
                value: Expr::Int(next_value),
            });
            next_value += 1;
            self.skip_newlines();
        }
        self.expect(Token::Dedent);
        items
    }

    fn parse_item(&mut self) -> Item {
        let is_unsafe = self.eat(&Token::Unsafe);
        match self.cur().clone() {
            Token::Fn => {
                self.advance();
                Item::Function(self.parse_fn_rest(is_unsafe))
            }
            Token::Struct => {
                self.advance();
                Item::Struct(self.parse_struct())
            }
            Token::Static => {
                self.advance();
                let mutable = self.eat(&Token::Mut);
                let name = self.ident();
                self.expect(Token::Colon);
                let ty = self.parse_type();
                self.expect(Token::Assign);
                let value = self.parse_expr();
                Item::Static { name, mutable, ty, value }
            }
            Token::Const => {
                self.advance();
                let name = self.ident();
                self.expect(Token::Colon);
                let ty = self.parse_type();
                self.expect(Token::Assign);
                let value = self.parse_expr();
                Item::Const { name, ty, value }
            }
            Token::Import => {
                self.advance();
                // Dotted module path, e.g. `import utils.math` -- see
                // ast.rs's Item::Import doc comment for how modules.rs
                // resolves this to a file.
                let mut path = self.ident();
                while self.eat(&Token::Dot) {
                    path.push('.');
                    path.push_str(&self.ident());
                }
                Item::Import(path)
            }
            other => panic!("Kernel parser: unexpected top-level token {:?}", other),
        }
    }

    // Parses an optional `<T>` or `<T, U, ...>` type-parameter list right
    // after a function or struct name -- shared between parse_fn_rest and
    // parse_struct since the syntax (and meaning: these are generics, see
    // ast.rs's FnDecl/StructDecl.type_params) is identical in both spots.
    fn parse_optional_type_params(&mut self) -> Vec<String> {
        let mut type_params = Vec::new();
        if self.eat(&Token::Lt) {
            loop {
                type_params.push(self.ident());
                if !self.eat(&Token::Comma) {
                    break;
                }
            }
            self.expect(Token::Gt);
        }
        type_params
    }

    fn parse_fn_rest(&mut self, is_unsafe: bool) -> FnDecl {
        let name = self.ident();
        let type_params = self.parse_optional_type_params();
        self.expect(Token::LParen);
        let mut params = Vec::new();
        while !self.check(&Token::RParen) {
            let mutable = self.eat(&Token::Mut);
            let pname = self.ident();
            self.expect(Token::Colon);
            // Kernel's actual reference syntax puts `&` on the TYPE side
            // (`p: &Point`, matching every example and the README), not
            // before the parameter name. parse_type() already folds a
            // leading `&` into the returned type string (e.g. "&Point") —
            // detect and strip it here so `ty` is always the bare struct
            // name everywhere downstream (checker's struct-name lookup,
            // codegen's c_type()), with `borrowed` as the single source of
            // truth for "this is a reference," instead of leaving the `&`
            // baked into a string two different ways in two different
            // places (found as a real bug: the checker's `.structs.contains(&p.ty)`
            // lookup silently failed for "&Point" since only "Point" is
            // registered, and codegen's fn_signature always emitted the
            // struct by value since `borrowed` here was dead/always-false).
            let raw_ty = self.parse_type();
            let (borrowed, rest) = match raw_ty.strip_prefix('&') {
                Some(rest) => (true, rest.to_string()),
                None => (false, raw_ty),
            };
            // &mut T in a parameter's type folds "mut " onto the string
            // right after the stripped '&' (parse_type() emits "&mut Point"
            // as one run) -- strip that too, same reasoning as the `&`
            // strip above: leave `exclusive` as the single source of truth
            // for "this is a &mut reference" rather than letting "mut "
            // survive inside `ty` where it would break struct-name lookups
            // exactly the way the Revision 3 bug (see ARD-003) did for `&`.
            let (exclusive, ty) = if borrowed {
                match rest.strip_prefix("mut ") {
                    Some(r) => (true, r.to_string()),
                    None => (false, rest),
                }
            } else {
                (false, rest)
            };
            params.push(Param { name: pname, ty, borrowed, exclusive, mutable });
            if !self.eat(&Token::Comma) {
                break;
            }
        }
        self.expect(Token::RParen);
        let return_ty = if self.eat(&Token::Arrow) {
            Some(self.parse_type())
        } else {
            None
        };
        self.expect(Token::Colon);
        let body = self.parse_block();
        FnDecl { name, is_unsafe, type_params, params, return_ty, body }
    }

    // Closure literal: `|x: int, y: int| -> int:` + an indented block --
    // see ast.rs's Expr::Closure doc comment for v1.x's scope (non-
    // escaping, lambda-lifted away entirely before the checker runs).
    // Param parsing deliberately mirrors parse_fn_rest's loop above (same
    // &/&mut-stripping logic) but terminates on Pipe instead of RParen;
    // not factored into one shared helper since the two terminators and
    // surrounding delimiters differ enough that sharing would need its own
    // parameter just to carry the terminator token.
    fn parse_closure_literal(&mut self) -> Expr {
        self.expect(Token::Pipe);
        let mut params = Vec::new();
        while !self.check(&Token::Pipe) {
            let mutable = self.eat(&Token::Mut);
            let pname = self.ident();
            self.expect(Token::Colon);
            let raw_ty = self.parse_type();
            let (borrowed, rest) = match raw_ty.strip_prefix('&') {
                Some(rest) => (true, rest.to_string()),
                None => (false, raw_ty),
            };
            let (exclusive, ty) = if borrowed {
                match rest.strip_prefix("mut ") {
                    Some(r) => (true, r.to_string()),
                    None => (false, rest),
                }
            } else {
                (false, rest)
            };
            params.push(Param { name: pname, ty, borrowed, exclusive, mutable });
            if !self.eat(&Token::Comma) {
                break;
            }
        }
        self.expect(Token::Pipe);
        let return_ty = if self.eat(&Token::Arrow) {
            Some(self.parse_type())
        } else {
            None
        };
        self.expect(Token::Colon);
        let body = self.parse_block();
        Expr::Closure { params, return_ty, body }
    }

    fn parse_struct(&mut self) -> StructDecl {
        let name = self.ident();
        let type_params = self.parse_optional_type_params();
        self.expect(Token::Colon);
        self.expect(Token::Newline);
        self.expect(Token::Indent);
        let mut fields = Vec::new();
        loop {
            self.skip_newlines();
            if self.check(&Token::Dedent) {
                break;
            }
            let fname = self.ident();
            self.expect(Token::Colon);
            let fty = self.parse_type();
            fields.push((fname, fty));
            self.skip_newlines();
        }
        self.expect(Token::Dedent);
        StructDecl { name, type_params, fields }
    }

    fn parse_block(&mut self) -> Vec<Stmt> {
        self.expect(Token::Newline);
        self.expect(Token::Indent);
        let mut stmts = Vec::new();
        loop {
            self.skip_newlines();
            if self.check(&Token::Dedent) || self.check(&Token::Eof) {
                break;
            }
            stmts.push(self.parse_stmt());
            self.skip_newlines();
        }
        self.expect(Token::Dedent);
        stmts
    }

    // `match` (v1.x) desugars entirely to Stmt::If at parse time -- same
    // "erase via AST-to-AST pass" philosophy as C-style enum desugaring
    // above, just done inline here instead of a separate pass, since the
    // target (If) already exists and needs zero changes to the
    // checker/interpreter/codegen. Each arm becomes `scrutinee == pattern`;
    // `_` becomes the else branch.
    //
    // Honest v1.x scope (named gaps, not silent ones):
    //   - Patterns are int/bool literals or bare idents (e.g. enum consts)
    //     only -- no destructuring, no struct/array patterns, no guards,
    //     no or-patterns (`a | b`).
    //   - String literal patterns are rejected with a clear parse error:
    //     `==` on `str` is still POINTER equality in the C codegen backend
    //     (a separate, pre-existing gap -- see codegen_c.rs's `binop`,
    //     which emits plain C `==` for every type), so a string pattern
    //     would silently miscompile there. Flagged as a named follow-up,
    //     not fixed as part of this feature.
    //   - The scrutinee must be a plain identifier or field access (not a
    //     general expression) because it's cloned into every arm's
    //     condition and re-evaluated per arm tested -- a call expression
    //     would silently run once per arm. Fails loudly instead at parse
    //     time if the scrutinee isn't one of those two safe shapes.
    fn parse_match_desugared(&mut self) -> Stmt {
        self.expect(Token::Match);
        let scrutinee = self.parse_expr();
        if !matches!(scrutinee, Expr::Ident(_) | Expr::Field(..)) {
            panic!(
                "Kernel parser: `match` scrutinee must be a plain variable or field access in v1.x \
                 (it's re-evaluated once per arm) -- bind it to a `let` first, found {:?}",
                scrutinee
            );
        }
        self.expect(Token::Colon);
        self.expect(Token::Newline);
        self.expect(Token::Indent);

        let mut arms = Vec::new();
        let mut else_body = None;
        loop {
            self.skip_newlines();
            if self.check(&Token::Dedent) || self.check(&Token::Eof) {
                break;
            }
            let is_wildcard = matches!(self.cur(), Token::Ident(n) if n == "_");
            if is_wildcard {
                self.advance();
                self.expect(Token::Colon);
                else_body = Some(self.parse_block());
            } else {
                let pattern = self.parse_match_pattern();
                self.expect(Token::Colon);
                let body = self.parse_block();
                let cond = Expr::Binary(Box::new(scrutinee.clone()), BinOp::Eq, Box::new(pattern));
                arms.push((cond, body));
            }
            self.skip_newlines();
        }
        self.expect(Token::Dedent);
        Stmt::If { arms, else_body }
    }

    fn parse_match_pattern(&mut self) -> Expr {
        match self.cur().clone() {
            Token::Int(n) => {
                self.advance();
                Expr::Int(n)
            }
            Token::Minus => {
                self.advance();
                match self.advance() {
                    Token::Int(n) => Expr::Int(-n),
                    other => panic!("Kernel parser: expected an int literal after `-` in a match pattern, found {:?}", other),
                }
            }
            Token::True => {
                self.advance();
                Expr::Bool(true)
            }
            Token::False => {
                self.advance();
                Expr::Bool(false)
            }
            Token::Ident(name) => {
                self.advance();
                Expr::Ident(name)
            }
            Token::Str(_) => panic!(
                "Kernel parser: string literal patterns in `match` aren't supported yet -- \
                 see parse_match_desugared's doc comment (C codegen's `==` is pointer-equality \
                 for `str`); use an `if`/`elif` chain with string comparisons directly for now."
            ),
            other => panic!(
                "Kernel parser: unsupported `match` pattern {:?} -- v1.x supports int/bool \
                 literals and bare identifiers (e.g. enum constants) only",
                other
            ),
        }
    }

    fn parse_stmt(&mut self) -> Stmt {
        match self.cur().clone() {
            Token::Let => {
                self.advance();
                let mutable = self.eat(&Token::Mut);
                let name = self.ident();
                let ty = if self.eat(&Token::Colon) {
                    Some(self.parse_type())
                } else {
                    None
                };
                self.expect(Token::Assign);
                // A closure literal (`|params| -> ret:` + an indented
                // block) is ONLY ever valid as the whole RHS of a `let` --
                // see ast.rs's Expr::Closure doc comment for why -- so it's
                // special-cased here rather than reachable from the
                // general parse_expr()/parse_primary() chain.
                let value = if self.check(&Token::Pipe) {
                    self.parse_closure_literal()
                } else {
                    self.parse_expr()
                };
                Stmt::Let { name, mutable, ty, value }
            }
            Token::If => {
                self.advance();
                let mut arms = Vec::new();
                let cond = self.parse_expr();
                self.expect(Token::Colon);
                let body = self.parse_block();
                arms.push((cond, body));
                loop {
                    if self.eat(&Token::Elif) {
                        let c = self.parse_expr();
                        self.expect(Token::Colon);
                        let b = self.parse_block();
                        arms.push((c, b));
                    } else {
                        break;
                    }
                }
                let else_body = if self.eat(&Token::Else) {
                    self.expect(Token::Colon);
                    Some(self.parse_block())
                } else {
                    None
                };
                Stmt::If { arms, else_body }
            }
            Token::While => {
                self.advance();
                let cond = self.parse_expr();
                self.expect(Token::Colon);
                let body = self.parse_block();
                Stmt::While { cond, body }
            }
            Token::For => {
                self.advance();
                let var = self.ident();
                self.expect(Token::In);
                let iter = self.parse_expr();
                self.expect(Token::Colon);
                let body = self.parse_block();
                Stmt::For { var, iter, body }
            }
            Token::Return => {
                self.advance();
                if self.check(&Token::Newline) || self.check(&Token::Dedent) || self.check(&Token::Eof) {
                    Stmt::Return(None)
                } else {
                    Stmt::Return(Some(self.parse_expr()))
                }
            }
            Token::Unsafe => {
                self.advance();
                self.expect(Token::Colon);
                Stmt::Unsafe(self.parse_block())
            }
            Token::Match => self.parse_match_desugared(),
            _ => {
                // assignment or expression statement; parse an expr first
                let expr = self.parse_expr();
                match self.cur().clone() {
                    Token::Assign | Token::PlusEq | Token::MinusEq => {
                        let op = match self.advance() {
                            Token::Assign => AssignOp::Set,
                            Token::PlusEq => AssignOp::Add,
                            Token::MinusEq => AssignOp::Sub,
                            _ => unreachable!(),
                        };
                        let value = self.parse_expr();
                        match expr {
                            Expr::Ident(name) => Stmt::Assign { name, op, value },
                            Expr::Field(obj, field) => {
                                if op != AssignOp::Set {
                                    panic!("Kernel parser: compound assignment on fields not supported in v1");
                                }
                                Stmt::FieldAssign { object: *obj, field, value }
                            }
                            Expr::Index(array, index) => {
                                if op != AssignOp::Set {
                                    panic!("Kernel parser: compound assignment on array indices not supported in v1.x");
                                }
                                Stmt::IndexAssign { array: *array, index: *index, value }
                            }
                            other => panic!("Kernel parser: invalid assignment target {:?}", other),
                        }
                    }
                    _ => Stmt::Expr(expr),
                }
            }
        }
    }

    // ---- expressions, lowest to highest precedence ----

    fn parse_expr(&mut self) -> Expr {
        self.parse_range()
    }

    fn parse_range(&mut self) -> Expr {
        let left = self.parse_or();
        if self.eat(&Token::DotDot) {
            let right = self.parse_or();
            Expr::Range(Box::new(left), Box::new(right))
        } else {
            left
        }
    }

    fn parse_or(&mut self) -> Expr {
        let mut left = self.parse_and();
        while self.eat(&Token::PipePipe) {
            let right = self.parse_and();
            left = Expr::Binary(Box::new(left), BinOp::Or, Box::new(right));
        }
        left
    }

    fn parse_and(&mut self) -> Expr {
        let mut left = self.parse_equality();
        while self.eat(&Token::AmpAmp) {
            let right = self.parse_equality();
            left = Expr::Binary(Box::new(left), BinOp::And, Box::new(right));
        }
        left
    }

    fn parse_equality(&mut self) -> Expr {
        let mut left = self.parse_comparison();
        loop {
            let op = match self.cur() {
                Token::EqEq => BinOp::Eq,
                Token::NotEq => BinOp::NotEq,
                _ => break,
            };
            self.advance();
            let right = self.parse_comparison();
            left = Expr::Binary(Box::new(left), op, Box::new(right));
        }
        left
    }

    fn parse_comparison(&mut self) -> Expr {
        let mut left = self.parse_additive();
        loop {
            let op = match self.cur() {
                Token::Lt => BinOp::Lt,
                Token::Gt => BinOp::Gt,
                Token::LtEq => BinOp::LtEq,
                Token::GtEq => BinOp::GtEq,
                _ => break,
            };
            self.advance();
            let right = self.parse_additive();
            left = Expr::Binary(Box::new(left), op, Box::new(right));
        }
        left
    }

    fn parse_additive(&mut self) -> Expr {
        let mut left = self.parse_multiplicative();
        loop {
            let op = match self.cur() {
                Token::Plus => BinOp::Add,
                Token::Minus => BinOp::Sub,
                _ => break,
            };
            self.advance();
            let right = self.parse_multiplicative();
            left = Expr::Binary(Box::new(left), op, Box::new(right));
        }
        left
    }

    fn parse_multiplicative(&mut self) -> Expr {
        let mut left = self.parse_unary();
        loop {
            let op = match self.cur() {
                Token::Star => BinOp::Mul,
                Token::Slash => BinOp::Div,
                Token::Percent => BinOp::Mod,
                _ => break,
            };
            self.advance();
            let right = self.parse_unary();
            left = Expr::Binary(Box::new(left), op, Box::new(right));
        }
        left
    }

    fn parse_unary(&mut self) -> Expr {
        match self.cur() {
            Token::Minus => {
                self.advance();
                Expr::Unary(UnaryOp::Neg, Box::new(self.parse_unary()))
            }
            Token::Bang => {
                self.advance();
                Expr::Unary(UnaryOp::Not, Box::new(self.parse_unary()))
            }
            Token::Amp => {
                self.advance();
                let exclusive = self.eat(&Token::Mut);
                Expr::Borrow(Box::new(self.parse_unary()), exclusive)
            }
            _ => self.parse_postfix(),
        }
    }

    fn parse_postfix(&mut self) -> Expr {
        let mut expr = self.parse_primary();
        loop {
            if self.eat(&Token::Dot) {
                let field = self.ident();
                expr = Expr::Field(Box::new(expr), field);
            } else if self.eat(&Token::LBracket) {
                let index = self.parse_expr();
                self.expect(Token::RBracket);
                expr = Expr::Index(Box::new(expr), Box::new(index));
            } else {
                break;
            }
        }
        expr
    }

    fn parse_primary(&mut self) -> Expr {
        match self.advance() {
            Token::Int(n) => Expr::Int(n),
            Token::Float(f) => Expr::Float(f),
            Token::Str(s) => Expr::Str(s),
            Token::True => Expr::Bool(true),
            Token::False => Expr::Bool(false),
            Token::LParen => {
                let e = self.parse_expr();
                self.expect(Token::RParen);
                e
            }
            Token::LBracket => {
                // Array literal `[e1, e2, ...]` -- see ast.rs's Expr::ArrayLit
                // doc comment for v1.x's fixed-size-only scope.
                let mut elems = Vec::new();
                while !self.check(&Token::RBracket) {
                    elems.push(self.parse_expr());
                    if !self.eat(&Token::Comma) {
                        break;
                    }
                }
                self.expect(Token::RBracket);
                Expr::ArrayLit(elems)
            }
            Token::Ident(name) => {
                if self.check(&Token::LParen) {
                    self.advance();
                    let mut args = Vec::new();
                    while !self.check(&Token::RParen) {
                        // lookahead: `ident :` means a named arg (struct field)
                        if let Token::Ident(argname) = self.cur().clone() {
                            if self.tokens.get(self.pos + 1).map(|t| &t.0) == Some(&Token::Colon) {
                                self.advance(); // ident
                                self.advance(); // colon
                                let v = self.parse_expr();
                                args.push(CallArg::Named(argname, v));
                                if !self.eat(&Token::Comma) {
                                    break;
                                }
                                continue;
                            }
                        }
                        let v = self.parse_expr();
                        args.push(CallArg::Positional(v));
                        if !self.eat(&Token::Comma) {
                            break;
                        }
                    }
                    self.expect(Token::RParen);
                    Expr::Call { name, args }
                } else {
                    Expr::Ident(name)
                }
            }
            other => panic!("Kernel parser: unexpected token in expression: {:?}", other),
        }
    }
}
