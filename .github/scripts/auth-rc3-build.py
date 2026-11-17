#!/usr/bin/env python3
"""Produce two independent matching RC3 binaries, without enrolling them."""

import hashlib
import gzip
import io
import json
import os
from pathlib import Path
import platform
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import zipfile


ROOT = Path(__file__).resolve().parents[2]
DATA = ROOT / ".github/experimental-auth"
IMAGE = "docker.io/library/python@sha256:0a0704ac83aa7b896a68b710c24f9502bbae87d32e6c7ef6148ce6a60ce05260"
CONFIG = "sha256:4bbf6d52fe8de40e9e20b1453e14abb40eb1c73cd1d2b22e8ed015ad0817a088"
PIN = "a93d7d19f82dbeab1e092b072c8f93dabd8733b1"
TREE = "9baee9bed7e1d0cffa92d2705786511c1cf3361f"
ENV = {"PATH": "/usr/bin:/bin", "LC_ALL": "C", "LANG": "C", "TZ": "UTC", "SOURCE_DATE_EPOCH": "1794475640", "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_NO_REPLACE_OBJECTS": "1"}
EXTRACT = """import sys,tarfile,pathlib,os
base=pathlib.Path('/build')
with tarfile.open(fileobj=sys.stdin.buffer,mode='r|') as archive:
 for item in archive:
  name=pathlib.PurePosixPath(item.name)
  if name.is_absolute() or '..' in name.parts: raise RuntimeError('unsafe archive path')
  target=base.joinpath(*name.parts)
  if not target.resolve().is_relative_to(base): raise RuntimeError('archive escape')
  if item.isdir(): target.mkdir(parents=True,exist_ok=True)
  elif item.isfile():
   target.parent.mkdir(parents=True,exist_ok=True)
   with target.open('xb') as dest: dest.write(archive.extractfile(item).read())
   target.chmod(item.mode&0o777)
  elif item.issym():
   if not (target.parent/item.linkname).resolve().is_relative_to(base): raise RuntimeError('symlink escape')
   target.parent.mkdir(parents=True,exist_ok=True);target.symlink_to(item.linkname)
  else: raise RuntimeError('unsupported archive entry')
"""
EXPORT = """import sys,zipfile,pathlib
base=pathlib.Path('/build/evidence')
if not base.is_dir(): raise RuntimeError('missing build evidence')
with zipfile.ZipFile(sys.stdout.buffer,'w',compression=zipfile.ZIP_DEFLATED) as archive:
 for path in sorted(base.rglob('*')):
  if path.is_symlink(): raise RuntimeError('unexpected evidence symlink')
  if path.is_file(): archive.write(path,str(path.relative_to(base)))
"""


def sha(data):
    return hashlib.sha256(data).hexdigest()


