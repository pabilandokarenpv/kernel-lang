// Kernel (.kn) lexer — v1
// Indentation-sensitive like Python: tracks an indent stack and emits
// explicit Indent/Dedent/Newline tokens so the parser never has to look
// at whitespace itself.

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    // literals / identifiers
    Ident(String),
    Int(i64),
    Float(f64),
    Str(String),

    // keywords
    Fn,
    Let,
    Mut,
    If,
    Elif,
    Else,
    For,
    While,
    In,
    Return,
    Struct,
    Enum,
    Impl,
    Trait,
    Match,
    Unsafe,
    Static,
    Const,
    Import,
    As,
    True,
    False,

    // symbols / operators
    Colon,      // :
    Comma,      // ,
    Dot,        // .
    DotDot,     // ..
    Arrow,      // ->
    Question,   // ?
    Amp,        // &
    AmpAmp,     // &&
    PipePipe,   // ||
    Bang,       // !
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Assign,     // =
    PlusEq,     // +=
    MinusEq,    // -=
    EqEq,       // ==
    NotEq,      // !=
    Lt,
    Gt,
    LtEq,
    GtEq,
    LParen,
    RParen,
    LBracket,
    RBracket,
    LAngle,     // < used generically for generics is reused from Lt/Gt
    Semicolon,  // ; used only inside array types/literals: [int;5]
    Pipe,       // | used only for closure param lists: |x: int| -> int:

    // structure
    Newline,
    Indent,
    Dedent,
    Eof,
}

pub struct Lexer {
    chars: Vec<char>,
    pos: usize,
    indent_stack: Vec<usize>,
    pending: Vec<Token>,
    at_line_start: bool,
    line: usize, // 1-indexed, tracked only so tokenize() can attach a
    col: usize,  // (line, col) to every token -- needed for real tooling
    // (the LSP server, better panic messages) that this project had no
    // position info for at all until now; every other lexer field/method
    // is unchanged, this only touches advance() and tokenize()'s signature.
}

impl Lexer {
    pub fn new(src: &str) -> Self {
        Lexer {
            chars: src.chars().collect(),
            pos: 0,
            indent_stack: vec![0],
            pending: Vec::new(),
            at_line_start: true,
            line: 1,
            col: 1,
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.pos + offset).copied()
    }

    fn advance(&mut self) -> Option<char> {
        let c = self.peek();
        if let Some(ch) = c {
            self.pos += 1;
            if ch == '\n' {
                self.line += 1;
                self.col = 1;
            } else {
                self.col += 1;
            }
        }
        c
    }

    // Returns (token, line, col) triples instead of bare tokens -- parser.rs
    // stores the richer tuple but exposes the same `&Token`/`Token` surface
    // via cur()/advance() as before (plus a new cur_pos()), so this doesn't
    // ripple through every existing `match self.cur()` call site.
    pub fn tokenize(mut self) -> Vec<(Token, usize, usize)> {
        let mut tokens = Vec::new();

        loop {
            if !self.pending.is_empty() {
                let pos = (self.line, self.col);
                tokens.push((self.pending.remove(0), pos.0, pos.1));
                continue;
            }

            if self.at_line_start {
                let pos = (self.line, self.col);
                if let Some(tok) = self.handle_indentation() {
                    tokens.push((tok, pos.0, pos.1));
                    continue;
                }
            }

            let pos = (self.line, self.col);
            match self.next_token() {
                Some(Token::Eof) | None => break,
                Some(tok) => tokens.push((tok, pos.0, pos.1)),
            }
        }

        // close out any remaining indents
        while self.indent_stack.len() > 1 {
            self.indent_stack.pop();
            tokens.push((Token::Dedent, self.line, self.col));
        }
        tokens.push((Token::Eof, self.line, self.col));
        tokens
    }

    /// Consumes leading whitespace on a fresh line and resolves it into
    /// Indent/Dedent tokens. Returns None (and clears at_line_start) once
    /// a non-blank, non-comment line of actual content is found.
    fn handle_indentation(&mut self) -> Option<Token> {
        loop {
            let start = self.pos;
            let mut col = 0;
            while let Some(c) = self.peek() {
                if c == ' ' {
                    col += 1;
                    self.advance();
                } else {
                    break;
                }
            }

            match self.peek() {
                None => {
                    self.at_line_start = false;
                    return None;
                }
                Some('\n') => {
                    // blank line, skip entirely, don't affect indent stack
                    self.advance();
                    continue;
                }
                Some('#') => {
                    // comment-only line, skip to end of line
                    while let Some(c) = self.peek() {
                        if c == '\n' {
                            break;
                        }
                        self.advance();
                    }
                    continue;
                }
                _ => {
                    let _ = start;
                    self.at_line_start = false;
                    let current = *self.indent_stack.last().unwrap();
                    if col > current {
                        self.indent_stack.push(col);
                        return Some(Token::Indent);
                    } else if col < current {
                        // may need multiple dedents, queue the rest
                        while *self.indent_stack.last().unwrap() > col {
                            self.indent_stack.pop();
                            self.pending.push(Token::Dedent);
                        }
                        return Some(self.pending.remove(0));
                    } else {
                        return None;
                    }
                }
            }
        }
    }

