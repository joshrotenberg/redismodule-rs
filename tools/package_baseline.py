#!/usr/bin/env python3
"""Build the four Cargo archives and prove external consumers use those bytes.

Python 3.11+, Cargo and libclang are required. No publication, Cargo configuration
outside the output directory, source patches, or checkout dependencies are used.
The registry serves only loopback, on an ephemeral port, for this process's life.
Cargo registry schema: https://doc.rust-lang.org/cargo/reference/registry-index.html
"""

from __future__ import annotations

import argparse
import contextlib
import functools
import hashlib
import http.server
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import signal
import socket
import subprocess
import sys
import tarfile
import threading
import time
import tomllib


PACKAGES = (
    "redis-module-common",
    "redis-module-macros-internals",
    "redis-module-macros",
    "redis-module",
)
CRATES_IO = "https://github.com/rust-lang/crates.io-index"


class BaselineError(Exception):
    """A baseline command or source-provenance requirement failed."""


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write_json(path: Path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def stop_process(process):
    if process.poll() is None:
        os.killpg(process.pid, signal.SIGTERM)
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=5)


class Runner:
    def __init__(self, output: Path, env: dict, timeout: int):
        self.output, self.env, self.timeout = output, env, timeout
        self.commands = []

    def run(self, args, cwd: Path, label: str) -> str:
        log = self.output / "logs" / f"{label}.log"
        log.parent.mkdir(parents=True, exist_ok=True)
        started = time.monotonic()
        print(f"[{label}] {' '.join(map(str, args))}", flush=True)
        with log.open("w") as stream:
            process = subprocess.Popen(args, cwd=cwd, env=self.env, stdout=stream,
                                       stderr=subprocess.STDOUT, start_new_session=True)
            try:
                status = process.wait(timeout=self.timeout)
            except (subprocess.TimeoutExpired, KeyboardInterrupt):
                stop_process(process)
                status = process.returncode
                raise
            finally:
                self.commands.append({"argv": list(map(str, args)), "cwd": str(cwd),
                                      "returncode": process.returncode,
                                      "seconds": round(time.monotonic() - started, 3),
                                      "log": str(log.relative_to(self.output))})
                write_json(self.output / "commands.json", self.commands)
        if status:
            raise BaselineError(f"{label} failed ({status}); see {log}\n{log.read_text()[-4000:]}")
        return log.read_text()


def archive_manifest(path: Path) -> tuple[dict, dict[str, str]]:
    """Validate archive paths and record every packaged file's digest."""
    files = {}
    prefix = path.name.removesuffix(".crate")
    manifest = None
    with tarfile.open(path, "r:gz") as archive:
        for member in archive.getmembers():
            parts = PurePosixPath(member.name).parts
            if (not parts or parts[0] != prefix or ".." in parts
                    or member.name.startswith("/") or "\\" in member.name
                    or member.name != PurePosixPath(member.name).as_posix()
                    or not member.isfile()):
                raise BaselineError(f"Unexpected archive member: {member.name}")
            relative = "/".join(parts[1:])
            if relative in files:
                raise BaselineError(f"Duplicate archive member: {member.name}")
            source = archive.extractfile(member)
            assert source is not None
            content = source.read()
            files[relative] = hashlib.sha256(content).hexdigest()
            if relative == "Cargo.toml":
                manifest = tomllib.loads(content.decode())
    if manifest is None or not {"Cargo.toml.orig", "LICENSE", "src/lib.rs"} <= files.keys():
        raise BaselineError(f"Missing manifest, source or license in {path.name}")
    if "workspace" in manifest or "patch" in manifest or "replace" in manifest:
        raise BaselineError(f"Archive still has workspace/source overrides: {path.name}")
    for _, _, dependencies in dependency_tables(manifest):
        for dep in dependencies.values():
            if any(key in dep for key in ("path", "git", "workspace")):
                raise BaselineError(f"Archive has non-registry dependencies: {path.name}")
    return manifest, files


