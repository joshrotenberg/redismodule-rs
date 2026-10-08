#!/usr/bin/env sh
set -eu

module_repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
cd "$module_repo_dir"

# Override the existing 8.0 API floor with, for example, REDIS_MODULE_COMPATIBILITY=7-2.
exec cargo test --workspace --all-targets --locked --no-default-features \
    --features "min-redis-compatibility-version-${REDIS_MODULE_COMPATIBILITY:-8-0},bindgen-runtime" \
    "$@"
