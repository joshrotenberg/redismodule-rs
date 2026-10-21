#!/usr/bin/env python3
"""Build one checksum-pinned CI server, or verify the restored executable."""

import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile


def verify(binary, kind, version):
    output = subprocess.check_output([str(binary), "--version"], text=True).strip()
    print(output, flush=True)
    product = "Redis" if kind == "redis" else "Valkey"
    if not re.search(rf"^{product} server v={re.escape(version)}(?:\s|$)", output):
        raise RuntimeError(f"Expected {product} {version} from {binary}")


def main():
    server, destination = sys.argv[1:]
    manifest = json.loads((Path(__file__).resolve().parents[1] / "servers.json").read_text())
    release = manifest[server]
    kind, version = server.split("-", 1)
    destination = Path(destination).resolve()
    binary = destination / f"{kind}-server"
    if binary.exists():
        verify(binary, kind, version)
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
                # Avoid platform-specific allocator dependencies and unbounded
                # parallelism on shared CI runners.
                subprocess.run(["make", "-C", str(source), "-j2", "MALLOC=libc", "BUILD_TLS=no"], stdout=output, stderr=subprocess.STDOUT, check=True)
            except subprocess.CalledProcessError:
                print(log.read_text(), file=sys.stderr)
                raise
        built = source / "src" / f"{kind}-server"
        verify(built, kind, version)
        shutil.copy2(built, binary)
    verify(binary, kind, version)


if __name__ == "__main__":
    main()
