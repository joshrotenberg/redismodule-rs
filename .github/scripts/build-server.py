#!/usr/bin/env python3
"""Build one checksum-pinned CI server, or verify the restored executable."""

import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile


def verify(binary, kind, version, allocator):
    output = subprocess.check_output([str(binary), "--version"], text=True).strip()
    print(output, flush=True)
    if kind != "redis":
        raise RuntimeError(f"Unsupported server {kind}")
    product = "Redis"
    if not re.search(rf"^{product} server v={re.escape(version)}(?:\s|$)", output):
        raise RuntimeError(f"Expected {product} {version} from {binary}")
    allocator_pattern = rf"(?:^|\s)malloc={re.escape(allocator)}(?:-\S+)?(?:\s|$)"
    if not re.search(allocator_pattern, output):
        raise RuntimeError(f"Expected {allocator} allocator from {binary}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("server")
    parser.add_argument("destination")
    parser.add_argument("--allocator", choices=["libc", "jemalloc"], default="libc")
    args = parser.parse_args()
    server, destination, allocator = args.server, args.destination, args.allocator
    manifest = json.loads((Path(__file__).resolve().parents[1] / "servers.json").read_text())
    release = manifest[server]
    kind, version = server.split("-", 1)
    destination = Path(destination).resolve()
    binary = destination / f"{kind}-server"
    if binary.exists():
        verify(binary, kind, version, allocator)
        return

    destination.mkdir(parents=True, exist_ok=True)
    log = destination.parent / "sdk-server-build.log"
    # Only the resulting executable is cached; source and build intermediates
    # belong to this run. Keep build output separately for failure artifacts.
    with tempfile.TemporaryDirectory(prefix="sdk-server-", dir=destination.parent) as temporary:
        source = Path(temporary)
        archive = source / "release.tar.gz"
        subprocess.run(["curl", "--fail", "--location", "--retry", "3", "--output", str(archive), release["url"]], check=True)
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        if digest != release["sha256"]:
            raise RuntimeError(f"Archive checksum mismatch for {server}: {digest}")
        subprocess.run(["tar", "-xzf", str(archive), "--strip-components=1", "-C", str(source)], check=True)
        with log.open("w") as output:
            try:
                # libc is the portable default; a dedicated Linux job selects
                # bundled jemalloc to exercise defrag. Bound build parallelism.
                # Newer Redis top-level targets also build unrelated test
                # modules, so build only the executable.
                subprocess.run(["make", "-C", str(source / "src"), "-j2", f"MALLOC={allocator}", "BUILD_TLS=no", f"{kind}-server"], stdout=output, stderr=subprocess.STDOUT, check=True)
            except subprocess.CalledProcessError:
                print(log.read_text(), file=sys.stderr)
                raise
        built = source / "src" / f"{kind}-server"
        verify(built, kind, version, allocator)
        shutil.copy2(built, binary)
    verify(binary, kind, version, allocator)


if __name__ == "__main__":
    main()
