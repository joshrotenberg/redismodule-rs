#!/usr/bin/env python3
"""Pull and inventory one pinned image on an existing GitHub Linux x64 runner.

No build, package installation, source mount, Redis process or artifact
enrollment is performed. The independent pre-execution gate covers this file,
auth-rc3-tools.py, the workflow, recipe and immutable OCI metadata.
"""

import datetime
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import sys
import time


IMAGE = "docker.io/library/python@sha256:0a0704ac83aa7b896a68b710c24f9502bbae87d32e6c7ef6148ce6a60ce05260"
CONFIG = "sha256:4bbf6d52fe8de40e9e20b1453e14abb40eb1c73cd1d2b22e8ed015ad0817a088"
ROOT = Path(__file__).resolve().parents[2]
DATA = ROOT / ".github/experimental-auth"
SOURCE_FILES = [
    ".github/scripts/auth-rc3-inventory.py",
    ".github/scripts/auth-rc3-tools.py",
    ".github/workflows/experimental-auth-inventory.yml",
    ".github/experimental-auth/rc3-bootstrap.json",
    ".github/experimental-auth/rc3-builder-manifest.json",
    ".github/experimental-auth/rc3-builder-config.json",
]
CONTAINER_ARGV = [
    "-i",
    "PATH=/usr/bin:/bin",
    "LC_ALL=C",
    "LANG=C",
    "TZ=UTC",
    "SOURCE_DATE_EPOCH=1794475640",
    "PYTHONDONTWRITEBYTECODE=1",
    "/bin/sh",
    "-s",
]
CHILD_ENV = {"PATH": "/usr/bin:/bin", "LC_ALL": "C", "LANG": "C"}


def sha(data):
    return hashlib.sha256(data).hexdigest()


def utc():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


