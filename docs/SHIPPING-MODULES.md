# Shipping a native module

Distribute the module's `cdylib` together with its source revision, build inputs,
checksums and tested server/platform combinations. For replacing a loaded module,
see [Upgrading a loaded module](UPGRADING-MODULES.md); for SDK source changes,
see [Migrating to 3.0](MIGRATING-3.0.md).

## Choose the deployment target

A Redis compatibility floor selects module APIs; it does not select the CPU,
operating system, libc or shared libraries. Name artifacts with the module
version, source revision and Rust target, and record additional platform limits.

- Match the Redis process architecture, including when using an emulated shell.
  A macOS `.dylib` cannot be deployed as a Linux `.so` by renaming it.
- On Linux, distinguish GNU/glibc from musl. Build in a deployment-compatible
  environment and inspect shared-library dependencies and symbol versions, for
  example with `readelf -d` and `readelf --version-info`. A GNU target name alone
  does not establish the oldest glibc that the finished module can load against.
- On macOS, record the deployment target and linked libraries (`otool -L` and
  `otool -l`). Check for dependencies on paths specific to the build machine.
  A declared minimum OS version still needs a load test on that OS version.
- Avoid `-C target-cpu=native` for a general distribution. Record custom compiler
  flags and native dependencies. Cross-compilation produces an artifact, but
  running it with Redis on the target is a separate check.

