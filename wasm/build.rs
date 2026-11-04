use std::{
    env, fs,
    hash::{Hash, Hasher},
    path::Path,
    process::Command,
};

fn main() {
    let lock = Path::new("../Cargo.lock");
    println!("cargo:rerun-if-changed={}", lock.display());
    println!("cargo:rerun-if-changed=src/compiler_engine.rs");
    println!("cargo:rerun-if-changed=src/compiler_protocol.rs");
    let text = fs::read_to_string(lock).expect("compiler identity requires Cargo.lock");
    let package = text
        .split("[[package]]")
        .find(|block| block.lines().any(|line| line == "name = \"wasmtime\""))
        .expect("resolved Wasmtime package");
    let version = package
        .lines()
        .find_map(|line| {
            line.strip_prefix("version = \"")
                .and_then(|line| line.strip_suffix('"'))
        })
        .expect("Wasmtime version");
    assert_eq!(
        version, "36.0.19",
        "review the compiler trust boundary before changing Wasmtime"
    );
    let rustc = Command::new(env::var_os("RUSTC").expect("RUSTC"))
        .arg("-vV")
        .output()
        .expect("compiler build identity");
    assert!(rustc.status.success());
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hash);
    rustc.stdout.hash(&mut hash);
    env::var("TARGET").expect("TARGET").hash(&mut hash);
    fs::read("src/compiler_engine.rs").unwrap().hash(&mut hash);
    fs::read("src/compiler_protocol.rs")
        .unwrap()
        .hash(&mut hash);
    println!(
        "cargo:rustc-env=WASM_COMPILER_BUILD=wasmtime-{version}-{:016x}",
        hash.finish()
    );
}
