#!/usr/bin/env python3
"""Two-build witness driver, inside a fresh offline pinned builder only."""

import hashlib
import json
import os
from pathlib import Path
import platform
import stat
import subprocess


ROOT = Path("/build/redis")
OUT = Path("/build/evidence")
PIN = "a93d7d19f82dbeab1e092b072c8f93dabd8733b1"
TREE = "9baee9bed7e1d0cffa92d2705786511c1cf3361f"


def digest(data):
    return hashlib.sha256(data).hexdigest()


def run(name, argv, cwd=ROOT, timeout=30):
    with (OUT / (name + ".stdout")).open("wb") as stdout, (OUT / (name + ".stderr")).open("wb") as stderr:
        p = subprocess.run(argv, cwd=cwd, stdout=stdout, stderr=stderr, timeout=timeout, check=False)
    if p.returncode:
        raise RuntimeError("command failed: " + name)
    return (OUT / (name + ".stdout")).read_bytes()


def inventory(paths):
    result = []
    for relative in sorted(paths):
        p = ROOT / relative
        mode = p.lstat().st_mode
        if stat.S_ISLNK(mode):
            data = os.readlink(p).encode()
            kind = "symlink"
        elif stat.S_ISREG(mode):
            data = p.read_bytes()
            kind = "file"
        else:
            raise RuntimeError("unexpected source entry: " + relative)
        result.append({"path": relative, "kind": kind, "mode": oct(stat.S_IMODE(mode)), "bytes": len(data), "sha256": digest(data)})
    return result


def clean(name):
    for suffix, argv in [
        ("index", ["/usr/bin/git", "diff", "--cached", "--exit-code", "--no-ext-diff", "HEAD", "--"]),
        ("worktree", ["/usr/bin/git", "diff", "--exit-code", "--no-ext-diff", "--"]),
    ]:
        run(name + "-" + suffix, argv)


def main():
    os.umask(0o022)
    OUT.mkdir()
    Path("/build/tmp").mkdir()
    report = {"phase": "controlled build only", "umask": "022", "native_server_run": False, "manifest_enrolled": False}
    try:
        if platform.system() != "Linux" or platform.machine() != "x86_64" or os.getuid() != 65534:
            raise RuntimeError("native Linux amd64 unprivileged builder required")
        recipe = json.loads(Path("/build/recipe.json").read_bytes())
        expected = json.loads(Path("/build/toolchain.json").read_bytes())
        wanted_env = recipe["controlled_environment"]["environment_allowlist"]
        for key, value in wanted_env.items():
            if os.environ.get(key) != value:
                raise RuntimeError("environment mismatch: " + key)
        for key in ["CFLAGS", "LDFLAGS", "MAKEFLAGS", "LD_PRELOAD", "LD_LIBRARY_PATH", "CCACHE_DIR"]:
            if key in os.environ:
                raise RuntimeError("unexpected inherited build input: " + key)
        actual_tools = run("tool-inventory", ["/usr/bin/python3", "/build/tools.py"], cwd=Path("/"), timeout=240)
        tools = json.loads(actual_tools)
        if tools["failures"]:
            raise RuntimeError("tool inventory failure")
        files_digest = digest(json.dumps(tools["files"], sort_keys=True, separators=(",", ":")).encode())
        packages_digest = digest(tools["packages"]["stdout"].encode())
        if files_digest != expected["files_canonical_sha256"] or packages_digest != expected["packages_stdout_sha256"]:
            raise RuntimeError("accepted compiler/library/header/package inventory changed")
        report["tool_inventory"] = {"files_canonical_sha256": files_digest, "packages_stdout_sha256": packages_digest}
        if run("head", ["/usr/bin/git", "rev-parse", "HEAD"]).decode().strip() != PIN:
            raise RuntimeError("source commit mismatch")
        if run("tree", ["/usr/bin/git", "rev-parse", "HEAD^{tree}"]).decode().strip() != TREE:
            raise RuntimeError("source tree mismatch")
        tracked = run("tracked", ["/usr/bin/git", "ls-files", "-z"]).decode().split("\0")[:-1]
        clean("before")
        for name, args in [("untracked-before", ["--others", "--exclude-standard"]), ("ignored-before", ["--others", "--ignored", "--exclude-standard"])]:
            if run(name, ["/usr/bin/git", "ls-files", "-z", *args]):
                raise RuntimeError("preexisting untracked/ignored build input")
        before = inventory(tracked)
        (OUT / "source-before.json").write_text(json.dumps(before, indent=2) + "\n")
        host_expected = json.loads(Path("/build/source-expected.json").read_bytes())
        if before != host_expected:
            raise RuntimeError("source transport inventory mismatch")
        run("git-config", ["/usr/bin/git", "config", "--list", "--show-origin"])
        run("git-index", ["/usr/bin/git", "ls-files", "--stage", "-z"])
        argv = recipe["controlled_environment"]["make_argv"]
        (OUT / "make-argv.json").write_text(json.dumps(argv, indent=2) + "\n")
        run("make", argv, cwd=ROOT / "src", timeout=900)
        clean("after")
        after = inventory(tracked)
        (OUT / "source-after.json").write_text(json.dumps(after, indent=2) + "\n")
        if before != after:
            raise RuntimeError("tracked source bytes or modes changed during build")
        generated_paths = run("generated-paths", ["/usr/bin/git", "ls-files", "-z", "--others", "--ignored", "--exclude-standard"]).decode().split("\0")[:-1]
        untracked_paths = run("untracked-after", ["/usr/bin/git", "ls-files", "-z", "--others", "--exclude-standard"]).decode().split("\0")[:-1]
        (OUT / "generated.json").write_text(json.dumps(inventory(sorted(set(generated_paths + untracked_paths))), indent=2) + "\n")
        for name, evidence_name in [("release.h", "release.h"), (".make-settings", "make-settings.txt")]:
            (OUT / evidence_name).write_bytes((ROOT / "src" / name).read_bytes())
        binary = ROOT / "src/redis-server"
        data = binary.read_bytes()
        if data[:6] != b"\x7fELF\x02\x01" or int.from_bytes(data[18:20], "little") != 62:
            raise RuntimeError("unexpected artifact ELF identity")
        (OUT / "redis-server").write_bytes(data)
        os.chmod(OUT / "redis-server", 0o755)
        version = run("binary-version", [str(binary), "--version"], cwd=ROOT / "src").decode().strip()
        if not version.startswith("Redis server v=8.11.242 sha=a93d7d19:0 malloc=libc bits=64 build="):
            raise RuntimeError("unexpected built-binary metadata")
        report.update(status="success", version=version, source_commit=PIN, source_tree=TREE, environment=dict(os.environ), artifact={"bytes": len(data), "sha256": digest(data), "elf": "ELF64 little-endian EM_X86_64"})
    except Exception as error:
        report.update(status="failure", error=type(error).__name__ + ": " + str(error))
    (OUT / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report))
    return 0 if report["status"] == "success" else 2


if __name__ == "__main__":
    raise SystemExit(main())
