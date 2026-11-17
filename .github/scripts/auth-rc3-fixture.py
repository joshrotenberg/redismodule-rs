#!/usr/bin/env python3
"""Install an already reviewed exact binary for the gated positive CI lane.

This never discovers/enrolls a digest. It compares the fixed public witness
archive and binary with committed expected values. The SDK independently
measures its running image during ordinary-command preparation.
"""

import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import sys
import zipfile


ROOT = Path(__file__).resolve().parents[2]


def sha(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        while data := stream.read(1024 * 1024):
            digest.update(data)
    return digest.hexdigest()


def main():
    if platform.system() != "Linux" or platform.machine() != "x86_64":
        raise RuntimeError("positive fixture requires native Linux x86_64")
    for key, value in {"GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted", "RUNNER_OS": "Linux", "RUNNER_ARCH": "X64", "GITHUB_REPOSITORY": "joshrotenberg/redismodule-rs"}.items():
        if os.environ.get(key) != value:
            raise RuntimeError("unexpected positive runner: " + key)
    if len(sys.argv) != 2:
        raise RuntimeError("usage: auth-rc3-fixture.py fresh-runner-temp-directory")
    output = Path(sys.argv[1]).resolve()
    if Path(os.environ["RUNNER_TEMP"]).resolve() not in output.parents:
        raise RuntimeError("fixture output outside runner temp")
    output.mkdir(mode=0o700)
    manifest = json.loads((ROOT / ".github/experimental-auth/accepted-runtime.json").read_bytes())
    artifact = manifest["artifact"]
    gh = shutil.which("gh")
    if gh is None:
        raise RuntimeError("existing GitHub client missing; no install/fallback")
    # These identify transport produced by this same workflow. They never
    # supply an expected executable digest; that remains the reviewed literal.
    identity = int(os.environ["SDK_ASYNC_ARTIFACT_ID"])
    archive_digest = os.environ["SDK_ASYNC_ARTIFACT_DIGEST"]
    if identity <= 0 or not re.fullmatch(r"[0-9a-f]{64}", archive_digest):
        raise RuntimeError("invalid current-workflow witness transport")
    archive = output / "reviewed-builds.zip"
    argv = [gh, "api", "repos/joshrotenberg/redismodule-rs/actions/artifacts/" + str(identity) + "/zip"]
    with archive.open("wb") as stream:
        subprocess.run(argv, stdout=stream, check=True, timeout=120)
    if archive.stat().st_size > 128 * 1024 * 1024 or sha(archive) != archive_digest:
        raise RuntimeError("reviewed witness archive mismatch")
    with zipfile.ZipFile(archive) as zipped:
        member = zipped.getinfo("build-1/redis-server")
        if member.file_size != artifact["bytes"]:
            raise RuntimeError("reviewed binary length mismatch")
        binary = output / "redis-server"
        with zipped.open(member) as source, binary.open("xb") as dest:
            shutil.copyfileobj(source, dest)
    if sha(binary) != artifact["sha256"]:
        raise RuntimeError("reviewed binary digest mismatch")
    binary.chmod(0o755)
    version = subprocess.check_output([str(binary), "--version"], timeout=10, text=True).strip()
    expected = manifest["expected_native"]
    if version != "Redis server v=" + expected["redis_version"] + " sha=" + expected["redis_git_sha1"] + ":" + expected["redis_git_dirty"] + " malloc=libc bits=64 build=" + expected["redis_build_id"]:
        raise RuntimeError("reviewed binary native metadata mismatch")
    report = {"native_host": platform.uname()._asdict(), "binary": str(binary), "bytes": binary.stat().st_size, "sha256": sha(binary), "version": version, "source_manifest_sha256": sha(ROOT / ".github/experimental-auth/accepted-runtime.json"), "archive_sha256": sha(archive), "download_argv": argv, "gh_binary": {"path": gh, "sha256": sha(Path(gh).resolve())}, "sdk_head": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()}
    (output / "fixture.json").write_text(json.dumps(report, indent=2) + "\n")
    evidence = output / "runtime"
    evidence.mkdir()
    with Path(os.environ["GITHUB_ENV"]).open("a") as env:
        env.write("TEST_REDIS_SERVER_BIN=" + str(binary) + "\n")
        env.write("TEST_ASYNC_AUTH_EVIDENCE_DIR=" + str(evidence) + "\n")


if __name__ == "__main__":
    main()
