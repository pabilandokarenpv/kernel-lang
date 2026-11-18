// Kernel (.kn) parser — v1 core subset.
// Recursive descent, operator precedence climbing for expressions.

use crate::ast::*;
use crate::lexer::Token;

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Parser { tokens, pos: 0 }
    }

    fn cur(&self) -> &Token {
        self.tokens.get(self.pos).unwrap_or(&Token::Eof)
    }

    fn advance(&mut self) -> Token {
        let t = self.tokens.get(self.pos).cloned().unwrap_or(Token::Eof);
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
            panic!("Kernel parser: expected {:?}, found {:?} at token {}", t, self.cur(), self.pos);
        }
    }

    fn skip_newlines(&mut self) {
        while self.check(&Token::Newline) {
            self.advance();
        }
    }

    fn ident(&mut self) -> String {
        match self.advance() {
            Token::Ident(s) => s,
            other => panic!("Kernel parser: expected identifier, found {:?}", other),
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
                    s.push('[');
                    self.advance();
                }
                Token::RBracket => {
                    s.push(']');
                    self.advance();
                    if depth == 0 {
                        break;
                    }
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
            items.push(self.parse_item());
            self.skip_newlines();
        }
        Program { items }
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
            other => panic!("Kernel parser: unexpected top-level token {:?}", other),
        }
    }

    fn parse_fn_rest(&mut self, is_unsafe: bool) -> FnDecl {
        let name = self.ident();
        self.expect(Token::LParen);
        let mut params = Vec::new();
        while !self.check(&Token::RParen) {
            let mutable = self.eat(&Token::Mut);
            let borrowed = self.eat(&Token::Amp);
            let pname = self.ident();
            self.expect(Token::Colon);
            let ty = self.parse_type();
            params.push(Param { name: pname, ty, borrowed, mutable });
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
        FnDecl { name, is_unsafe, params, return_ty, body }
    }

    fn parse_struct(&mut self) -> StructDecl {
        let name = self.ident();
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
        StructDecl { name, fields }
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
                let value = self.parse_expr();
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
                Expr::Borrow(Box::new(self.parse_unary()))
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
            } else {
                break;
            }
        }
        expr
    }

    fn parse_primary(&mut self) -> Expr {
        match self.advance() {
            Token::Int(n) => Expr::Int(n),
            Token::Str(s) => Expr::Str(s),
            Token::True => Expr::Bool(true),
            Token::False => Expr::Bool(false),
            Token::LParen => {
                let e = self.parse_expr();
                self.expect(Token::RParen);
                e
            }
            Token::Ident(name) => {
                if self.check(&Token::LParen) {
                    self.advance();
                    let mut args = Vec::new();
                    while !self.check(&Token::RParen) {
                        // lookahead: `ident :` means a named arg (struct field)
                        if let Token::Ident(argname) = self.cur().clone() {
                            if self.tokens.get(self.pos + 1) == Some(&Token::Colon) {
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
