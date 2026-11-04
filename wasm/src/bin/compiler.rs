//! A separate executable. Never import redis_module_wasm: its global allocator
//! requires Redis. Only engine/protocol/limit source is shared with the host.
#[path = "../compiler_engine.rs"]
mod compiler_engine;
#[path = "../compiler_limits.rs"]
mod compiler_limits;
#[path = "../compiler_protocol.rs"]
mod compiler_protocol;

use anyhow::{Context as _, ensure};
use compiler_protocol::{MAX_DIAGNOSTIC, REQUEST_SIZE, Response};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};

#[global_allocator]
static ALLOCATOR: std::alloc::System = std::alloc::System;

fn run() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    ensure!(
        args.len() == 2,
        "compiler expects address-space allowance and CPU seconds"
    );
    let (baseline, ceiling) = compiler_limits::apply(args[0].parse()?, args[1].parse()?)?;
    let engine = compiler_engine::engine()?;
    let fingerprint = compiler_engine::fingerprint(&engine);
    let mut input = std::io::stdin().lock();
    let mut request = [0; REQUEST_SIZE];
    input.read_exact(&mut request)?;
    let (nonce, len) = compiler_protocol::parse_request(&request, fingerprint)?;
    let mut wasm = vec![0; len];
    input.read_exact(&mut wasm)?;
    let mut extra = [0];
    ensure!(input.read(&mut extra)? == 0, "extra compiler input");
    let result = engine
        .precompile_component(&wasm)
        .context("component compilation failed");
    drop(wasm);
    let (failed, bytes) = match result {
        Ok(bytes) => {
            ensure!(
                bytes.len() <= compiler_protocol::MAX_ARTIFACT,
                "compiled artifact exceeds output bound"
            );
            (false, bytes)
        }
        Err(error) => {
            let mut message = format!("{error:#}").into_bytes();
            message.truncate(MAX_DIAGNOSTIC);
            (true, message)
        }
    };
    let response = Response {
        nonce,
        fingerprint,
        failed,
        len: bytes.len(),
        baseline,
        ceiling,
        cpu_ms: compiler_limits::cpu_milliseconds(),
        checksum: Sha256::digest(&bytes).into(),
    };
    let mut output = std::io::stdout().lock();
    output.write_all(&response.encode())?;
    output.write_all(&bytes)?;
    output.flush()?;
    ensure!(!failed, "compiler refused component");
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("wasm compiler: {error:#}");
        std::process::exit(1);
    }
}
