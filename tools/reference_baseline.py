#!/usr/bin/env python3
"""Build isolated snapshots of four local consumers without editing their trees.

Python 3.11+ and Rust 1.94.0 are required. Outputs and Cargo's writable cache stay
under target/f01 and target/cargo-home. This deliberately validates current local
source, recording dirty status and source digests, rather than silently testing
only the last commit. Runtime tests are a separate, explicitly selected step.
"""

import argparse
import difflib
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import socket
import subprocess
import sys
import time


ROOT = Path(__file__).resolve().parents[1]
OUTPUT = ROOT / "target/f01/references"
CONFIG = {
    "jmespath": ("redis-jmespath-module", "redis-jmespath", "Cargo.toml"),
    "eventstream": ("redis-event-stream-module", "redis-event-stream-module", "Cargo.toml"),
    "streamfilter": ("redis-stream-filter-module", "redis-stream-filter-module", "Cargo.toml"),
    "carolina": ("carolina", "klog-module", "crates/klog-module/Cargo.toml"),
}
SDK_PACKAGES = {"redis-module", "redis-module-macros", "redis-module-macros-internals", "redis-module-common"}
SDK_MANIFESTS = {
    "redis-module": ROOT / "Cargo.toml",
    "redis-module-macros": ROOT / "crates/macros/Cargo.toml",
    "redis-module-macros-internals": ROOT / "crates/macros-internals/Cargo.toml",
    "redis-module-common": ROOT / "crates/common/Cargo.toml",
}


def sha(data):
    return hashlib.sha256(data).hexdigest()


def git(source, *args):
    return subprocess.check_output(["git", "-C", str(source), *args])


def selected(path, name):
    first = path.split("/", 1)[0]
    if name == "carolina":
        return path in {"Cargo.toml", "Cargo.lock"} or path.startswith("crates/klog-module/")
    return first in {"src", "crates", "tests", "benches", "examples", "functions", ".cargo"} or path in {
        "Cargo.toml", "Cargo.lock", "build.rs", "README.md", "LICENSE", "LICENSE-MIT", "LICENSE-APACHE",
    }


def prepare(name):
    directory, package, manifest = CONFIG[name]
    source = ROOT.parent / directory
    destination = OUTPUT / name
    if destination.exists():
        raise RuntimeError(f"{destination} already exists; use --reuse to build the existing snapshot")
    paths = sorted(set(git(source, "ls-files", "-z", "--cached", "--others", "--exclude-standard").decode().split("\0")) - {""})
    file_hashes = {}
    destination.mkdir(parents=True)
    for path in paths:
        if not selected(path, name) or not (source / path).is_file():
            continue
        if (source / path).is_symlink():
            raise RuntimeError(f"Refusing to dereference snapshot symlink: {source / path}")
        data = (source / path).read_bytes()
        file_hashes[path] = sha(data)
        target = destination / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
    record = {
        "name": name, "package": package, "source": str(source),
        "source_revision": git(source, "rev-parse", "HEAD").decode().strip(),
        "source_status": git(source, "status", "--porcelain=v1", "--untracked-files=all").decode(),
        "tracked_diff_sha256": sha(git(source, "diff", "HEAD", "--binary")),
        "source_files": file_hashes,
        "source_tree_sha256": sha(json.dumps(file_hashes, sort_keys=True).encode()),
        "original_lock_sha256": file_hashes.get("Cargo.lock"),
        "sdk_revision": git(ROOT, "rev-parse", "HEAD").decode().strip(),
        "migrations": [], "commands": [],
    }
    target = destination / manifest
    text = target.read_text()
    replacements = {
        "redis-module": '{ path = ' + json.dumps(str(ROOT)) + ', default-features = false, features = ["min-redis-compatibility-version-7-2", "bindgen-runtime"] }',
        "redis-module-macros": '{ path = ' + json.dumps(str(ROOT / "crates/macros")) + ' }',
    }
    for dependency, replacement in replacements.items():
        pattern = r"(?m)^" + re.escape(dependency) + r"\s*=\s*\{[^}]+\}"
        text, count = re.subn(pattern, dependency + " = " + replacement, text)
        if count != 1:
            raise RuntimeError(f"Expected one {dependency} dependency in {target}, found {count}")
    target.write_text(text)
    record["migrations"].append("Select this fork's facade and public macros by path; explicitly enable bindgen-runtime alongside the existing Redis 7.2 API floor.")
    if name == "jmespath":
        with target.open("a") as handle:
            handle.write('\n# F01 isolated consumer workspace.\n[workspace]\n')
        record["migrations"].append("Add an empty workspace table to detach the snapshot from the enclosing SDK workspace (harness only).")
    if name == "carolina":
        workspace = destination / "Cargo.toml"
        text = workspace.read_text()
        text = re.sub(r'(?m)^(members|default-members)\s*=\s*\[[^]]*\]', r'\1 = ["crates/klog-module"]', text)
        workspace.write_text(text)
        record["migrations"].append("Restrict the snapshot workspace to klog-module; broker/client/benchmark crates are outside this SDK adoption check (harness only).")
    (destination / "baseline.json").write_text(json.dumps(record, indent=2) + "\n")
    return record