    fn next_token(&mut self) -> Option<Token> {
        // skip inline spaces (not newlines)
        while self.peek() == Some(' ') || self.peek() == Some('\t') {
            self.advance();
        }

        let c = match self.peek() {
            Some(c) => c,
            None => return Some(Token::Eof),
        };

        if c == '#' {
            while let Some(c) = self.peek() {
                if c == '\n' {
                    break;
                }
                self.advance();
            }
            return self.next_token();
        }

        if c == '\n' {
            self.advance();
            self.at_line_start = true;
            return Some(Token::Newline);
        }

        if c.is_ascii_digit() {
            let mut s = String::new();
            while let Some(c) = self.peek() {
                if c.is_ascii_digit() {
                    s.push(c);
                    self.advance();
                } else {
                    break;
                }
            }
            // a '.' followed by a digit makes this a float literal; a '.'
            // followed by another '.' is the range operator (`1..5`) and
            // must be left alone so next_token's two-char-operator check
            // still sees it.
            if self.peek() == Some('.') && self.peek_at(1).map(|c| c.is_ascii_digit()).unwrap_or(false) {
                s.push('.');
                self.advance();
                while let Some(c) = self.peek() {
                    if c.is_ascii_digit() {
                        s.push(c);
                        self.advance();
                    } else {
                        break;
                    }
                }
                return Some(Token::Float(s.parse().unwrap_or(0.0)));
            }
            return Some(Token::Int(s.parse().unwrap_or(0)));
        }

        if c.is_alphabetic() || c == '_' {
            let mut s = String::new();
            while let Some(c) = self.peek() {
                if c.is_alphanumeric() || c == '_' {
                    s.push(c);
                    self.advance();
                } else {
                    break;
                }
            }
            return Some(match s.as_str() {
                "fn" => Token::Fn,
                "let" => Token::Let,
                "mut" => Token::Mut,
                "if" => Token::If,
                "elif" => Token::Elif,
                "else" => Token::Else,
                "for" => Token::For,
                "while" => Token::While,
                "in" => Token::In,
                "return" => Token::Return,
                "struct" => Token::Struct,
                "enum" => Token::Enum,
                "impl" => Token::Impl,
                "trait" => Token::Trait,
                "match" => Token::Match,
                "unsafe" => Token::Unsafe,
                "static" => Token::Static,
                "const" => Token::Const,
                "import" => Token::Import,
                "as" => Token::As,
                "true" => Token::True,
                "false" => Token::False,
                _ => Token::Ident(s),
            });
        }

        if c == '"' {
            self.advance();
            let mut s = String::new();
            while let Some(c) = self.peek() {
                if c == '"' {
                    self.advance();
                    break;
                }
                s.push(c);
                self.advance();
            }
            return Some(Token::Str(s));
        }

        // two-character operators first
        let two: Option<Token> = match (c, self.peek_at(1)) {
            ('-', Some('>')) => Some(Token::Arrow),
            ('=', Some('=')) => Some(Token::EqEq),
            ('!', Some('=')) => Some(Token::NotEq),
            ('<', Some('=')) => Some(Token::LtEq),
            ('>', Some('=')) => Some(Token::GtEq),
            ('+', Some('=')) => Some(Token::PlusEq),
            ('-', Some('=')) => Some(Token::MinusEq),
            ('&', Some('&')) => Some(Token::AmpAmp),
            ('|', Some('|')) => Some(Token::PipePipe),
            ('.', Some('.')) => Some(Token::DotDot),
            _ => None,
        };
        if let Some(tok) = two {
            self.advance();
            self.advance();
            return Some(tok);
        }

        self.advance();
        Some(match c {
            ':' => Token::Colon,
            ',' => Token::Comma,
            '.' => Token::Dot,
            '?' => Token::Question,
            '&' => Token::Amp,
            '!' => Token::Bang,
            '+' => Token::Plus,
            '-' => Token::Minus,
            '*' => Token::Star,
            '/' => Token::Slash,
            '%' => Token::Percent,
            '=' => Token::Assign,
            '<' => Token::Lt,
            '>' => Token::Gt,
            '(' => Token::LParen,
            ')' => Token::RParen,
            '[' => Token::LBracket,
            ']' => Token::RBracket,
            ';' => Token::Semicolon,
            '|' => Token::Pipe,
            other => panic!("Kernel lexer: unexpected character '{}' at line {}, column {}", other, self.line, self.col),
        })
    }
}
