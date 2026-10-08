# Archive consumers

These are ordinary external module-author fixtures. `manual` uses `redis_module!`
and `NextArg` to echo an arbitrary byte string. `proc` uses the public `command`
attribute, a typed integer parser and the `RedisValue` derive.
The command attribute currently requires an explicit downstream `linkme`
dependency; the generated proc-consumer manifest includes it.

Run from the repository root with Python 3.11+, the development compiler,
installed stable Rust, libclang and Redis on `PATH`:

```sh
LIBCLANG_PATH=/Library/Developer/CommandLineTools/usr/lib \
  python3 tools/package_baseline.py --output target/f01/packages
python3 -m unittest discover -s tools -p test_package_baseline.py
```

The tool refuses to overwrite an evidence directory. Give a new output path for
another run. `--toolchains 1.94.0 stable`, `--command-timeout 600`,
`--redis-server /path/to/redis-server` and `--skip-runtime` control local checks.
The libclang setting above is specific to Apple's Command Line Tools installation.

The four actual `cargo package` archives enter a temporary loopback sparse
registry. Generated consumer manifests contain exact package versions and a
named registry dependency, with no path, Git or `[patch]` override. Internal
candidate dependencies remain in that registry; third-party dependencies use
crates.io. The registry has no publishing endpoint. A fresh Cargo home avoids
preexisting same-version candidate packages. The tool retains generated manifests,
fresh locks, metadata, package and extracted-file digests, compiler identities,
command logs and runtime logs under the chosen output directory.

The metadata and lock checks reject an upstream registry package with the same
name/version. Every extracted candidate source file must also match its archive.
The manual consumer needs the facade, internals and common packages; the proc
consumer additionally proves use of the public macro archive. Both consumers
build in debug and release with each selected compiler. Successful package
assembly alone is not treated as successful consumption.

Only the manual release consumer is loaded into a loopback Redis process. It
verifies the owned process ID, server version and allocator using retained
`INFO server` and `INFO memory` responses before exercising module commands.
The expected version and allocator come from the exact executable's `--version`;
the executable and loaded library hashes are retained. It checks empty, ASCII,
NUL/non-UTF-8/CRLF and all-256-byte payloads and verifies that
the server still responds. Startup has a 15-second deadline, every Cargo command
has a deadline, and subprocesses are cleaned up on success and failure. Proc
command registration remains a separate runtime gate; this harness does not
load that fixture. Compiler or host success here does not establish an MSRV,
general Redis support, CI runner correctness, or publication readiness.

Registry entries follow the [Cargo index format](https://doc.rust-lang.org/cargo/reference/registry-index.html)
and [alternate-registry configuration](https://doc.rust-lang.org/cargo/reference/registries.html).