def dependency_tables(manifest):
    kinds = {"dependencies": "normal", "build-dependencies": "build", "dev-dependencies": "dev"}
    for table, kind in kinds.items():
        yield kind, None, manifest.get(table, {})
    for target, tables in manifest.get("target", {}).items():
        for table, kind in kinds.items():
            yield kind, target, tables.get(table, {})


def index_entry(manifest: dict, checksum: str) -> dict:
    dependencies = []
    for kind, target, table in dependency_tables(manifest):
        for name, dependency in sorted(table.items()):
            package = dependency.get("package", name)
            dependencies.append({"name": name, "package": dependency.get("package"),
                                 "req": dependency["version"], "kind": kind,
                                 "target": target, "features": dependency.get("features", []),
                                 "optional": dependency.get("optional", False),
                                 "default_features": dependency.get("default-features", True),
                                 "registry": None if package in PACKAGES else CRATES_IO})
    package = manifest["package"]
    return {"name": package["name"], "vers": package["version"], "deps": dependencies,
            "cksum": checksum, "features": manifest.get("features", {}),
            "yanked": False, "links": package.get("links"),
            "rust_version": package.get("rust-version")}


class QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_):
        pass


@contextlib.contextmanager
def registry_server(directory: Path):
    handler = functools.partial(QuietHandler, directory=str(directory))
    with http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler) as server:
        base = f"http://127.0.0.1:{server.server_port}"
        write_json(directory / "index/config.json", {"dl": base + "/archives/{crate}-{version}.crate"})
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            yield "sparse+" + base + "/index/"
        finally:
            server.shutdown()
            thread.join(timeout=5)


def consumer_manifest(kind: str, versions: dict) -> str:
    macros = (f'redis-module-macros = {{ version = "={versions["redis-module-macros"]}", '
              'registry = "candidate" }\n') if kind != "manual" else ""
    crate_type = "rlib" if kind == "registration" else "cdylib"
    return (f'[package]\nname = "archive-{kind}"\nversion = "0.0.0"\nedition = "2021"\n'
            f'publish = false\n\n[workspace]\n\n[lib]\ncrate-type = ["{crate_type}"]\n\n'
            '[dependencies]\n'
            f'redis-module = {{ version = "={versions["redis-module"]}", registry = "candidate", '
            'default-features = false, features = ["min-redis-compatibility-version-7-2", '
            '"bindgen-runtime"] }\n' + macros)


def verify_consumer_dependencies(metadata, kind: str) -> list[str]:
    root_id = metadata["resolve"]["root"]
    package = next((p for p in metadata["packages"] if p["id"] == root_id), None)
    expected = {"redis-module"} if kind == "manual" else {"redis-module", "redis-module-macros"}
    actual = {dependency["name"] for dependency in package["dependencies"]} if package else set()
    if actual != expected:
        raise BaselineError(f"Unexpected direct dependencies for {kind}: {sorted(actual)}")
    return sorted(actual)


def verify_metadata(metadata, lock, registry, archives, cargo_home: Path, expected_names: set[str]) -> list[dict]:
    found = []
    for package in metadata["packages"]:
        name = package["name"]
        if name not in PACKAGES:
            continue
        expected = archives[name]
        source = registry
        manifest_path = Path(package["manifest_path"])
        if (package["version"] != expected["version"] or package["source"] != source
                or not manifest_path.resolve().is_relative_to((cargo_home / "registry/src").resolve())):
            raise BaselineError(f"{name} resolved outside this candidate registry")
        matches = [p for p in lock["package"] if p["name"] == name]
        if (len(matches) != 1 or matches[0].get("version") != expected["version"]
                or matches[0].get("source") != source or matches[0].get("checksum") != expected["sha256"]):
            raise BaselineError(f"{name} lock source/checksum differs from candidate archive")
        registry_id = manifest_path.parent.parent.name
        cached_archive = cargo_home / "registry/cache" / registry_id / f"{name}-{package['version']}.crate"
        if not cached_archive.is_file() or sha256(cached_archive) != expected["sha256"]:
            raise BaselineError(f"{name} cached archive has a different package checksum")
        for relative, checksum in expected["files"].items():
            if sha256(manifest_path.parent / relative) != checksum:
                raise BaselineError(f"Extracted file differs from archive: {name}/{relative}")
        found.append({"name": name, "version": package["version"], "source": source,
                      "manifest_path": str(manifest_path), "sha256": expected["sha256"],
                      "verified_files": len(expected["files"])})
    if len(found) != len(expected_names) or {p["name"] for p in found} != expected_names:
        raise BaselineError("Unexpected candidate package graph")
    return sorted(found, key=lambda package: package["name"])


