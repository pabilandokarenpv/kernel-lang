mod ast;
mod checker;
mod codegen_c;
mod interpreter;
mod lambda_lift;
mod lexer;
mod monomorphize;
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
        Some("trace") => {
            let path = args.get(2).map(|s| s.as_str()).unwrap_or("examples/hello.kn");
            cmd_trace(path);
        }
        Some(p) => cmd_run(p),
        None => cmd_run("examples/hello.kn"),
    }
}

fn load_checked(path: &str) -> ast::Program {
    let src = fs::read_to_string(path).unwrap_or_else(|e| panic!("could not read {}: {}", path, e));
    let tokens = Lexer::new(&src).tokenize();
    let program = Parser::new(tokens).parse_program();
    // Generics (v1.x) are fully erased here, before the checker ever runs:
    // every generic fn/struct is replaced by one concrete, mangled copy per
    // distinct type it's actually instantiated with (see monomorphize.rs).
    let program = monomorphize::monomorphize(program);
    // Closures (v1.x) are fully erased here too, right after generics --
    // every closure becomes a real top-level function (its captures as
    // extra leading params) and every call site is rewritten; see
    // lambda_lift.rs for the full scope (non-escaping, primitive captures
    // only). Must run before the checker for the same reason monomorphize
    // does: nothing downstream should ever see a closure literal.
    let program = lambda_lift::lambda_lift(program);
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
    for line in &interp.output {
        println!("{}", line);
    }
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out
}

fn json_str_array(items: &[String]) -> String {
    let parts: Vec<String> = items.iter().map(|s| format!("\"{}\"", json_escape(s))).collect();
    format!("[{}]", parts.join(","))
}

fn cmd_trace(path: &str) {
    let program = load_checked(path);
    let mut interp = Interpreter::new();
    interp.load(&program);
    let final_value = interp.run_main();
    println!(
        "{{\"output\":{},\"trace\":{},\"final_value\":\"{}\"}}",
        json_str_array(&interp.output),
        json_str_array(&interp.trace),
        json_escape(&final_value.to_string())
    );
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