def environment():
    result = dict(os.environ)
    result["CARGO_HOME"] = str(ROOT / "target/cargo-home")
    result["CARGO_TARGET_DIR"] = str(OUTPUT / "build")
    result["RUSTUP_TOOLCHAIN"] = "1.94.0"
    result["TEST_REDIS_SERVER_BIN"] = str(Path("/opt/homebrew/bin/redis-server").resolve())
    result["TEST_REDIS_CLI_BIN"] = str(Path("/opt/homebrew/bin/redis-cli").resolve())
    result.setdefault("LIBCLANG_PATH", "/Library/Developer/CommandLineTools/usr/lib")
    return result


def run(name, record, arguments, label, timeout):
    destination = OUTPUT / name
    original_label = label
    suffix = 1
    while (destination / f"{label}.stdout").exists():
        label = f"{original_label}-{suffix}"
        suffix += 1
    command = ["cargo", "+1.94.0", *arguments]
    started = time.monotonic()
    with (destination / f"{label}.stdout").open("w") as stdout, (destination / f"{label}.stderr").open("w") as stderr:
        try:
            process = subprocess.Popen(command, cwd=destination, env=environment(), stdout=stdout, stderr=stderr, start_new_session=True)
            code = process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()
            code = 124
        finally:
            # Test harnesses normally own teardown. Also reclaim any remaining
            # descendants in our private process group after the command exits.
            if "process" in locals():
                try:
                    os.killpg(process.pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
    record["commands"].append({"argv": command, "exit_code": code, "seconds": round(time.monotonic() - started, 3), "stdout": f"{label}.stdout", "stderr": f"{label}.stderr"})
    (destination / "baseline.json").write_text(json.dumps(record, indent=2) + "\n")
    print(f"{name}: {label}: exit {code}", flush=True)
    return code


def build(name, record, profile, timeout):
    destination = OUTPUT / name
    # Initial resolution intentionally replaces the original git graph and may
    # update Cargo.lock. Every actual compilation uses the resulting locked graph.
    if run(name, record, ["metadata", "--format-version=1"], "metadata", timeout):
        return False
    metadata = json.loads((destination / record["commands"][-1]["stdout"]).read_text())
    graph = [package for package in metadata["packages"] if package["name"] in SDK_PACKAGES]
    if len(graph) != 4 or {p["name"] for p in graph} != SDK_PACKAGES:
        raise RuntimeError("Missing or duplicate SDK packages")
    resolved = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    for package in graph:
        if package["source"] is not None or Path(package["manifest_path"]).resolve() != SDK_MANIFESTS[package["name"]]:
            raise RuntimeError("SDK graph escaped this fork")
    record["sdk_graph"] = [{"name": p["name"], "version": p["version"], "manifest_path": p["manifest_path"], "source": p["source"], "features": resolved[p["id"]]["features"]} for p in graph]
    record["adapted_lock_sha256"] = sha((destination / "Cargo.lock").read_bytes())
    record["adapted_manifests"] = {str(path.relative_to(destination)): sha(path.read_bytes()) for path in destination.rglob("Cargo.toml")}
    arguments = ["build", "--locked", "-p", record["package"], "--lib"]
    if profile == "release":
        arguments.append("--release")
    return run(name, record, arguments, f"build-{profile}", timeout) == 0


def redis_command(port, *arguments):
    """Minimal RESP2 client for bounded, ordinary baseline commands."""
    encoded = [str(argument).encode() for argument in arguments]
    wire = f"*{len(encoded)}\r\n".encode() + b"".join(f"${len(arg)}\r\n".encode() + arg + b"\r\n" for arg in encoded)
    with socket.create_connection(("127.0.0.1", port), timeout=2) as connection:
        connection.sendall(wire)
        reader = connection.makefile("rb")

        def read():
            prefix = reader.read(1)
            line = reader.readline().removesuffix(b"\r\n")
            if prefix == b"+":
                return line.decode()
            if prefix == b"-":
                raise RuntimeError(line.decode())
            if prefix == b":":
                return int(line)
            if prefix == b"$":
                length = int(line)
                if length == -1:
                    return None
                data = reader.read(length)
                if reader.read(2) != b"\r\n":
                    raise RuntimeError("Incomplete bulk reply")
                return data.decode()
            if prefix == b"*":
                length = int(line)
                return None if length == -1 else [read() for _ in range(length)]
            raise RuntimeError("Unexpected Redis reply")

        return read()


def runtime(name, record, server_binary):
    """Only selected normal paths; broad consumer suites remain separate work."""
    destination = OUTPUT / name
    extension = "dylib" if sys.platform == "darwin" else "so"
    artifact = OUTPUT / "build/release" / f'lib{record["package"].replace("-", "_")}.{extension}'
    record["runtime"] = {"status": "incomplete", "artifact_sha256": sha(artifact.read_bytes()), "server_binary": str(server_binary), "server_binary_sha256": sha(server_binary.read_bytes()), "checks": []}
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        port = probe.getsockname()[1]
    command = [str(server_binary), "--bind", "127.0.0.1", "--port", str(port), "--save", "", "--appendonly", "no", "--dir", str(destination), "--loadmodule", str(artifact)]
    with (destination / "runtime-server.log").open("w") as log:
        process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        try:
            deadline = time.monotonic() + 10
            while True:
                if process.poll() is not None:
                    raise RuntimeError(f"{name} host exited during startup; inspect runtime-server.log")
                try:
                    info = redis_command(port, "INFO", "server")
                    break
                except (ConnectionError, OSError):
                    if time.monotonic() >= deadline:
                        raise RuntimeError(f"{name} host startup exceeded 10 seconds")
                    time.sleep(0.05)
            fields = dict(line.split(":", 1) for line in info.splitlines() if ":" in line)
            if int(fields["process_id"]) != process.pid:
                raise RuntimeError("Connected server is not the owned process")
            record["runtime"].update({"redis_version": fields["redis_version"], "owned_pid_verified": True, "arch_bits": fields["arch_bits"], "os": fields["os"]})
            record["runtime"]["checks"].append("module loaded in an owned loopback server; INFO process ID matched child")
            if name == "jmespath":
                if redis_command(port, "JMESPATH.EVAL", '{"users":[{"name":"A","age":30},{"name":"B","age":20}]}', 'users[?age > `25`].name') != '["A"]':
                    raise RuntimeError("JMESPath EVAL output mismatch")
                if redis_command(port, "SET", "f01:document", '{"items":[3,1,2]}') != "OK":
                    raise RuntimeError("SET did not succeed")
                if redis_command(port, "JMESPATH.QUERY", "f01:document", "sort(items)") != "[1,2,3]":
                    raise RuntimeError("JMESPath QUERY output mismatch")
                record["runtime"]["checks"].extend(["JMESPATH.EVAL filter/projection exact JSON", "JMESPATH.QUERY on a Redis String exact sorted JSON"])
                record["runtime"]["optional_hosts"] = "RedisJSON/Search/TimeSeries integration not exercised"
            if redis_command(port, "PING") != "PONG":
                raise RuntimeError("PING did not succeed")
            record["runtime"]["status"] = "passed"
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait()
            (destination / "baseline.json").write_text(json.dumps(record, indent=2) + "\n")
    print(f"{name}: owned host runtime: passed", flush=True)
    return True


def selected_tests(name, record, timeout):
    """Adapt only test infrastructure, then run a small reviewed test allowlist."""
    if name == "jmespath":
        return True  # The owned-server EVAL/QUERY checks cover this selection.
    destination = OUTPUT / name
    if name == "carolina":
        path = destination / "crates/klog-module/tests/live.rs"
        before = path.read_text()
        after = before.replace('manifest\n        .join("../../target/release")', 'std::env::var("CARGO_TARGET_DIR")\n        .map(PathBuf::from)\n        .unwrap_or_else(|_| manifest.join("../../target"))\n        .join("release")')
        after = after.replace('.args(["build", "--release", "-p", "klog-module"])', '.args(["build", "--locked", "--release", "-p", "klog-module"])')
        after = after.replace('let server = RedisServer::new()\n', 'let server = RedisServer::new()\n        .redis_server_bin(std::env::var("TEST_REDIS_SERVER_BIN").expect("pinned F01 server"))\n')
        after = after.replace('.redis_server_bin("redis-server")', '.redis_server_bin(std::env::var("TEST_REDIS_SERVER_BIN").expect("pinned F01 server"))')
        tests = [("live", "live_round_trip_and_rdb_restart"), ("live", "blocking_fetch_wakes_when_append_makes_data_visible"), ("live", "aof_rewrite_emits_restorable_native_value")]
    else:
        path = destination / "tests/common/mod.rs"
        before = path.read_text()
        after = before.replace('.args(["build", "--release"])', f'.args(["build", "--locked", "--release", "-p", "{record["package"]}"])')
        after = after.replace('.args(["build", "--release", "--lib"])', f'.args(["build", "--locked", "--release", "--lib", "-p", "{record["package"]}"])')
        tests = [("entry_format", "default_format_is_fixed_and_byte_identical")] if name == "eventstream" else [("equivalence", "filtered_groups_match_client_side_filtering"), ("equivalence", "else_group_is_the_complement_of_its_siblings")]
    if after != before:
        path.write_text(after)
        record["runtime_harness_adaptations"] = {"path": str(path.relative_to(destination)), "before_sha256": sha(before.encode()), "after_sha256": sha(after.encode()), "patch": "runtime-harness.patch", "purpose": "Lock and scope nested builds; Carolina additionally honors shared CARGO_TARGET_DIR and the pinned server binary."}
        (destination / "runtime-harness.patch").write_text("".join(difflib.unified_diff(before.splitlines(keepends=True), after.splitlines(keepends=True), fromfile=str(path.relative_to(destination)), tofile=str(path.relative_to(destination)))))
    outcomes = []
    for target, test in tests:
        label = "test-" + test
        code = run(name, record, ["test", "--locked", "--release", "-p", record["package"], "--test", target, test, "--", "--exact", "--test-threads=1"], label, timeout)
        # Cargo succeeds even with a misspelled filter and zero tests. Require
        # one actual passing test, in addition to the process exit status.
        stdout = (destination / record["commands"][-1]["stdout"]).read_text()
        passed = code == 0 and "1 passed; 0 failed" in stdout
        outcomes.append({"test": test, "passed": passed})
        if not passed:
            break
    record["selected_tests"] = outcomes
    extension = "dylib" if sys.platform == "darwin" else "so"
    artifact = OUTPUT / "build/release" / f'lib{record["package"].replace("-", "_")}.{extension}'
    record["selected_tests_artifact_sha256"] = sha(artifact.read_bytes())
    (destination / "baseline.json").write_text(json.dumps(record, indent=2) + "\n")
    return all(outcome["passed"] for outcome in outcomes)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--only", choices=CONFIG, action="append")
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--prepare-only", action="store_true")
    parser.add_argument("--reuse", action="store_true", help="Reuse an existing isolated snapshot; do not overwrite its adaptations")
    parser.add_argument("--profile", choices=["debug", "release"], default="debug")
    mode.add_argument("--runtime-only", action="store_true", help="Use existing release builds for ordinary owned-server checks")
    mode.add_argument("--selected-tests", action="store_true", help="Run only the reviewed reference test allowlist with harness-only adaptations")
    parser.add_argument("--redis-server", type=Path, default=Path("/opt/homebrew/bin/redis-server"))
    parser.add_argument("--timeout", type=int, default=900, help="Per Cargo command deadline in seconds")
    args = parser.parse_args()
    if (args.runtime_only or args.selected_tests) and not args.reuse:
        parser.error("--runtime-only and --selected-tests require --reuse")
    if args.timeout <= 0:
        parser.error("--timeout must be positive")
    failed = []
    for name in args.only or CONFIG:
        record = json.loads((OUTPUT / name / "baseline.json").read_text()) if args.reuse else prepare(name)
        if args.selected_tests:
            if not selected_tests(name, record, args.timeout):
                failed.append(name)
        elif args.runtime_only:
            runtime(name, record, args.redis_server.resolve())
        elif not args.prepare_only and not build(name, record, args.profile, args.timeout):
            failed.append(name)
    return bool(failed)


if __name__ == "__main__":
    sys.exit(main())
