#!/usr/bin/env python3
"""Run the explicitly selected ordinary integration baseline on owned servers.

Python 3.11+, Cargo and libclang are required. Outputs must be a new directory;
each test gets a private process group and retained server artifacts. This is
deliberately not a claim that the whole inherited integration suite passes.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import signal
import subprocess
import sys
import time


ROOT = Path(__file__).resolve().parents[1]
FEATURES = "min-redis-compatibility-version-7-2,bindgen-runtime"
CASES = (
    "hello", "keys_pos", "helper_version", "command_name", "helper_info",
    "info_handler_multiple_sections", "string", "scan", "scan_key",
    "scan_key_for_each", "stream_reader", "call", "ctx_flags",
    "get_current_user", "verify_acl_on_user", "key_space_notifications",
    "context_mutex", "server_event", "configuration", "response",
    "call_blocking", "open_key_with_flags", "expire",
    "logging_message_text", "logging_io_message_text",
    "command_proc_macro", "redis_value_derive",
)
EXCLUDED = {
    "test_test_helper_err": "Uses the hello module for a nonexistent helper command; needs a meaningful replacement.",
    "test_key_space_notification_with_invalid_utf8_event_name": "Specialized event-input contract deferred from the ordinary baseline.",
    "test_call_dump": "Specialized binary CallReply conversion contract deferred to focused repair validation.",
    "test_call_binary_data": "Specialized binary CallReply conversion contract deferred to focused repair validation.",
    "test_defrag": "The selected macOS servers use libc; active defragmentation requires a capable allocator/server cell.",
    "test_set_acl_categories": "Not compiled at the selected Redis 7.2 API floor (requires 7.4/8.0).",
    "test_set_acl_categories_commands": "Not compiled at the selected Redis 7.2 API floor (requires 8.0).",
}


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def capture(argv):
    return subprocess.check_output(argv, cwd=ROOT, text=True, timeout=30).strip()


def cleanup_group(process):
    # Also reclaim children when a failing test has already exited.
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    process.wait(timeout=5)


def run(argv, env, output, label, timeout):
    path = output / "logs" / (label + ".log")
    path.parent.mkdir(exist_ok=True)
    started = time.monotonic()
    expired = False
    with path.open("w") as stream:
        process = subprocess.Popen(argv, cwd=ROOT, env=env, stdout=stream,
                                   stderr=subprocess.STDOUT, start_new_session=True)
        try:
            process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            expired = True
        finally:
            cleanup_group(process)
    return {"argv": list(map(str, argv)), "exit_code": process.returncode,
            "timeout": expired, "seconds": round(time.monotonic() - started, 3),
            "log": str(path.relative_to(output))}


def integration_executable(log):
    artifacts = set()
    for line in log.splitlines():
        if not line.startswith("{"):
            continue
        message = json.loads(line)
        if (message.get("reason") == "compiler-artifact"
                and message["target"]["name"] == "integration"
                and message.get("executable") and message["profile"]["test"]):
            artifacts.add(message["executable"])
    if len(artifacts) != 1:
        raise RuntimeError(f"Expected exactly one integration executable, got {artifacts}")
    return artifacts.pop()


def assert_single_pass(log, name):
    return (f"test {name} ... ok" in log
            and re.search(r"test result: ok\. 1 passed; 0 failed; 0 ignored;", log) is not None)


def validate_server_cells(specifications):
    versions = [specification.split("=", 1)[0] for specification in specifications]
    if any("=" not in item for item in specifications) or any(
        re.fullmatch(r"\d+\.\d+\.\d+", version) is None for version in versions
    ):
        raise ValueError("Each server must be VERSION=PATH with a numeric major.minor.patch version")
    if len(versions) != len(set(versions)):
        raise ValueError("Use distinct server versions per run; compare same-version binaries in separate output directories")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", action="append", required=True, metavar="VERSION=PATH")
    parser.add_argument("--output", type=Path, default=ROOT / "target/f01/runtime")
    parser.add_argument("--toolchain", default="1.94.0")
    args = parser.parse_args()
    try:
        validate_server_cells(args.server)
    except ValueError as error:
        parser.error(str(error))
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    env = dict(os.environ, CARGO_HOME=str(ROOT / "target/cargo-home"),
               CARGO_TARGET_DIR=str(output / "build"), RUSTUP_TOOLCHAIN=args.toolchain)
    # Artifact inference is part of this check; do not allow inherited overrides.
    env.pop("REDIS_MODULE_TEST_MODULE_DIR", None)
    source_paths = capture(["git", "ls-files", "src", "crates", "examples", "tests",
                            "Cargo.toml", "Cargo.lock", "build.rs", "redismodule.h",
                            "rust-toolchain.toml"]).splitlines()
    source_files = {p: digest(ROOT / p) for p in source_paths}
    report = {"status": "running", "revision": capture(["git", "rev-parse", "HEAD"]),
              "source_files": source_files, "tool_sha256": digest(__file__),
              "source_tree_sha256": hashlib.sha256(json.dumps(source_files, sort_keys=True).encode()).hexdigest(),
              "rustc": capture(["rustc", "+" + args.toolchain, "-vV"]),
              "cargo": capture(["cargo", "+" + args.toolchain, "--version"]),
              "cc": capture(["cc", "--version"]), "host": platform.platform(),
              "libclang_path": env.get("LIBCLANG_PATH"), "features": FEATURES,
              "excluded": EXCLUDED, "servers": [], "builds": [], "cases": []}
    clang = Path(env.get("LIBCLANG_PATH", "/nonexistent")) / "libclang.dylib"
    if clang.is_file():
        report["libclang_sha256"] = digest(clang)
    try:
        for specification in args.server:
            version, path = specification.split("=", 1)
            binary = Path(path).resolve(strict=True)
            version_output = capture([str(binary), "--version"])
            if f"v={version} " not in version_output:
                raise RuntimeError(f"Server does not report required version {version}: {version_output}")
            report["servers"].append({"expected_version": version, "binary": str(binary),
                                      "sha256": digest(binary), "version_output": version_output})
        for profile in ("debug", "release"):
            flags = ["--locked", "--no-default-features", "--features", FEATURES]
            if profile == "release":
                flags.append("--release")
            executable = None
            for label, command in (
                ("examples", ["build", "--examples", *flags]),
                ("integration", ["test", "--test", "integration", "--no-run", "--message-format=json", *flags]),
            ):
                result = run(["cargo", "+" + args.toolchain, *command], env, output,
                             profile + "-" + label, 600)
                report["builds"].append(result)
                if result["exit_code"] != 0 or result["timeout"]:
                    raise RuntimeError(f"Build failed: {result['log']}")
                if label == "integration":
                    executable = integration_executable((output / result["log"]).read_text())
            listing = capture([executable, "--list", "--format", "terse"])
            names = {line.removesuffix(": test") for line in listing.splitlines() if line.endswith(": test")}
            selected = {"test_" + case for case in CASES}
            missing, unknown = selected - names, names - selected - EXCLUDED.keys()
            unknown = {name for name in unknown if not name.startswith("utils::harness_tests::")}
            if missing or unknown:
                raise RuntimeError(f"Review case inventory: missing={missing}, unknown={unknown}")
            module_dir = Path(executable).parent.parent / "examples"
            artifacts = {str(path.relative_to(output)): digest(path)
                         for path in module_dir.glob("lib*")
                         if path.suffix in {".so", ".dylib", ".dll"}}
            if not artifacts:
                raise RuntimeError(f"No module artifacts found next to {executable}")
            report.setdefault("artifacts", {})[profile] = artifacts
            report.setdefault("executables", {})[profile] = {"path": executable, "sha256": digest(executable)}
            for server in report["servers"]:
                version = server["expected_version"]
                for case in CASES:
                    name = "test_" + case
                    label = profile + "-" + version + "-" + case
                    case_dir = output / "servers" / label
                    case_env = dict(env, REDIS_MODULE_TEST_REDIS_SERVER=server["binary"],
                                    REDIS_MODULE_TEST_EXPECT_REDIS_VERSION=version,
                                    REDIS_MODULE_TEST_LOG_DIR=str(case_dir))
                    result = run([executable, "--exact", name, "--nocapture"], case_env, output, label, 60)
                    passed = result["exit_code"] == 0 and not result["timeout"] and assert_single_pass(
                        (output / result["log"]).read_text(), name)
                    info = list(case_dir.glob("*/server-info.txt"))
                    memory = list(case_dir.glob("*/memory-info.txt"))
                    passed = passed and bool(info) and len(info) == len(memory)
                    result.update({"test": name, "profile": profile, "server": version,
                                   "status": "passed" if passed else "failed",
                                   "server_info": [str(p.relative_to(output)) for p in info],
                                   "memory_info": [str(p.relative_to(output)) for p in memory]})
                    report["cases"].append(result)
                    write_json(output / "report.json", report)
                    print(label + ": " + result["status"], flush=True)
        report["status"] = "passed" if all(c["status"] == "passed" for c in report["cases"]) else "failed"
    except BaseException as error:
        report["status"], report["error"] = "failed", str(error)
        raise
    finally:
        write_json(output / "report.json", report)
    return 0 if report["status"] == "passed" else 1


if __name__ == "__main__":
    sys.exit(main())
