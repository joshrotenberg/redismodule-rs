#!/usr/bin/env bash
# Run from the repository root with the four pinned lockfiles already present.
# This script deliberately does not replace a developer's existing lockfiles.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"
log_dir="$root/target/wasm-ci-logs"
mkdir -p "$log_dir"
export WASM_TEST_REQUIRE_JS=1
export CARGO_NET_OFFLINE=true
export RUST_TEST_THREADS=${RUST_TEST_THREADS:-2}
timeout_bin=${TIMEOUT_BIN:-timeout}

verify_locks() {
  cmp wasm/ci/locks/workspace.lock Cargo.lock || return 1
  cmp wasm/ci/locks/guest-sdk.lock wasm/guest-sdk/Cargo.lock || return 1
  cmp wasm/ci/locks/hello.lock wasm/guests/hello/Cargo.lock || return 1
  cmp wasm/ci/locks/bench.lock wasm/bench/guest/Cargo.lock || return 1
}

finish() {
  status=$?
  if ! verify_locks; then
    echo 'A pinned Rust lockfile changed during validation' >&2
    status=1
  fi
  exit "$status"
}
trap finish EXIT

run() {
  stage=$1
  shift
  "$timeout_bin" --kill-after=20s 12m "$@" 2>&1 | tee "$log_dir/$stage.log"
}

verify_locks
run versions bash -ec 'rustc --version; cargo --version; node --version; npm --version; redis-server --version'
run javascript-dependencies npm ci --prefix wasm/guests/js --no-audit --no-fund
run format cargo fmt --all -- --check
run clippy cargo clippy --locked --workspace --all-targets -- -D warnings
run examples cargo build --locked --examples
run host cargo build --locked -p redis-module-wasm
run tests cargo test --locked --workspace --no-fail-fast
run fallback cargo test --locked --features system-allocator-fallback --test system_allocator_fallback
run docs env RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps --features all-features-but-xor
run sdk-7-2-clippy cargo clippy --locked --workspace --exclude redis-module-wasm --all-targets --no-default-features --features min-redis-compatibility-version-7-2 -- -D warnings
run sdk-7-2-docs env RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --exclude redis-module-wasm --no-deps --no-default-features --features all-features-but-xor,min-redis-compatibility-version-7-2
for guest in wasm/guest-sdk wasm/guests/hello wasm/bench/guest; do
  run "$(basename "$guest")-format" cargo fmt --manifest-path "$guest/Cargo.toml" -- --check
done
run guest-sdk-clippy env CARGO_TARGET_DIR="$root/target/wasm-guest-sdk" cargo clippy --locked --manifest-path wasm/guest-sdk/Cargo.toml --all-targets -- -D warnings
run guest-sdk-tests env CARGO_TARGET_DIR="$root/target/wasm-guest-sdk" cargo test --locked --manifest-path wasm/guest-sdk/Cargo.toml
for guest in hello bench; do
  if [ "$guest" = hello ]; then
    manifest=wasm/guests/hello/Cargo.toml
  else
    manifest=wasm/bench/guest/Cargo.toml
  fi
  run "$guest-clippy" env CARGO_TARGET_DIR="$root/target/wasm-guests" cargo clippy --locked --manifest-path "$manifest" --target wasm32-unknown-unknown -- -D warnings
  run "$guest-build" env CARGO_TARGET_DIR="$root/target/wasm-guests" cargo build --locked --manifest-path "$manifest" --release --target wasm32-unknown-unknown
done
