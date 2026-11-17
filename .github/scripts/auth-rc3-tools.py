#!/usr/bin/env python3
"""Read-only inventory inside the pinned native Linux amd64 builder.

Executed from stdin with the recipe PATH. This script never compiles, installs,
starts Redis, selects an artifact digest or grants runtime admission.
"""

import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import stat
import subprocess
import sys
import time


MAX_OUTPUT = 512 * 1024
MAX_INVENTORY_FILES = 50000
MAX_INVENTORY_BYTES = 512 * 1024 * 1024
STARTED = time.monotonic()
FAILURES = []
COMMANDS = []
FILES = {}
TOOLS = {}

# Explicit metadata-only argv. No source files or compiler -c/-E/-S input.
TOOL_ARGV = {
    "gcc": ["--version"],
    "g++": ["--version"],
    "cc": ["--version"],
    "ld": ["--version"],
    "as": ["--version"],
    "ar": ["--version"],
    "ranlib": ["--version"],
    "nm": ["--version"],
    "objcopy": ["--version"],
    "strip": ["--version"],
    "make": ["--version"],
    "git": ["--version"],
    "sh": None,  # dash has no version flag; hash and package ownership identify it.
    "date": ["--version"],
    "sha256sum": ["--version"],
    "readelf": ["--version"],
    "python3": ["--version"],
    "dpkg-query": ["--version"],
    "ldd": ["--version"],
    "ldconfig": None,  # /sbin is deliberately outside the recipe PATH.
    "env": ["--version"],
    "uname": ["--version"],
    "file": ["--version"],
}


def fail(message):
    FAILURES.append(message)


def check_time():
    if time.monotonic() - STARTED > 180:
        raise RuntimeError("inventory elapsed-time ceiling")


def command(argv, required=True):
    check_time()
    result = {"argv": argv}
    try:
        p = subprocess.run(argv, capture_output=True, timeout=10, check=False)
        result.update(exit_code=p.returncode)
        for field, value in [("stdout", p.stdout), ("stderr", p.stderr)]:
            result[field] = value[:MAX_OUTPUT].decode("utf-8", "replace")
            result[field + "_bytes"] = len(value)
            result[field + "_sha256"] = hashlib.sha256(value).hexdigest()
            if len(value) > MAX_OUTPUT:
                fail("metadata output ceiling: " + repr(argv))
        if required and p.returncode:
            fail("metadata command failed: " + repr(argv))
    except (OSError, subprocess.TimeoutExpired) as error:
        result["error"] = type(error).__name__ + ": " + str(error)
        if required:
            fail("metadata command unavailable: " + repr(argv))
    COMMANDS.append(result)
    return result


def file_record(path):
    check_time()
    path = Path(path)
    key = str(path)
    if key in FILES:
        return FILES[key]
    result = {"path": key, "symlink": os.readlink(path) if path.is_symlink() else None}
    FILES[key] = result
    try:
        real = path.resolve(strict=True)
        before = real.stat()
        if not stat.S_ISREG(before.st_mode):
            raise RuntimeError("not a regular file")
        digest = hashlib.sha256()
        with real.open("rb") as stream:
            prefix = stream.read(4)
            digest.update(prefix)
            total = len(prefix)
            while block := stream.read(1024 * 1024):
                check_time()
                digest.update(block)
                total += len(block)
                if total > MAX_INVENTORY_BYTES:
                    raise RuntimeError("individual file length ceiling")
        after = real.stat()
        fields = ("st_dev", "st_ino", "st_size", "st_mtime_ns", "st_ctime_ns")
        if any(getattr(before, f) != getattr(after, f) for f in fields):
            raise RuntimeError("file changed during inventory")
        result.update(
            realpath=str(real),
            size=total,
            sha256=digest.hexdigest(),
            mode=oct(stat.S_IMODE(before.st_mode)),
            elf=prefix == b"\x7fELF",
        )
    except (OSError, RuntimeError) as error:
        result["error"] = str(error)
        fail("file inventory failed: " + key)
    return result


def locate(name):
    path = shutil.which(name)
    TOOLS[name] = {"resolved": path, "missing": path is None, "required_in_path": name != "ldconfig"}
    return path


