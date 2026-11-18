mod lexer;

use lexer::Lexer;
use std::env;
use std::fs;

fn main() {
    let args: Vec<String> = env::args().collect();
    let path = args.get(1).map(|s| s.as_str()).unwrap_or("examples/hello.kn");

    let src = fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("could not read {}: {}", path, e));

    let tokens = Lexer::new(&src).tokenize();

    println!("--- Kernel lexer output for {} ---", path);
    println!("{} tokens produced\n", tokens.len());
    for tok in &tokens {
        println!("{:?}", tok);
    }
}