def main():
    if len(sys.argv) != 2:
        raise RuntimeError("usage: auth-rc3-inventory.py fresh-output-directory")
    expected = {"GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted", "RUNNER_OS": "Linux", "RUNNER_ARCH": "X64"}
    if any(os.environ.get(key) != value for key, value in expected.items()):
        raise RuntimeError("requires an existing standard GitHub-hosted Linux X64 runner")
    if platform.system() != "Linux" or platform.machine() != "x86_64":
        raise RuntimeError("host architecture mismatch; emulation is not supported")
    if os.environ.get("GITHUB_REPOSITORY") != "joshrotenberg/redismodule-rs":
        raise RuntimeError("unexpected source repository")
    runner_temp = Path(os.environ["RUNNER_TEMP"]).resolve(strict=True)
    output = Path(sys.argv[1]).resolve()
    if output == runner_temp or runner_temp not in output.parents:
        raise RuntimeError("output must be a fresh directory inside RUNNER_TEMP")
    output.mkdir(mode=0o700)
    report = {
        "schema_version": 1,
        "started_at_utc": utc(),
        "phase": "pull and read-only inventory only",
        "image": IMAGE,
        "container_argv": CONTAINER_ARGV,
        "source_files": {},
        "commands": [],
        "failures": [],
        "cleanup": {},
    }
    (output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    docker = None
    container = None
    container_name = None
    owner = None
    image_was_present = None
    pull_attempted = False

    def run(name, argv, timeout=30, input_bytes=None, required=True):
        # Fixed argv and trusted metadata tools only. Logs remain outside the
        # container; neither source nor an output directory is host-mounted.
        item = {"name": name, "argv": argv, "started_at_utc": utc()}
        report["commands"].append(item)
        out = output / (name + ".stdout")
        err = output / (name + ".stderr")
        with out.open("wb") as stdout, err.open("wb") as stderr:
            try:
                result = subprocess.run(argv, input=input_bytes, stdout=stdout, stderr=stderr, env=CHILD_ENV, timeout=timeout, check=False)
                item["exit_code"] = result.returncode
            except (OSError, subprocess.TimeoutExpired) as error:
                item["error"] = type(error).__name__ + ": " + str(error)
        for field, path in [("stdout", out), ("stderr", err)]:
            data = path.read_bytes()
            item[field] = {"file": path.name, "bytes": len(data), "sha256": sha(data)}
            if len(data) > 32 * 1024 * 1024:
                raise RuntimeError("inventory log ceiling: " + name)
        item["finished_at_utc"] = utc()
        if required and item.get("exit_code") != 0:
            raise RuntimeError("inventory command failed: " + name)
        return item, out.read_bytes()

    def drun(name, args, **kwargs):
        return run(name, [*docker, *args], **kwargs)

    try:
        for relative in SOURCE_FILES:
            path = ROOT / relative
            if path.is_symlink():
                raise RuntimeError("source symlink refused: " + relative)
            data = path.read_bytes()
            report["source_files"][relative] = {"bytes": len(data), "sha256": sha(data)}
        recipe = json.loads((DATA / "rc3-bootstrap.json").read_bytes())
        manifest_bytes = (DATA / "rc3-builder-manifest.json").read_bytes()
        config_bytes = (DATA / "rc3-builder-config.json").read_bytes()
        manifest = json.loads(manifest_bytes)
        config = json.loads(config_bytes)
        if recipe["builder"]["execution_reference"] != IMAGE or recipe["builder"]["config_digest"] != CONFIG:
            raise RuntimeError("recipe image pin mismatch")
        if "sha256:" + sha(manifest_bytes) != IMAGE.split("@", 1)[1] or "sha256:" + sha(config_bytes) != CONFIG:
            raise RuntimeError("immutable metadata digest mismatch")
        if manifest["config"]["digest"] != CONFIG or manifest["layers"] != recipe["builder"]["layers"]:
            raise RuntimeError("manifest descriptor mismatch")
        if config["os"] != "linux" or config["architecture"] != "amd64":
            raise RuntimeError("pinned configuration platform mismatch")
        if recipe["runtime_admission"] != {"enabled": False, "approved_executable_entries": [], "policy": "This recipe is build provenance input only. It never supplies a runtime expected digest and never enrolls a result automatically."}:
            raise RuntimeError("inventory cannot enable runtime admission")
        if any(recipe["expected_artifact"][key] is not None for key in ["length", "sha256", "redis_build_id"]):
            raise RuntimeError("inventory phase requires an empty artifact manifest")
        if recipe["controlled_environment"]["environment_allowlist"]["PATH"] != "/usr/bin:/bin":
            raise RuntimeError("recipe PATH mismatch")

        report["host"] = {
            "uname": platform.uname()._asdict(),
            "python": {"executable": sys.executable, "version": sys.version, "sha256": sha(Path(sys.executable).resolve().read_bytes())},
            "runner": {key: os.environ.get(key) for key in [*expected, "RUNNER_NAME", "ImageOS", "ImageVersion", "GITHUB_REPOSITORY", "GITHUB_RUN_ID", "GITHUB_RUN_ATTEMPT", "GITHUB_SHA", "SOURCE_PR_HEAD"]},
        }
        for path in ["/etc/os-release", "/proc/version", "/proc/cpuinfo"]:
            data = Path(path).read_bytes()
            if len(data) > 1024 * 1024:
                raise RuntimeError("host metadata ceiling")
            (output / (Path(path).name + ".txt")).write_bytes(data)
        run("host-uname", ["/usr/bin/uname", "-a"])
        run("host-cpu", ["/usr/bin/lscpu", "--json"])
        _, head = run("source-head", ["/usr/bin/git", "-C", str(ROOT), "rev-parse", "HEAD"])
        if head.decode().strip() != os.environ.get("SOURCE_PR_HEAD"):
            raise RuntimeError("checkout does not match the requested exact PR head")
        docker_path = shutil.which("docker", path=os.environ.get("PATH"))
        if docker_path is None:
            raise RuntimeError("existing Docker client missing; installation is refused")
        report["host"]["docker"] = {"resolved": docker_path, "realpath": str(Path(docker_path).resolve()), "sha256": sha(Path(docker_path).resolve().read_bytes())}
        docker_config = output / "docker-client"
        docker_config.mkdir(mode=0o700)
        (docker_config / "config.json").write_text('{"auths":{}}\n')
        # Explicit existing local socket. No inherited context, login helper,
        # remote daemon, engine startup or daemon socket inside the container.
        docker = [docker_path, "--config", str(docker_config), "--host", "unix:///var/run/docker.sock"]
        _, version_bytes = drun("engine-version", ["version", "--format", "{{json .}}"])
        _, info_bytes = drun("engine-info", ["info", "--format", "{{json .}}"])
        version = json.loads(version_bytes)
        info = json.loads(info_bytes)
        if version["Server"]["Os"] != "linux" or version["Server"]["Arch"] != "amd64" or info["OSType"] != "linux" or info["Architecture"] not in ["x86_64", "amd64"]:
            raise RuntimeError("existing daemon is not native Linux amd64")
        pre, _ = drun("image-before", ["image", "inspect", IMAGE], required=False)
        image_was_present = pre.get("exit_code") == 0
        report["image_was_present"] = image_was_present
        # The only networked image operation. No mutable tag or repair path.
        pull_attempted = True
        drun("image-pull", ["pull", "--platform", "linux/amd64", IMAGE], timeout=240)
        _, image_bytes = drun("image-after", ["image", "inspect", IMAGE])
        image = json.loads(image_bytes)
        if len(image) != 1:
            raise RuntimeError("unexpected image inspect cardinality")
        image = image[0]
        if image["Id"] != CONFIG or image["Os"] != "linux" or image["Architecture"] != "amd64":
            raise RuntimeError("pulled image configuration mismatch")
        if image["RootFS"]["Layers"] != config["rootfs"]["diff_ids"]:
            raise RuntimeError("pulled image layer identity mismatch")
        expected_names = ["python", "library/python", "docker.io/library/python"]
        if not any(value.split("@", 1) == [name, IMAGE.split("@", 1)[1]] for value in image["RepoDigests"] for name in expected_names):
            raise RuntimeError("pulled image repository digest mismatch")
        report["verified_image"] = {"id": image["Id"], "repo_digests": image["RepoDigests"], "compressed_layers": manifest["layers"], "rootfs_diff_ids": image["RootFS"]["Layers"], "config_sha256": sha(config_bytes)}

        run_id = os.environ["GITHUB_RUN_ID"]
        attempt = os.environ["GITHUB_RUN_ATTEMPT"]
        if not re.fullmatch(r"[0-9]+", run_id) or not re.fullmatch(r"[0-9]+", attempt):
            raise RuntimeError("invalid run identity")
        owner = run_id + "-" + attempt
        container_name = "sdk-auth-rc3-inventory-" + owner
        create = [
            "create", "--interactive", "--name", container_name, "--pull", "never",
            "--label", "sdk-auth-rc3-inventory-owner=" + owner,
            "--platform", "linux/amd64", "--network", "none", "--read-only",
            "--cap-drop", "ALL", "--security-opt", "no-new-privileges=true",
            "--pids-limit", "64", "--memory", "512m", "--cpus", "2",
            "--user", "65534:65534", "--workdir", "/",
            "--entrypoint", "/usr/bin/env", IMAGE, *CONTAINER_ARGV,
        ]
        _, created = drun("container-create", create)
        container = created.decode().strip()
        if not re.fullmatch(r"[0-9a-f]{64}", container):
            raise RuntimeError("invalid owned container identity")
        report["owned_container"] = container
        _, before_bytes = drun("container-before", ["inspect", container])
        before = json.loads(before_bytes)[0]
        if before["Image"] != CONFIG or before["Mounts"] or before["HostConfig"].get("Binds") or before["HostConfig"]["NetworkMode"] != "none" or not before["HostConfig"]["ReadonlyRootfs"]:
            raise RuntimeError("container isolation mismatch")
        if before["Config"]["Entrypoint"] != ["/usr/bin/env"] or before["Config"]["Cmd"] != CONTAINER_ARGV or before["Config"]["Labels"].get("sdk-auth-rc3-inventory-owner") != owner:
            raise RuntimeError("container command/ownership mismatch")
        tools = (ROOT / ".github/scripts/auth-rc3-tools.py").read_bytes()
        marker = b"AUTH_RC3_PINNED_TOOL_INVENTORY"
        if marker in tools:
            raise RuntimeError("inventory stdin delimiter collision")
        script = b"set -eu\numask 022\ncommand -v python3 >&2 || { printf 'missing python3 under recipe PATH\\n' >&2; exit 2; }\nexec python3 - <<'" + marker + b"'\n" + tools + b"\n" + marker + b"\n"
        report["inventory_stdin"] = {"bytes": len(script), "sha256": sha(script)}
        (output / "inventory-stdin.sh").write_bytes(script)
        _, inventory_bytes = drun("container-inventory", ["start", "--attach", "--interactive", container], timeout=240, input_bytes=script)
        _, after_bytes = drun("container-after", ["inspect", container])
        after = json.loads(after_bytes)[0]
        if after["State"]["Running"] or after["State"]["ExitCode"] != 0:
            raise RuntimeError("inventory container did not exit successfully")
        inventory = json.loads(inventory_bytes)
        if inventory.get("failures") or inventory.get("environment", {}).get("PATH") != "/usr/bin:/bin":
            raise RuntimeError("actual tool inventory refused; no repair/fallback")
        report["inventory_success"] = True
    except Exception as error:
        report["failures"].append(type(error).__name__ + ": " + str(error))
    finally:
        if docker is not None:
            try:
                # Recover ownership even if create returned a truncated/error
                # response after the daemon created our labelled container.
                if not container and container_name:
                    found, found_bytes = drun("container-recover", ["inspect", container_name], required=False)
                    if found.get("exit_code") == 0:
                        recovered = json.loads(found_bytes)[0]
                        if recovered["Config"]["Labels"].get("sdk-auth-rc3-inventory-owner") != owner:
                            raise RuntimeError("container name belongs to another owner")
                        container = recovered["Id"]
                if container and re.fullmatch(r"[0-9a-f]{64}", container):
                    drun("container-remove", ["rm", "--force", container])
                    _, remaining = drun("container-remaining", ["ps", "--all", "--no-trunc", "--quiet", "--filter", "id=" + container])
                    report["cleanup"]["container_absent"] = not remaining.strip()
                    if remaining.strip():
                        raise RuntimeError("owned inventory container remains")
                if pull_attempted and image_was_present is False:
                    present, _ = drun("image-cleanup-check", ["image", "inspect", IMAGE], required=False)
                    if present.get("exit_code") == 0:
                        drun("image-remove", ["image", "rm", IMAGE])
                    elif present.get("exit_code") != 1:
                        raise RuntimeError("owned image state unproved after pull")
                    state, _ = drun("image-remaining", ["image", "inspect", IMAGE], required=False)
                    drun("engine-after-cleanup", ["info", "--format", "{{json .ServerVersion}}"])
                    report["cleanup"]["owned_image_reference_absent"] = state.get("exit_code") == 1
                    if state.get("exit_code") != 1:
                        raise RuntimeError("owned image reference removal unproved")
                else:
                    report["cleanup"]["preexisting_image_retained"] = image_was_present
                report["cleanup"]["layer_cache"] = "No daemon-wide prune. Content-addressed unused/partial layer storage ends with this ephemeral GitHub runner."
            except Exception as error:
                report["failures"].append("cleanup: " + str(error))
        report["finished_at_utc"] = utc()
        report["native_build_runtime_enrollment"] = False
        report["next_gate"] = "Independent actual inventory acceptance and separate build release."
        (output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    return 2 if report["failures"] else 0


if __name__ == "__main__":
    sys.exit(main())
