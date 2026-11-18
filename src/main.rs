mod ast;
mod checker;
mod codegen_c;
mod interpreter;
mod lexer;
mod parser;

use checker::Checker;
use codegen_c::Codegen;
use interpreter::Interpreter;
use lexer::Lexer;
use parser::Parser;
use std::env;
use std::fs;
use std::process::{self, Command};

const RUNTIME_DIR: &str = env!("CARGO_MANIFEST_DIR");

fn main() {
    let args: Vec<String> = env::args().collect();

    let subcmd = args.get(1).map(|s| s.as_str());
    match subcmd {
        Some("tokens") => {
            let path = args.get(2).map(|s| s.as_str()).unwrap_or("examples/hello.kn");
            cmd_tokens(path);
        }
        Some("run") => {
            let path = args.get(2).map(|s| s.as_str()).unwrap_or("examples/hello.kn");
            cmd_run(path);
        }
        Some("build") => {
            let path = args.get(2).map(|s| s.as_str()).unwrap_or("examples/hello.kn");
            let out = args.get(3).map(|s| s.as_str()).unwrap_or("kernel.bin");
            cmd_build(path, out);
        }
        Some(p) => cmd_run(p),
        None => cmd_run("examples/hello.kn"),
    }
}

fn load_checked(path: &str) -> ast::Program {
    let src = fs::read_to_string(path).unwrap_or_else(|e| panic!("could not read {}: {}", path, e));
    let tokens = Lexer::new(&src).tokenize();
    let program = Parser::new(tokens).parse_program();
    let errors = Checker::new(&program).check(&program);
    if !errors.is_empty() {
        eprintln!("error: Kernel safety check failed in {}:\n", path);
        for e in &errors {
            eprintln!("  {}", e);
        }
        process::exit(1);
    }
    program
}

fn cmd_tokens(path: &str) {
    let src = fs::read_to_string(path).unwrap_or_else(|e| panic!("could not read {}: {}", path, e));
    let tokens = Lexer::new(&src).tokenize();
    println!("--- Kernel lexer output for {} ---", path);
    println!("{} tokens produced\n", tokens.len());
    for tok in &tokens {
        println!("{:?}", tok);
    }
}

fn cmd_run(path: &str) {
    let program = load_checked(path);
    let mut interp = Interpreter::new();
    interp.load(&program);
    interp.run_main();
}

fn cmd_build(path: &str, out: &str) {
    let program = load_checked(path);
    let c_src = Codegen::new(&program).generate(&program);

    let build_dir = "/tmp/kernel_build";
    fs::create_dir_all(build_dir).unwrap();
    let c_path = format!("{}/kmain.c", build_dir);
    fs::write(&c_path, &c_src).unwrap();
    println!("generated C: {}", c_path);

    let runtime_c = format!("{}/kernel_runtime/runtime.c", RUNTIME_DIR);
    let boot_s = format!("{}/kernel_runtime/boot.s", RUNTIME_DIR);
    let link_ld = format!("{}/kernel_runtime/link.ld", RUNTIME_DIR);

    let kmain_o = format!("{}/kmain.o", build_dir);
    let runtime_o = format!("{}/runtime.o", build_dir);
    let boot_o = format!("{}/boot.o", build_dir);

    run(Command::new("gcc")
        .args(["-m32", "-ffreestanding", "-fno-pic", "-fno-stack-protector", "-O1", "-c"])
        .arg(&c_path)
        .arg("-o")
        .arg(&kmain_o));

    run(Command::new("gcc")
        .args(["-m32", "-ffreestanding", "-fno-pic", "-fno-stack-protector", "-O1", "-c"])
        .arg(&runtime_c)
        .arg("-o")
        .arg(&runtime_o));

    run(Command::new("as").args(["--32"]).arg(&boot_s).arg("-o").arg(&boot_o));

    run(Command::new("ld")
        .args(["-m", "elf_i386", "-T"])
        .arg(&link_ld)
        .arg("-nostdlib")
        .arg("-o")
        .arg(out)
        .arg(&boot_o)
        .arg(&kmain_o)
        .arg(&runtime_o));

    println!("built: {}", out);
}

fn run(cmd: &mut Command) {
    let status = cmd.status().unwrap_or_else(|e| panic!("failed to run {:?}: {}", cmd, e));
    if !status.success() {
        eprintln!("command failed: {:?}", cmd);
        process::exit(1);
    }
}