def read_resp2(reader):
    line = reader.readline()
    if not line.endswith(b"\r\n"):
        raise BaselineError("Truncated Redis response header")
    if line.startswith(b"+"):
        return line[1:-2]
    if line.startswith(b"-"):
        raise BaselineError("Redis error: " + line[1:-2].decode("utf-8", "replace"))
    if line.startswith(b":"):
        return int(line[1:-2])
    if line.startswith((b"$", b"*")):
        size = int(line[1:-2])
        if size == -1:
            return None
        if size < 0:
            raise BaselineError("Invalid Redis response length")
        if line.startswith(b"*"):
            return [read_resp2(reader) for _ in range(size)]
        result = reader.read(size)
        if len(result) != size or reader.read(2) != b"\r\n":
            raise BaselineError("Truncated Redis bulk response")
        return result
    raise BaselineError(f"Unexpected Redis response: {line!r}")


def redis_command(port: int, *args: bytes):
    request = b"*" + str(len(args)).encode() + b"\r\n"
    for arg in args:
        request += b"$" + str(len(arg)).encode() + b"\r\n" + arg + b"\r\n"
    with socket.create_connection(("127.0.0.1", port), timeout=2) as connection:
        connection.sendall(request)
        with connection.makefile("rb") as reader:
            return read_resp2(reader)


def info_fields(raw: bytes) -> dict[str, str]:
    return dict(line.split(":", 1) for line in raw.decode("utf-8", "replace").splitlines()
                if ":" in line and not line.startswith("#"))


def verify_server_info(raw: bytes, process_id: int, expected_version: str) -> dict[str, str]:
    fields = info_fields(raw)
    if fields.get("process_id") != str(process_id):
        raise BaselineError("Redis on the selected port is not the owned server")
    if fields.get("redis_version") != expected_version:
        raise BaselineError(f"Expected Redis {expected_version}, got {fields.get('redis_version')}")
    return fields


def proc_runtime_cases(query) -> list[str]:
    """Validate RESP2 behavior; queries use fresh connections, not session recovery."""
    entry = query(b"COMMAND", b"INFO", b"archive.typed")[0]
    if entry[0:2] != [b"archive.typed", 2] or b"readonly" not in entry[2]:
        raise BaselineError("Archived typed command name/arity/flags differ")
    if entry[3:6] != [0, 0, 0] or entry[8] != []:
        raise BaselineError("Archived typed command must have no key specs")
    docs_reply = query(b"COMMAND", b"DOCS", b"archive.typed")
    docs = dict(zip(docs_reply[::2], docs_reply[1::2]))
    details_reply = docs[b"archive.typed"]
    details = dict(zip(details_reply[::2], details_reply[1::2]))
    if details.get(b"summary") != b"Return the parsed integer and a fixed message.":
        raise BaselineError("Archived typed command summary differs")
    for number in (0, -1, 7, -(2 ** 63), 2 ** 63 - 1):
        if query(b"archive.typed", str(number).encode()) != [
            b"count", number, b"message", b"packaged macros"
        ]:
            raise BaselineError("Archived typed command changed its derived reply")
    for args, expected in [
        ((b"archive.typed",), "wrong number of arguments"),
        ((b"archive.typed", b"1", b"extra"), "wrong number of arguments"),
        ((b"archive.typed", b"not-an-integer"), "Couldn't parse as integer"),
    ]:
        try:
            query(*args)
        except BaselineError as error:
            if expected not in str(error):
                raise
        else:
            raise BaselineError("Archived typed command accepted invalid arguments")
        if query(b"archive.typed", b"7") != [b"count", 7, b"message", b"packaged macros"]:
            raise BaselineError("Archived typed command did not recover after an argument error")
    return ["command-info", "command-docs", "typed-i64-replies", "arity-errors",
            "integer-error", "valid-command-after-errors"]


