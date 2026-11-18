mod ast;
mod interpreter;
mod lexer;
mod parser;

use interpreter::Interpreter;
use lexer::Lexer;
use parser::Parser;
use std::env;
use std::fs;

fn main() {
    let args: Vec<String> = env::args().collect();

    // `kernel tokens <file>` -> debug-dump the lexer output
    // `kernel run <file>` or just `kernel <file>` -> interpret it (dev mode)
    let (mode, path) = match args.get(1).map(|s| s.as_str()) {
        Some("tokens") => ("tokens", args.get(2).map(|s| s.as_str()).unwrap_or("examples/hello.kn")),
        Some("run") => ("run", args.get(2).map(|s| s.as_str()).unwrap_or("examples/hello.kn")),
        Some(p) => ("run", p),
        None => ("run", "examples/hello.kn"),
    };

    let src = fs::read_to_string(path).unwrap_or_else(|e| panic!("could not read {}: {}", path, e));
    let tokens = Lexer::new(&src).tokenize();

    if mode == "tokens" {
        println!("--- Kernel lexer output for {} ---", path);
        println!("{} tokens produced\n", tokens.len());
        for tok in &tokens {
            println!("{:?}", tok);
        }
        return;
    }

    let program = Parser::new(tokens).parse_program();
    let mut interp = Interpreter::new();
    interp.load(&program);
    interp.run_main();
}