Rust documents [platform support](https://doc.rust-lang.org/rustc/platform-support.html)
and Cargo's [target/build options](https://doc.rust-lang.org/cargo/commands/cargo-build.html).
The recipe below supports native 64-bit macOS and GNU/Linux hosts; it does not
certify other operating systems, libc variants or CPU baselines.

## Build and package a concrete example

This recipe uses the SDK's `hello` module at a fixed revision. It requires Bash,
Git, a C compiler, rustup with **Rust 1.94.0**, **Python 3.13** and native
**Redis server/CLI 8.10.2** on `PATH`. Use a clean Cargo/C compiler environment;
record any wrappers, Cargo configuration or native compiler overrides you add.
Run the blocks in the same Bash session.
For your own module, pin its SDK git dependencies, build its `cdylib` with
`--lib` instead of `--example hello`, and substitute its name, version, features
and smoke assertions. Cargo normally replaces hyphens in library names with
underscores; confirm the produced filename rather than guessing it.

```bash
set -euo pipefail
ship_work=$(mktemp -d /tmp/redis-module-ship.XXXXXX)
git clone https://github.com/joshrotenberg/redismodule-rs "$ship_work/source"
cd "$ship_work/source"
git checkout --detach 85791c4be4a1f4b49a2b506500e897eaa3630332
ship_revision=$(git rev-parse HEAD)
ship_target=$(rustc +1.94.0 -vV | sed -n 's/^host: //p')
case "$ship_target" in
  aarch64-apple-darwin|x86_64-apple-darwin) ship_ext=dylib ;;
  aarch64-unknown-linux-gnu|x86_64-unknown-linux-gnu) ship_ext=so ;;
  *) echo "Recipe requires a native macOS or GNU/Linux target" >&2; exit 1 ;;
esac

# This library repository does not commit Cargo.lock. Record the first
# resolution, then reuse this lockfile with --locked for subsequent builds.
cargo +1.94.0 generate-lockfile
RUSTFLAGS='' CARGO_ENCODED_RUSTFLAGS='' cargo +1.94.0 build --locked \
  --release --example hello --target "$ship_target" \
  --target-dir "$ship_work/target" --no-default-features \
  --features min-redis-compatibility-version-6-0
ship_package="hello-3.0.0-${ship_revision:0:12}-$ship_target"
ship_stage="$ship_work/$ship_package"
mkdir "$ship_stage"
cp "$ship_work/target/$ship_target/release/examples/libhello.$ship_ext" "$ship_stage/"
cp Cargo.lock "$ship_stage/"
{
  printf 'source=%s\ntarget=%s\nprofile=release\nfeatures=min-redis-compatibility-version-6-0\n' \
    "$ship_revision" "$ship_target"
  printf 'rustflags=empty\n'
  rustc +1.94.0 -vV
  cargo +1.94.0 --version
  cc --version
  uname -srm
  redis-server --version
  redis-cli --version
} > "$ship_stage/BUILD-INFO.txt"
```

For your module distribution, retain its source, manifest, lockfile, native build
inputs and exact build command too. The inputs above describe this example;
they are not a claim of byte-identical builds across different environments.
Do not enable `system-allocator-fallback` for a production build; it is a test
feature, not a replacement for Redis-owned module allocation.

The next block hashes the contents and creates the archive. Keep its checksum with
release metadata delivered through your trusted distribution channel; a digest
by itself does not identify the publisher.

```bash
python3 - "$ship_stage" <<'PY'
import hashlib
from pathlib import Path
import sys
import tarfile

stage = Path(sys.argv[1])
def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()

files = sorted(path for path in stage.iterdir() if path.is_file())
(stage / "SHA256SUMS").write_text("".join(
    f"{digest(path)}  {path.name}\n" for path in files
))
archive = stage.parent / (stage.name + ".tar.gz")
with tarfile.open(archive, "w:gz") as bundle:
    bundle.add(stage, arcname=stage.name)
checksum = archive.with_name(archive.name + ".sha256")
checksum.write_text(f"{digest(archive)}  {archive.name}\n")
print(f"Packaged {archive}")
PY
```

Verify the saved digest before extraction, then check the extracted contents.
A recipient can use this same block with the archive and its trusted checksum.

```bash
python3 - "$ship_work/$ship_package.tar.gz" <<'PY'
import hashlib
from pathlib import Path
import sys
import tarfile

archive = Path(sys.argv[1])
def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()

expected = archive.with_name(archive.name + ".sha256").read_text().split()[0]
if digest(archive) != expected:
    raise ValueError("archive checksum mismatch")
extracted = archive.parent / "extracted"
with tarfile.open(archive) as bundle:
    bundle.extractall(extracted, filter="data")
package = extracted / archive.name.removesuffix(".tar.gz")
for line in (package / "SHA256SUMS").read_text().splitlines():
    expected, name = line.split("  ", 1)
    if digest(package / name) != expected:
        raise ValueError(f"content checksum mismatch: {name}")
print(f"Verified {archive}")
PY
```

## Load the extracted artifact

This starts a disposable native server with no TCP listener or persistence. It
loads the extracted library, checks the registered module/version and exercises
`hello.mul`. Startup and CLI calls are bounded; a failure prints the retained
server log. It does not contact an existing Redis instance.

```bash
python3 - "$ship_work/extracted/$ship_package/libhello.$ship_ext" "$ship_work" <<'PY'
import json
from pathlib import Path
import subprocess
import sys
import time

library = Path(sys.argv[1]).resolve()
work = Path(sys.argv[2]) / "smoke"
work.mkdir()
socket = work / "redis.sock"
log = work / "redis.log"

def query(*args):
    reply = subprocess.run(
        ["redis-cli", "-s", str(socket), "--json", *args],
        capture_output=True, text=True, timeout=2, check=True,
    )
    return json.loads(reply.stdout)

failed = True
with log.open("w") as output:
    server = subprocess.Popen([
        "redis-server", "--port", "0", "--unixsocket", str(socket),
        "--unixsocketperm", "700", "--save", "", "--appendonly", "no",
        "--dir", str(work), "--loadmodule", str(library),
    ], stdout=output, stderr=subprocess.STDOUT)
    try:
        deadline = time.monotonic() + 10
        while True:
            if server.poll() is not None:
                raise RuntimeError(f"Redis exited with {server.returncode}")
            try:
                if query("PING") == "PONG":
                    break
            except (subprocess.SubprocessError, json.JSONDecodeError):
                pass
            if time.monotonic() >= deadline:
                raise TimeoutError("Redis did not become ready")
            time.sleep(0.05)
        modules = query("MODULE", "LIST")
        if not any(m["name"] == "hello" and m["ver"] == 30000 for m in modules):
            raise RuntimeError(f"unexpected MODULE LIST: {modules!r}")
        actual = query("hello.mul", "31", "11")
        if actual != [31, 11, 341]:
            raise RuntimeError(f"unexpected hello.mul reply: {actual!r}")
        failed = False
        print(f"Native artifact smoke passed; log: {log}")
    finally:
        if server.poll() is None:
            server.terminate()
            try:
                server.wait(timeout=5)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait(timeout=5)
        if failed:
            print(log.read_text(), file=sys.stderr)
PY
printf 'Archive, extracted files and logs retained in %s\n' "$ship_work"
```

This recipe was validated on `aarch64-apple-darwin` with Rust 1.94.0,
Python 3.13.0, Apple Clang 21.0.0 and Redis server/CLI 8.10.2. The archive and
content corruption controls rejected altered inputs, and an invalid native
library produced a retained load-failure log.

Before distributing your module, run its behavior tests against the actual
server/platform combinations you intend to support. Include load failures,
configuration, persistence and upgrade checks where the module uses them. Keep
versioned artifact paths and the previous working binary; the
[upgrade guide](UPGRADING-MODULES.md) covers unload limitations and recovery.

Redis Software/Enterprise has its own version-specific package and installation
requirements. Its [user-defined module package reference](https://redis.io/docs/latest/operate/rs/references/rest-api/objects/bootstrap/user_defined_module/#module-package-requirements)
describes a ZIP containing a native library and `module.json`. This tarball
recipe does not generate or validate that product package.
