#!/usr/bin/env python3
"""Compile corrected lifetime/API doc contracts without executing Redis code."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifacts", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    args.output.mkdir(parents=True, exist_ok=False)
    artifacts = [json.loads(line) for line in args.artifacts.read_text().splitlines()
                 if line.startswith("{")]
    libraries = [Path(p) for row in artifacts
                 if row.get("reason") == "compiler-artifact"
                 and row.get("target", {}).get("name") == "redis_module"
                 for p in row["filenames"] if p.endswith(".rlib")]
    if len(libraries) != 1:
        raise RuntimeError("Require exactly one SDK rlib from the Cargo artifact log")
    library = libraries[0]
    cases = []
    for source, expected_count in (("src/redismodule.rs", 10), ("src/args.rs", 4)):
        blocks = re.findall(r"/// ```(.*?)\n(.*?)/// ```", (root / source).read_text(), re.S)
        if len(blocks) != expected_count:
            raise RuntimeError(f"Review changed doc-contract inventory in {source}")
        for number, (flags, code) in enumerate(blocks):
            code = re.sub(r"^[ \t]*/// ?", "", code, flags=re.M)
            expected = flags.split(",")[1] if flags.startswith("compile_fail,") else None
            cases.append((source + ":" + str(number), code, expected))

    custom = """use redis_module::{NextArg, RedisError, RedisString};
pub struct CustomArgs;
impl NextArg for CustomArgs {
    fn next_arg(&mut self) -> Result<RedisString, RedisError> { Err(RedisError::WrongArity) }
    fn next_string(&mut self) -> Result<String, RedisError> { Err(RedisError::WrongArity) }
    fn next_i64(&mut self) -> Result<i64, RedisError> { Err(RedisError::WrongArity) }
    fn next_u64(&mut self) -> Result<u64, RedisError> { Err(RedisError::WrongArity) }
    fn next_f64(&mut self) -> Result<f64, RedisError> { Err(RedisError::WrongArity) }
    fn done(&mut self) -> Result<(), RedisError> { Ok(()) }
    // OLD_METHOD
}
"""
    cases.append(("custom-migrated", custom, None))
    cases.append(("custom-stale-method", custom.replace("// OLD_METHOD", "fn next_str<'a>(&mut self) -> Result<&'a str, RedisError> { Err(RedisError::WrongArity) }"), "E0407"))
    callback = """use redis_module::{Context, NotifyEvent};
use std::{ffi::CStr, os::raw::{c_char, c_int}};
fn handler(_: &Context, _: NotifyEvent, _: &str, _: &[u8]) {}
pub fn register(ctx: *mut redis_module::raw::RedisModuleCtx) -> c_int {
    redis_module::redis_event_handler!(ctx, NotifyEvent::GENERIC, handler);
    0
}
"""
    cases.append(("callback-local-view", callback, None))
    # Pinned rustc reports the macro's local-context borrow as E0716. A compiler
    # change must recheck the diagnostic; a different failure is not a pass.
    cases.append(("callback-requires-static", callback.replace("_: &[u8]", "_: &'static [u8]"), "E0716"))
    report = {"compiler": subprocess.check_output(["rustup", "run", "1.94.0", "rustc", "-Vv"], text=True),
              "library": str(library), "library_sha256": digest(library),
              "source_files": {p: digest(root / p) for p in ("src/redismodule.rs", "src/args.rs", "src/macros.rs")},
              "native_code_executed": False, "cases": []}
    for index, (label, code, expected) in enumerate(cases):
        source = args.output / f"case-{index}.rs"
        source.write_text(code)
        command = ["rustup", "run", "1.94.0", "rustc", "--edition=2021", "--crate-type=lib",
                   "--emit=metadata", "--error-format=json", "--extern", f"redis_module={library}",
                   "-L", "dependency=" + str(library.parent), "--out-dir", str(args.output), str(source)]
        result = subprocess.run(command, capture_output=True, text=True)
        (args.output / f"case-{index}.diagnostics.jsonl").write_text(result.stderr)
        messages = [json.loads(line) for line in result.stderr.splitlines() if line.startswith("{")]
        codes = [message["code"]["code"] for message in messages if message.get("code")]
        passed = (result.returncode != 0 and expected in codes) if expected else result.returncode == 0
        report["cases"].append({"label": label, "source_sha256": digest(source), "expected_error": expected,
                                "actual_exit": result.returncode, "observed_codes": codes,
                                "passed": passed, "command": command})
        (args.output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
        print(label, "passed" if passed else "FAILED", flush=True)
        if not passed:
            raise RuntimeError(f"Contract mismatch for {label}: {codes}, exit {result.returncode}")


if __name__ == "__main__":
    main()
