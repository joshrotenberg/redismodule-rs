#!/usr/bin/env python3
"""Build disposable allocator variants from a pinned Git tree for issue 68."""

import argparse
import difflib
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[2]
VARIANTS = {
    "baseline": None,
    "system": ("wasm/src/lib.rs",
               "(redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc)",
               "(std::alloc::System, std::alloc::System)"),
    "heap": ("wasm/src/host.rs", "config.signals_based_traps(false);",
             "config.signals_based_traps(false);\n"
             "        config.memory_guard_size(0);\n"
             "        config.memory_reservation(0);\n"
             "        config.memory_reservation_for_growth(0);\n"
             "        config.memory_init_cow(false);"),
}
LOCKS = {"workspace.lock": "Cargo.lock", "guest-sdk.lock": "wasm/guest-sdk/Cargo.lock",
         "hello.lock": "wasm/guests/hello/Cargo.lock", "bench.lock": "wasm/bench/guest/Cargo.lock"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--revision", default="4546214080ff09cfb56eae062338020021a74e24")
    parser.add_argument("--source-archive", type=Path, help="Git archive for builds without Git access")
    parser.add_argument("--archive-sha256", help="Required checksum with --source-archive")
    parser.add_argument("--output", type=Path, required=True, help="New directory; must not exist")
    parser.add_argument("--target-dir", type=Path, help="Optional shared Cargo cache")
    parser.add_argument("--variants", nargs="+", choices=VARIANTS, default=list(VARIANTS))
    parser.add_argument("--jobs", type=int, default=2)
    args = parser.parse_args()
    if sys.platform not in ["darwin", "linux"]:
        parser.error("Only macOS and Linux builds are supported")
    if args.jobs < 1:
        parser.error("--jobs must be positive")
    if args.source_archive:
        if not args.archive_sha256:
            parser.error("--source-archive requires --archive-sha256")
        revision = args.revision
    else:
        revision = subprocess.check_output(["git", "rev-parse", "--verify", args.revision + "^{commit}"],
                                           cwd=ROOT, text=True).strip()
    if not re.fullmatch("[0-9a-f]{40}", revision):
        raise ValueError("Expected a resolved Git commit")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    archive = output / "source.tar"
    if args.source_archive:
        shutil.copyfile(args.source_archive, archive)
    else:
        with archive.open("wb") as stream:
            subprocess.run(["git", "archive", revision], cwd=ROOT, stdout=stream, check=True)
    archive_hash = hashlib.sha256(archive.read_bytes()).hexdigest()
    if args.archive_sha256 and archive_hash != args.archive_sha256:
        raise ValueError("Source archive checksum mismatch")
    env = dict(os.environ, CARGO_TARGET_DIR=str((args.target_dir or output / "target").resolve()))
    manifest = {"revision": revision, "archive_sha256": archive_hash, "rustc": subprocess.check_output(
        ["rustc", "--version"], cwd=ROOT, text=True).strip(), "variants": {}}
    for variant in args.variants:
        source = output / variant
        source.mkdir()
        subprocess.run(["tar", "-xf", str(archive), "-C", str(source)], check=True)
        for snapshot, destination in LOCKS.items():
            shutil.copyfile(source / "wasm/ci/locks" / snapshot, source / destination)
        patch = VARIANTS[variant]
        if patch:
            path, before, after = patch
            file = source / path
            original = file.read_text()
            if original.count(before) != 1:
                raise ValueError(f"Expected one patch anchor in {path}")
            changed = original.replace(before, after)
            file.write_text(changed)
            (output / f"{variant}.patch").write_text("".join(difflib.unified_diff(
                original.splitlines(True), changed.splitlines(True), path, path)))
        with (output / f"{variant}.build.log").open("w") as log:
            # Archives can be older than the shared cache's dep-info. Force
            # this package to rebuild while retaining dependency artifacts.
            subprocess.run(["cargo", "clean", "--release", "-p", "redis-module-wasm"],
                           cwd=source, env=env, stdout=log, stderr=log, check=True)
            subprocess.run(["cargo", "build", "--locked", "--release", "--lib", "-j",
                            str(args.jobs), "-p", "redis-module-wasm"], cwd=source,
                           env=env, stdout=log, stderr=log, check=True)
        extension = "dylib" if sys.platform == "darwin" else "so"
        artifact = output / f"{variant}.{extension}"
        # Redis requires executable module files. Keep the linker's mode.
        shutil.copy2(Path(env["CARGO_TARGET_DIR"]) / "release" /
                     f"libredis_module_wasm.{extension}", artifact)
        for snapshot, destination in LOCKS.items():
            if (source / "wasm/ci/locks" / snapshot).read_bytes() != (source / destination).read_bytes():
                raise RuntimeError(f"Pinned lock changed: {destination}")
        manifest["variants"][variant] = {"sha256": hashlib.sha256(artifact.read_bytes()).hexdigest(),
                                         "bytes": artifact.stat().st_size}
        (output / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
        print(variant, manifest["variants"][variant], flush=True)


if __name__ == "__main__":
    main()
