//! Turbo overrides from eink-qemu/turbo-args.sh, one argument list per
//! machine, embedded as JSON (session.rs). The script is the one source the
//! native launchers, the smoke tests and the browser runtime share.

use std::path::Path;
use std::process::Command;

fn main() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../turbo-args.sh");
    println!("cargo:rerun-if-changed={}", script.display());
    let mut machines = Vec::new();
    for machine in ["x3", "x4", "x4pro", "picco"] {
        // Windows hosts have Git's sh or bash on PATH in CI.
        let output = Command::new("sh")
            .arg(&script)
            .arg(machine)
            .output()
            .or_else(|_| Command::new("bash").arg(&script).arg(machine).output())
            .unwrap_or_else(|e| panic!("could not run sh {}: {e}", script.display()));
        if !output.status.success() {
            panic!("{} {machine} failed: {}", script.display(), String::from_utf8_lossy(&output.stderr));
        }
        let args: Vec<String> = String::from_utf8(output.stdout)
            .expect("turbo-args.sh printed non-UTF-8")
            .lines()
            .map(|line| format!("\"{}\"", line.replace('\\', "\\\\").replace('"', "\\\"")))
            .collect();
        machines.push(format!("\"{machine}\": [{}]", args.join(", ")));
    }
    let out = Path::new(&std::env::var("OUT_DIR").unwrap()).join("turbo-args.json");
    std::fs::write(out, format!("{{{}}}\n", machines.join(", "))).unwrap();
}