def main():
    os.umask(0o022)
    if len(sys.argv) != 2:
        raise RuntimeError("usage: auth-rc3-build.py fresh-output-directory")
    for key, wanted in {"GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted", "RUNNER_OS": "Linux", "RUNNER_ARCH": "X64", "GITHUB_REPOSITORY": "joshrotenberg/redismodule-rs"}.items():
        if os.environ.get(key) != wanted:
            raise RuntimeError("existing standard native GitHub runner required: " + key)
    if platform.system() != "Linux" or platform.machine() != "x86_64":
        raise RuntimeError("emulation/host mismatch refused")
    output = Path(sys.argv[1]).resolve()
    runner_temp = Path(os.environ["RUNNER_TEMP"]).resolve()
    if runner_temp not in output.parents:
        raise RuntimeError("output outside task runner temp")
    output.mkdir(mode=0o700)
    report = {"phase": "two controlled builds", "failures": [], "builds": [], "cleanup": [], "manifest_enrolled": False}
    count = 0

    def run(label, argv, cwd=ROOT, stdin=None, timeout=60, required=True):
        nonlocal count
        count += 1
        stem = str(count).zfill(3) + "-" + label
        record = {"label": label, "argv": argv, "cwd": str(cwd)}
        with (output / (stem + ".stdout")).open("wb") as out, (output / (stem + ".stderr")).open("wb") as err:
            try:
                p = subprocess.run(argv, cwd=cwd, env=ENV, input=stdin, stdout=out, stderr=err, timeout=timeout, check=False)
                record["exit_code"] = p.returncode
            except (OSError, subprocess.TimeoutExpired) as error:
                record["error"] = str(error)
        for field in ["stdout", "stderr"]:
            p = output / (stem + "." + field)
            b = p.read_bytes()
            record[field] = {"file": p.name, "bytes": len(b), "sha256": sha(b)}
        report.setdefault("commands", []).append(record)
        (output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
        if required and record.get("exit_code") != 0:
            raise RuntimeError("command failed: " + label)
        return record, (output / (stem + ".stdout")).read_bytes()

    def git(args, **kw):
        return run("git-" + args[0], ["/usr/bin/git", "-c", "core.hooksPath=/dev/null", "-c", "core.autocrlf=false", *args], **kw)[1]

    recipe = json.loads((DATA / "rc3-bootstrap.json").read_bytes())
    if sha((DATA / "rc3-bootstrap.json").read_bytes()) != "cbd6ca31b0da01cc0dbacb159c4cfa55438b9aaab0c58bce01ca18f3ca1d827d":
        raise RuntimeError("reviewed recipe changed")
    expected_config = json.loads((DATA / "rc3-builder-config.json").read_bytes())
    if "sha256:" + sha((DATA / "rc3-builder-config.json").read_bytes()) != CONFIG:
        raise RuntimeError("configuration pin mismatch")
    docker_path = shutil.which("docker", path=os.environ.get("PATH"))
    if not docker_path:
        raise RuntimeError("existing Docker missing; installation refused")
    client = output / "docker-client"
    client.mkdir(); (client / "config.json").write_text('{"auths":{}}\n')
    docker = [docker_path, "--config", str(client), "--host", "unix:///var/run/docker.sock"]

    def drun(label, args, **kw):
        return run(label, [*docker, *args], **kw)

    image_preexisting = None
    pull_attempted = False
    container = None
    owner = os.environ["GITHUB_RUN_ID"] + "-" + os.environ["GITHUB_RUN_ATTEMPT"]
    if not re.fullmatch(r"[0-9]+-[0-9]+", owner):
        raise RuntimeError("invalid run owner")
    try:
        report["host"] = {"uname": platform.uname()._asdict(), "runner": {k: os.environ.get(k) for k in ["RUNNER_OS", "RUNNER_ARCH", "ImageOS", "ImageVersion", "SOURCE_PR_HEAD", "GITHUB_RUN_ID", "GITHUB_RUN_ATTEMPT"]}, "docker_path": docker_path, "docker_sha256": sha(Path(docker_path).resolve().read_bytes())}
        if git(["rev-parse", "HEAD"]).decode().strip() != os.environ["SOURCE_PR_HEAD"]:
            raise RuntimeError("SDK source head mismatch")
        _, raw = drun("engine", ["version", "--format", "{{json .}}"])
        server = json.loads(raw)["Server"]
        if server["Os"] != "linux" or server["Arch"] != "amd64":
            raise RuntimeError("native daemon mismatch")
        before, _ = drun("image-before", ["image", "inspect", IMAGE], required=False)
        image_preexisting = before.get("exit_code") == 0
        pull_attempted = True
        drun("pull", ["pull", "--platform", "linux/amd64", IMAGE], timeout=240)
        _, raw = drun("image", ["image", "inspect", IMAGE])
        image = json.loads(raw)[0]
        if image["Id"] != CONFIG or image["Architecture"] != "amd64" or image["Os"] != "linux" or image["RootFS"]["Layers"] != expected_config["rootfs"]["diff_ids"]:
            raise RuntimeError("image identity/layers mismatch")
        report["image"] = image
        for number in [1, 2]:
            source = output / ("source-" + str(number))
            source.mkdir()
            git(["init", "--quiet"], cwd=source)
            git(["fetch", "--depth=1", "https://github.com/redis/redis.git", PIN], cwd=source, timeout=180)
            git(["checkout", "--detach", "--quiet", PIN], cwd=source)
            if git(["rev-parse", "HEAD^{tree}"], cwd=source).decode().strip() != TREE:
                raise RuntimeError("official source tree mismatch")
            tracked = git(["ls-files", "-z"], cwd=source).decode().split("\0")[:-1]
            expected = []
            for relative in sorted(tracked):
                p = source / relative; mode = p.lstat().st_mode
                data = os.readlink(p).encode() if stat.S_ISLNK(mode) else p.read_bytes()
                expected.append({"path": relative, "kind": "symlink" if stat.S_ISLNK(mode) else "file", "mode": oct(stat.S_IMODE(mode)), "bytes": len(data), "sha256": sha(data)})
            archive_bytes = io.BytesIO()
            with tarfile.open(fileobj=archive_bytes, mode="w") as archive:
                def normalize(info):
                    info.uid = info.gid = 65534; info.uname = info.gname = ""; info.mtime = 1794475640
                    return info
                archive.add(source, arcname="redis", filter=normalize)
                for p, name in [(ROOT / ".github/scripts/auth-rc3-build-driver.py", "driver.py"), (ROOT / ".github/scripts/auth-rc3-tools.py", "tools.py"), (DATA / "rc3-bootstrap.json", "recipe.json"), (DATA / "rc3-toolchain.json", "toolchain.json")]:
                    archive.add(p, arcname=name, filter=normalize)
                b = json.dumps(expected, indent=2).encode() + b"\n"
                info = tarfile.TarInfo("source-expected.json");info.size = len(b); normalize(info);archive.addfile(info, io.BytesIO(b))
            transport = archive_bytes.getvalue()
            record = {"number": number, "source_transport_bytes": len(transport), "source_transport_sha256": sha(transport)}
            compressed = gzip.compress(transport, mtime=1794475640)
            (output / ("source-" + str(number) + ".tar.gz")).write_bytes(compressed)
            record["source_transport_gzip_sha256"] = sha(compressed)
            report["builds"].append(record)
            label = owner + "-" + str(number)
            _, created = drun("create-" + str(number), ["create", "--name", "sdk-auth-rc3-build-" + label, "--label", "sdk-auth-rc3-build-owner=" + owner, "--pull", "never", "--platform", "linux/amd64", "--network", "none", "--read-only", "--cap-drop", "ALL", "--security-opt", "no-new-privileges=true", "--pids-limit", "128", "--memory", "4g", "--cpus", "2", "--user", "65534:65534", "--tmpfs", "/build:rw,exec,nosuid,nodev,size=2147483648,uid=65534,gid=65534,mode=0755", "--workdir", "/", "--entrypoint", "/usr/bin/env", IMAGE, "-i", *[k + "=" + v for k, v in ENV.items()], "TMPDIR=/build/tmp", "PYTHONDONTWRITEBYTECODE=1", "/usr/bin/python3", "-c", "import time; time.sleep(1800)"])
            container = created.decode().strip()
            if not re.fullmatch(r"[0-9a-f]{64}", container):
                raise RuntimeError("invalid container identity")
            record["container"] = container
            drun("start-" + str(number), ["start", container])
            _, raw = drun("settings-" + str(number), ["inspect", container])
            settings = json.loads(raw)[0]
            bad_mount = any(m.get("Type") != "tmpfs" or m.get("Destination") != "/build" for m in settings["Mounts"])
            if settings["Image"] != CONFIG or bad_mount or settings["HostConfig"].get("Binds") or settings["HostConfig"]["NetworkMode"] != "none" or not settings["HostConfig"]["ReadonlyRootfs"]:
                raise RuntimeError("builder isolation mismatch")
            exec_env = ["/usr/bin/env", "-i", *[k + "=" + v for k, v in ENV.items()], "TMPDIR=/build/tmp", "PYTHONDONTWRITEBYTECODE=1"]
            drun("extract-" + str(number), ["exec", "--interactive", container, *exec_env, "/usr/bin/python3", "-c", EXTRACT], stdin=transport, timeout=120)
            outcome, _ = drun("build-" + str(number), ["exec", container, *exec_env, "/usr/bin/python3", "/build/driver.py"], timeout=1080, required=False)
            # Capture failed build evidence before removing the tmpfs container.
            _, evidence = drun("copy-" + str(number), ["exec", container, *exec_env, "/usr/bin/python3", "-c", EXPORT])
            if len(evidence) > 256 * 1024 * 1024:
                raise RuntimeError("build evidence archive length ceiling")
            destination = output / ("build-" + str(number)); destination.mkdir()
            with zipfile.ZipFile(io.BytesIO(evidence)) as archive:
                entries = archive.infolist()
                if len(entries) > 10000 or sum(i.file_size for i in entries) > 512 * 1024 * 1024 or len({i.filename for i in entries}) != len(entries):
                    raise RuntimeError("build evidence entry/expanded length ceiling")
                for item in entries:
                    path = Path(item.filename)
                    if path.is_absolute() or '..' in path.parts or ((item.external_attr >> 16) & 0o170000) == 0o120000:
                        raise RuntimeError("unsafe build evidence archive entry")
                archive.extractall(destination)
            drun("remove-" + str(number), ["rm", "--force", container]);container = None
            if outcome.get("exit_code") != 0:
                raise RuntimeError("controlled build failed; evidence retained")
        a = json.loads((output / "build-1/report.json").read_bytes())
        b = json.loads((output / "build-2/report.json").read_bytes())
        if a["status"] != "success" or b["status"] != "success" or a["artifact"] != b["artifact"] or a["version"] != b["version"] or (output / "build-1/release.h").read_bytes() != (output / "build-2/release.h").read_bytes():
            raise RuntimeError("two-build bytes or metadata mismatch; no enrollment")
        report["matching_candidate"] = {"artifact": a["artifact"], "version": a["version"], "accepted": False}
    except Exception as error:
        report["failures"].append(type(error).__name__ + ": " + str(error))
    finally:
        try:
            if container:
                drun("cleanup-container", ["rm", "--force", container])
            _, remaining = drun("cleanup-owned-check", ["ps", "--all", "--quiet", "--filter", "label=sdk-auth-rc3-build-owner=" + owner])
            if remaining.strip():
                raise RuntimeError("owned build container remains")
            report["cleanup"].append("owned build containers absent")
            if pull_attempted and image_preexisting is False:
                exists, _ = drun("cleanup-image-check", ["image", "inspect", IMAGE], required=False)
                if exists.get("exit_code") == 0:
                    drun("cleanup-image", ["image", "rm", IMAGE])
                absent, _ = drun("cleanup-image-absent", ["image", "inspect", IMAGE], required=False)
                if absent.get("exit_code") != 1:
                    raise RuntimeError("owned image removal unproved")
                report["cleanup"].append("newly pulled image reference absent")
        except Exception as error:
            report["failures"].append("cleanup: " + str(error))
        report["cleanup"].append("no daemon-wide prune; unused layers end with ephemeral runner")
        (output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    return 2 if report["failures"] else 0


if __name__ == "__main__":
    sys.exit(main())
