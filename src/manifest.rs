// Kernel (.kn) project manifest -- v1.x "package manager skeleton"
// (Option C, final slice). Deliberately minimal and named as a skeleton,
// not a real package manager: there is no registry, no remote
// dependencies, no version resolution. What this DOES establish: a
// standard place (`kernel.toml`) and format for a project to name itself
// and its entry point, which `kernel run`/`kernel build` read when no
// explicit file path is given, plus `kernel init` to scaffold one --
// exactly the two integration points a real future package manager would
// need to build on (a manifest file, and commands that read it), without
// pretending dependency resolution is solved.
//
// Format: flat `key = "value"` lines, NOT full TOML (no `[sections]`,
// no nested tables, no arrays) -- intentionally the smallest useful
// subset, hand-parsed with no dependency, matching this project's
// no-external-crates identity (see ARD-004). Blank lines and lines
// starting with `#` are ignored.

use std::fs;
use std::path::Path;

pub struct Manifest {
    pub name: String,
    pub entry: String,
}

// Looks for `kernel.toml` in the current directory; returns None if it
// doesn't exist (callers fall back to their own default, e.g.
// `examples/hello.kn`, exactly like before this feature existed).
pub fn load_manifest() -> Option<Manifest> {
    let path = Path::new("kernel.toml");
    if !path.exists() {
        return None;
    }
    let src = fs::read_to_string(path).unwrap_or_else(|e| panic!("Kernel: could not read kernel.toml: {}", e));
    let mut name = None;
    let mut entry = None;
    for line in src.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            panic!("Kernel: malformed line in kernel.toml (expected `key = \"value\"`): {}", line);
        };
        let key = key.trim();
        let value = value.trim().trim_matches('"');
        match key {
            "name" => name = Some(value.to_string()),
            "entry" => entry = Some(value.to_string()),
            other => panic!("Kernel: unknown kernel.toml key `{}` (only `name` and `entry` are supported in this skeleton)", other),
        }
    }
    Some(Manifest {
        name: name.unwrap_or_else(|| "unnamed".to_string()),
        entry: entry.unwrap_or_else(|| "main.kn".to_string()),
    })
}

// `kernel init [name]` -- scaffolds kernel.toml + a hello-world entry file
// in the CURRENT directory. Refuses to overwrite an existing manifest
// (fails loudly rather than silently clobbering a real project).
pub fn cmd_init(name: &str) {
    let manifest_path = Path::new("kernel.toml");
    if manifest_path.exists() {
        panic!("Kernel: kernel.toml already exists in this directory -- refusing to overwrite it");
    }
    let manifest_src = format!("name = \"{}\"\nentry = \"main.kn\"\n", name);
    fs::write(manifest_path, manifest_src).unwrap_or_else(|e| panic!("Kernel: could not write kernel.toml: {}", e));

    let entry_path = Path::new("main.kn");
    if !entry_path.exists() {
        let entry_src = "fn main():\n    print(\"Hello from Kernel! (.kn)\")\n";
        fs::write(entry_path, entry_src).unwrap_or_else(|e| panic!("Kernel: could not write main.kn: {}", e));
    }

    println!("Initialized Kernel project `{}` (kernel.toml + main.kn)", name);
    println!("Run it with: kernel run");
}