def main():
    if platform.system() != "Linux" or platform.machine() != "x86_64":
        raise RuntimeError("requires native Linux x86_64")
    if os.environ.get("PATH") != "/usr/bin:/bin":
        raise RuntimeError("recipe PATH mismatch")
    if os.getuid() != 65534:
        raise RuntimeError("inventory must use the declared unprivileged UID")

    for name, args in TOOL_ARGV.items():
        path = locate(name)
        # Record absent /sbin tools honestly; inspect their fixed image path
        # separately, without changing the recipe PATH or using a fallback.
        if name == "ldconfig":
            continue
        if path is None:
            fail("required tool missing from recipe PATH: " + name)
            continue
        file_record(path)
        if args is not None:
            command([path, *args])

    extras = {}
    for name, path, args in [
        ("image_python3", "/usr/local/bin/python3", ["--version"]),
        ("image_ldconfig", "/sbin/ldconfig", ["-V"]),
    ]:
        extras[name] = {"path": path, "exists": Path(path).exists()}
        if Path(path).exists():
            extras[name]["file"] = file_record(path)
            command([path, *args])
        elif name == "image_ldconfig":
            fail("required explicit image path missing: " + path)

    gcc = TOOLS.get("gcc", {}).get("resolved")
    gxx = TOOLS.get("g++", {}).get("resolved")
    compiler_queries = {}
    for driver in filter(None, [gcc, gxx]):
        compiler_queries[driver] = {}
        for option in ["-dumpmachine", "-dumpfullversion", "-dumpversion", "-print-search-dirs", "-dumpspecs"]:
            compiler_queries[driver][option] = command([driver, option])
        machine = compiler_queries[driver]["-dumpmachine"].get("stdout", "").strip()
        if machine != "x86_64-linux-gnu":
            fail("unexpected compiler target: " + machine)
        for program in ["cc1", "cc1plus", "collect2", "lto-wrapper", "lto1", "as", "ld"]:
            result = command([driver, "-print-prog-name=" + program])
            value = result.get("stdout", "").strip()
            path = value if value.startswith("/") else shutil.which(value)
            result["resolved_subprogram"] = path
            if path:
                file_record(path)
            else:
                fail("compiler subprogram missing: " + program)
        for library in ["libgcc.a", "libgcc_s.so", "libstdc++.so", "libc.so"]:
            result = command([driver, "-print-file-name=" + library])
            value = result.get("stdout", "").strip()
            if value.startswith("/") and Path(value).is_file():
                file_record(value)
            else:
                fail("compiler library missing: " + library)

    package_inventory = None
    package_owners = {}
    dpkg = TOOLS.get("dpkg-query", {}).get("resolved")
    if dpkg:
        package_inventory = command([dpkg, "-W", "-f=${binary:Package}\t${Version}\t${Architecture}\n"])
    readelf = TOOLS.get("readelf", {}).get("resolved")
    ldd = TOOLS.get("ldd", {}).get("resolved")
    dependencies = {}
    # A fixed snapshot prevents recursion through the newly discovered libraries.
    elf_inputs = sorted({r["realpath"] for r in FILES.values() if r.get("elf")})
    for path in elf_inputs:
        record = {}
        if readelf:
            record["program_headers"] = command([readelf, "--program-headers", path])
            record["dynamic"] = command([readelf, "--dynamic", path])
        if ldd:
            linked = command([ldd, path], required=False)
            record["ldd"] = linked
            text = linked.get("stdout", "") + linked.get("stderr", "")
            if "not found" in text:
                fail("missing dynamic dependency: " + path)
            for lib in re.findall(r"(?:=>\s*)?(/[A-Za-z0-9_+./-]+)\s+\(", text):
                file_record(lib)
        dependencies[path] = record
    if dpkg:
        for path in sorted({r["realpath"] for r in FILES.values() if "realpath" in r}):
            # A non-owned path is recorded, not disguised as another package.
            package_owners[path] = command([dpkg, "-S", path], required=False)
    if Path("/sbin/ldconfig").exists():
        command(["/sbin/ldconfig", "-p"])

    headers = []
    total = 0
    for base in [Path("/usr/include"), Path("/usr/lib/gcc")]:
        if not base.is_dir():
            fail("required compiler input tree missing: " + str(base))
            continue
        for path in sorted(base.rglob("*")):
            check_time()
            if path.is_file():
                record = file_record(path)
                headers.append(str(path))
                total += record.get("size", 0)
                if len(headers) > MAX_INVENTORY_FILES or total > MAX_INVENTORY_BYTES:
                    raise RuntimeError("compiler input inventory ceiling")

    return {
        "schema_version": 1,
        "purpose": "read-only tool provenance; no compilation/runtime/enrollment",
        "system": platform.uname()._asdict(),
        "uid": os.getuid(),
        "python_executable": sys.executable,
        "python_version": sys.version,
        "environment": dict(os.environ),
        "tools": TOOLS,
        "explicit_image_paths": extras,
        "compiler_queries": compiler_queries,
        "packages": package_inventory,
        "package_owners": package_owners,
        "dynamic_dependencies": dependencies,
        "compiler_input_files": headers,
        "compiler_input_bytes": total,
    }


if __name__ == "__main__":
    result = {}
    try:
        result = main()
    except Exception as error:
        fail(type(error).__name__ + ": " + str(error))
    result.update(files=FILES, commands=COMMANDS, failures=FAILURES, elapsed_seconds=time.monotonic() - STARTED)
    print(json.dumps(result, indent=2))
    sys.exit(2 if FAILURES else 0)