def manual_view_runtime_cases(query) -> list[str]:
    """Validate corrected owner views and this fixture's isolated callback."""
    for sample in (b"", "café 日本語".encode(), b"a\x00b"):
        if query(b"archive.view", sample) != [sample, 1]:
            raise BaselineError("Archived owner/raw text views differ")
    # Seed a nonempty key so a missing empty-key callback cannot match initial state.
    for sample in (b"a\x00b\xff\r\n", b"", bytes(range(256))):
        if query(b"archive.notify", sample) != sample:
            raise BaselineError("Archived callback changed binary key bytes")
    for args in ((b"archive.view", b"\xff"), (b"archive.view",),
                 (b"archive.view", b"one", b"extra")):
        try:
            query(*args)
        except BaselineError as error:
            expected = "UTF-8" if len(args) == 2 else "wrong number of arguments"
            if expected not in str(error):
                raise BaselineError("Unexpected archived view error") from error
        else:
            raise BaselineError("Archived view accepted an invalid argument contract")
        if query(b"archive.view", b"valid") != [b"valid", 1]:
            raise BaselineError("Archived view did not recover after an ordinary error")
    return ["owner-raw-text-identity", "isolated-binary-callback", "view-errors-and-recovery"]


def runtime_manual(binary: Path, redis: str, output: Path, env: dict, *, kind="manual") -> dict:
    """Exercise ordinary command behavior only; clean up on every exit path."""
    executable = shutil.which(redis, path=env.get("PATH"))
    if executable is None:
        raise BaselineError(f"Cannot locate Redis executable: {redis}")
    executable = str(Path(executable).resolve())
    version_result = subprocess.run([executable, "--version"], env=env, check=True,
                                    stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                    text=True, timeout=10)
    version = version_result.stdout.strip()
    (output / "redis-version.txt").write_text(version + "\n")
    expected = re.search(r"\bv=(\d+\.\d+\.\d+)\b", version)
    expected_allocator = re.search(r"\bmalloc=(\S+)", version)
    if expected is None or expected_allocator is None:
        raise BaselineError(f"Cannot determine Redis version/allocator from {version!r}")
    with socket.socket() as reservation:
        reservation.bind(("127.0.0.1", 0))
        port = reservation.getsockname()[1]
    log = output / "redis.log"
    command = [executable, "--bind", "127.0.0.1", "--port", str(port), "--save", "",
               "--appendonly", "no", "--dir", str(output), "--loadmodule", str(binary)]
    with log.open("w") as stream:
        process = subprocess.Popen(command, stdout=stream, stderr=subprocess.STDOUT,
                                   env=env, start_new_session=True)
        try:
            deadline = time.monotonic() + 15
            while True:
                if process.poll() is not None:
                    raise BaselineError(f"Redis exited before startup; see {log}")
                try:
                    if redis_command(port, b"PING") == b"PONG":
                        break
                except (OSError, BaselineError):
                    if time.monotonic() >= deadline:
                        raise BaselineError(f"Redis startup exceeded 15 seconds; see {log}")
                    time.sleep(0.05)
            server_info = redis_command(port, b"INFO", b"server")
            (output / "server-info.txt").write_bytes(server_info)
            identity = verify_server_info(server_info, process.pid, expected.group(1))
            # Collect additional evidence only after establishing ownership.
            memory_info = redis_command(port, b"INFO", b"memory")
            (output / "memory-info.txt").write_bytes(memory_info)
            allocator = info_fields(memory_info).get("mem_allocator")
            if allocator != expected_allocator.group(1):
                raise BaselineError(f"Redis allocator differs from executable identity: {allocator}")
            if time.monotonic() >= deadline or process.poll() is not None:
                raise BaselineError(f"Owned Redis did not become ready before the deadline; see {log}")
            if kind == "proc":
                cases = proc_runtime_cases(lambda *args: redis_command(port, *args))
            elif kind == "manual":
                samples = [b"", b"hello", b"a\x00b\xff\r\n", bytes(range(256))]
                for sample in samples:
                    if redis_command(port, b"archive.echo", sample) != sample:
                        raise BaselineError("Archived module changed binary response bytes")
                cases = ["empty", "ascii", "nul-non-utf8-crlf", "all-256-byte-values"]
                cases.extend(manual_view_runtime_cases(lambda *args: redis_command(port, *args)))
            else:
                raise BaselineError(f"No runtime contract for {kind}")
            if redis_command(port, b"PING") != b"PONG":
                raise BaselineError("Redis did not respond after commands")
            return {"result": "passed", "argv": command, "binary_sha256": sha256(binary),
                    "cases": cases, "kind": kind, "log": str(log),
                    "redis_version": version, "redis_binary_sha256": sha256(Path(executable)),
                    "expected_redis_version": expected.group(1), "actual_redis_version": identity["redis_version"],
                    "process_id": process.pid, "allocator": allocator,
                    "server_info_sha256": sha256(output / "server-info.txt"),
                    "memory_info_sha256": sha256(output / "memory-info.txt")}
        finally:
            stop_process(process)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=Path("target/f01/packages"))
    parser.add_argument("--toolchains", nargs="+", default=["1.94.0", "stable"])
    parser.add_argument("--command-timeout", type=int, default=600)
    parser.add_argument("--redis-server", default="redis-server")
    parser.add_argument("--skip-runtime", action="store_true")
    options = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    output = options.output.resolve()
    if output.exists():
        raise BaselineError(f"Output exists; choose a fresh --output to retain evidence: {output}")
    output.mkdir(parents=True)
    env = os.environ.copy()
    # Use an explicit home so project/user source replacement settings cannot
    # silently redirect the consumer. External dependencies may use its cache.
    cargo_home = output / "cargo-home"
    cargo_home.mkdir()
    env["CARGO_HOME"] = str(cargo_home)
    env.setdefault("CARGO_HTTP_TIMEOUT", "60")
    env.setdefault("CARGO_NET_RETRY", "2")
    runner = Runner(output, env, options.command_timeout)
    report = {"result": "incomplete", "archives": {}, "consumers": [],
              "limitations": ["selected RESP2 module flows only", "no MSRV declaration",
                              "no GitHub runner verification", "no release publication"]}
    try:
        report["source_head"] = runner.run(["git", "rev-parse", "HEAD"], root, "source-head").strip()
        report["source_status"] = runner.run(["git", "status", "--short"], root, "source-status").splitlines()
        metadata = json.loads(runner.run(["cargo", "+1.94.0", "metadata", "--locked", "--no-deps",
                                          "--format-version", "1"], root, "source-metadata"))
        versions = {p["name"]: p["version"] for p in metadata["packages"] if p["name"] in PACKAGES}
        if set(versions) != set(PACKAGES):
            raise BaselineError("Workspace package identities differ from expected four")
        registry = output / "registry"
        archive_dir = registry / "archives"
        archive_dir.mkdir(parents=True)
        for name in PACKAGES:
            runner.run(["cargo", "+1.94.0", "package", "--locked", "--allow-dirty", "--no-verify",
                        "--package", name, "--target-dir", str(output / "packaging")], root, f"package-{name}")
            path = output / "packaging/package" / f"{name}-{versions[name]}.crate"
            manifest, files = archive_manifest(path)
            if manifest["package"]["name"] != name or manifest["package"]["version"] != versions[name]:
                raise BaselineError("Archive filename and package identity differ")
            digest = sha256(path)
            shutil.copy2(path, archive_dir / path.name)
            entry = index_entry(manifest, digest)
            index_path = registry / "index" / name[:2] / name[2:4] / name
            write_json(index_path, entry)
            # Sparse entries require one JSON object per physical line.
            index_path.write_text(json.dumps(entry, sort_keys=True) + "\n")
            report["archives"][name] = {"version": versions[name], "sha256": digest,
                                       "archive": str((archive_dir / path.name).relative_to(output)), "files": files}
        with registry_server(registry) as index:
            report["registry"] = index
            for toolchain in options.toolchains:
                compiler = runner.run(["rustc", "+" + toolchain, "--version"], root, f"compiler-{toolchain}").strip()
                for kind in ("manual", "proc", "registration"):
                    label = toolchain + "-" + kind
                    consumer = output / "consumers" / label
                    shutil.copytree(root / "fixtures/package-consumers" / kind, consumer)
                    (consumer / "Cargo.toml").write_text(consumer_manifest(kind, versions))
                    (consumer / ".cargo").mkdir()
                    (consumer / ".cargo/config.toml").write_text(f'[registries.candidate]\nindex = "{index}"\n')
                    target = output / "build" / toolchain
                    args = ["cargo", "+" + toolchain]
                    runner.run(args + ["generate-lockfile"], consumer, label + "-lock")
                    metadata_text = runner.run(args + ["metadata", "--locked", "--format-version", "1"],
                                               consumer, label + "-metadata")
                    # Cargo diagnostics share the log, but metadata JSON is one final line.
                    data = json.loads(next(line for line in metadata_text.splitlines() if line.startswith('{"packages":')))
                    write_json(consumer / "metadata.json", data)
                    direct_dependencies = verify_consumer_dependencies(data, kind)
                    lock = tomllib.loads((consumer / "Cargo.lock").read_text())
                    expected = set(PACKAGES) - {"redis-module-macros"} if kind == "manual" else set(PACKAGES)
                    graph = verify_metadata(data, lock, index, report["archives"], cargo_home, expected)
                    runner.run(args + ["build", "--locked", "--target-dir", str(target)], consumer, label + "-build")
                    runner.run(args + ["build", "--locked", "--release", "--target-dir", str(target)], consumer, label + "-release")
                    item = {"kind": kind, "toolchain": toolchain, "compiler": compiler, "result": "passed",
                            "graph": graph, "direct_dependencies": direct_dependencies,
                            "lock_sha256": sha256(consumer / "Cargo.lock"),
                            "manifest_sha256": sha256(consumer / "Cargo.toml"), "profiles": ["debug", "release"]}
                    if kind == "registration":
                        for profile in ([], ["--release"]):
                            runner.run(args + ["test", "--lib", "--locked", "--target-dir", str(target)] + profile,
                                       consumer, label + "-test" + ("-release" if profile else ""))
                        item["registration_tests"] = ["debug", "release"]
                    if kind == "manual" and not options.skip_runtime:
                        extension = "dylib" if sys.platform == "darwin" else "so"
                        item["runtime"] = runtime_manual(target / "release" / f"libarchive_manual.{extension}",
                                                          options.redis_server, consumer, env)
                    if kind == "proc" and not options.skip_runtime:
                        extension = "dylib" if sys.platform == "darwin" else "so"
                        item["runtime"] = {}
                        for profile in ("debug", "release"):
                            run_dir = consumer / ("runtime-" + profile)
                            run_dir.mkdir()
                            item["runtime"][profile] = runtime_manual(
                                target / profile / f"libarchive_proc.{extension}",
                                options.redis_server, run_dir, env, kind="proc")
                    report["consumers"].append(item)
                    write_json(output / "report.json", report)
        report["result"] = "passed"
        if not options.skip_runtime:
            report["redis_version"] = runner.run([options.redis_server, "--version"], root, "redis-version").strip()
    except Exception as error:
        report["result"] = "failed"
        report["error"] = str(error)
        raise
    finally:
        write_json(output / "report.json", report)
    print(f"Archive baseline passed; evidence: {output / 'report.json'}")


if __name__ == "__main__":
    try:
        main()
    except (BaselineError, subprocess.TimeoutExpired) as failure:
        print(f"error: {failure}", file=sys.stderr)
        sys.exit(1)
