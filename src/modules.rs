// Kernel (.kn) module system -- v1.x (Option C). See ast.rs's
// Item::Import doc comment for the full scope decision (unqualified
// textual splice, no visibility, relative-to-importing-file resolution,
// cycle/duplicate-safe). This is the FIRST pass run (even before
// monomorphize/lambda_lift) since everything downstream needs to see one
// single, fully flattened program -- a generic function or a closure
// defined in an imported file is just as real as one defined in the
// entry file, and monomorphize.rs/lambda_lift.rs have no reason to know
// which file anything originally came from.
//
// Honest v1.x scope (named gaps, not silent ones):
//   - No visibility/qualified names: `import utils.math` pastes ALL of
//     that file's top-level items in unqualified. Two modules defining
//     the same name (or a module colliding with the entry file) is a
//     loud, clear error -- see check_for_duplicate_names -- never a
//     silent "last one wins" overwrite (which is what would otherwise
//     happen, since checker.rs/interpreter.rs/codegen_c.rs all build
//     their function/struct tables via plain HashMap::insert).
//   - No package manager / remote dependencies -- see RFC-002's note
//     that a package manager has nothing to manage until a project
//     actually spans multiple local files, which is exactly what this
//     pass makes possible for the first time.

use crate::ast::*;
use crate::lexer::Lexer;
use crate::parser::Parser;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

pub fn resolve_modules(entry_path: &Path, program: Program) -> Program {
    let mut visited: HashSet<PathBuf> = HashSet::new();
    if let Ok(canon) = entry_path.canonicalize() {
        visited.insert(canon);
    }
    let mut out = Vec::new();
    collect_items(entry_path, program.items, &mut visited, &mut out);
    check_for_duplicate_names(&out);
    Program { items: out }
}

fn collect_items(current_path: &Path, items: Vec<Item>, visited: &mut HashSet<PathBuf>, out: &mut Vec<Item>) {
    let dir = current_path.parent().unwrap_or_else(|| Path::new("."));
    for item in items {
        match item {
            Item::Import(module_path) => {
                let rel = module_path.replace('.', "/") + ".kn";
                let full_path = dir.join(&rel);
                let canon = full_path.canonicalize().unwrap_or_else(|err| {
                    panic!(
                        "Kernel: cannot resolve `import {}` -- looked for {} ({})",
                        module_path,
                        full_path.display(),
                        err
                    )
                });
                if !visited.insert(canon) {
                    // Already imported somewhere in this tree (either a
                    // genuine diamond import, or a cycle) -- skip silently,
                    // same policy as a C header guard: imported once,
                    // total, no matter how many places import it.
                    continue;
                }
                let src = fs::read_to_string(&full_path).unwrap_or_else(|err| {
                    panic!("Kernel: cannot read imported module `{}` at {}: {}", module_path, full_path.display(), err)
                });
                let tokens = Lexer::new(&src).tokenize();
                let imported = Parser::new(tokens).parse_program();
                // Recurse into the imported module's OWN imports before
                // appending its items, so dependency order in the merged
                // program roughly follows the import graph (not load-
                // bearing for correctness -- functions/structs are all
                // resolved by name in a flat table downstream, order never
                // matters there -- just tidier generated-C reading order).
                collect_items(&full_path, imported.items, visited, out);
            }
            other => out.push(other),
        }
    }
}

// Every later stage (checker.rs, interpreter.rs, codegen_c.rs) builds its
// function/struct/const tables via plain `HashMap::insert`, which
// silently lets a later definition overwrite an earlier one with the same
// name -- harmless for a single file (nothing else could collide), but a
// real correctness hazard now that multiple files' items get merged into
// one flat list. Catching it here, once, right after merging, is cheaper
// and clearer than teaching every downstream HashMap-builder to detect it
// independently.
fn check_for_duplicate_names(items: &[Item]) {
    let mut seen: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for item in items {
        let name = match item {
            Item::Function(f) => &f.name,
            Item::Struct(s) => &s.name,
            Item::Static { name, .. } | Item::Const { name, .. } => name,
            Item::Import(_) => continue, // already resolved away by this point
        };
        *seen.entry(name.as_str()).or_insert(0) += 1;
    }
    let dupes: Vec<&str> = seen.iter().filter(|(_, count)| **count > 1).map(|(name, _)| *name).collect();
    if !dupes.is_empty() {
        panic!(
            "Kernel: duplicate top-level definition(s) after resolving imports: {} -- \
             rename one of them (no qualified/namespaced imports in v1.x, see ast.rs's Item::Import doc comment)",
            dupes.join(", ")
        );
    }
}
