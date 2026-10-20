#!/usr/bin/env bash
# Benchmarks the functions in guest/ (wasm) against functions.lua (Lua) on a
# throwaway redis-server, and prints a markdown table.
#
# Each function runs with redis-benchmark; "µs/call" is the server's time per
# call from INFO commandstats (usec_per_call), so it leaves out the network.
# Before timing, the script checks that wasm and Lua give the same results.
#
# Needs redis-server, redis-cli and redis-benchmark on the PATH, and the
# wasm32-unknown-unknown target. PORT and CLIENTS can be set in the
# environment.
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
bench=$root/wasm/bench
port=${PORT:-6700}
clients=${CLIENTS:-50}
case $(uname) in Darwin) ext=dylib ;; *) ext=so ;; esac

# function | keys and arguments | requests
cases=(
  "incr_twice|1 counter|200000"
  "nested|0|200000"
  "adler32|1 blob|20000"
  "median|1 numbers|20000"
  "lrange|1 numbers|20000"
  "median_flat|1 numbers|20000"
  "lrange_flat|1 numbers|20000"
  "primes|0 10000|5000"
)

cd "$root"
cargo build --quiet --release -p redis-module-wasm
cargo build --quiet --release --target wasm32-unknown-unknown --manifest-path "$bench/guest/Cargo.toml"

dir=$(mktemp -d)
cargo run --quiet --release -p redis-module-wasm --example componentize -- \
  "$bench/guest/target/wasm32-unknown-unknown/release/bench_guest.wasm" "$dir/bench.wasm"

redis-server --port "$port" --save '' --appendonly no --dir "$dir" --logfile "$dir/redis.log" \
  --loadmodule "$root/target/release/libredis_module_wasm.$ext" &
server=$!
trap 'kill $server 2>/dev/null; wait $server 2>/dev/null; rm -rf "$dir"' EXIT

cli() { redis-cli -p "$port" "$@"; }
until cli PING >/dev/null 2>&1; do sleep 0.1; done

cli -x WASM.LOAD bench < "$dir/bench.wasm" >/dev/null
cli -x FUNCTION LOAD REPLACE < "$bench/functions.lua" >/dev/null
cli SET counter 0 >/dev/null
head -c 16384 /dev/urandom | cli -x SET blob >/dev/null
# 1000 pseudo-random integers.
# shellcheck disable=SC2046
cli RPUSH numbers $(awk 'BEGIN { srand(42); for (i = 0; i < 1000; i++) printf "%d ", int(rand() * 1000000) }') >/dev/null

# The same results from both, except incr_twice, whose result changes per call.
for entry in "${cases[@]}"; do
  IFS='|' read -r name args _ <<< "$entry"
  [ "$name" = incr_twice ] && continue
  # shellcheck disable=SC2086
  lua=$(cli --no-raw FCALL "$name" $args)
  # shellcheck disable=SC2086
  wasm=$(cli --no-raw WASM.CALL "$name" $args)
  if [ "$lua" != "$wasm" ]; then
    printf 'results differ for %s:\nLua:\n%s\nwasm:\n%s\n' "$name" "$lua" "$wasm" >&2
    exit 1
  fi
done

# Prints the server's µs per call and the calls per second for one command.
measure() {
  local command=$1 name=$2 args=$3 requests=$4 rps usec
  cli CONFIG RESETSTAT >/dev/null
  # shellcheck disable=SC2086
  rps=$(redis-benchmark -p "$port" -c "$clients" -n "$requests" -q "$command" "$name" $args 2>/dev/null \
    | tr '\r' '\n' | grep 'requests per second' | tail -1 | sed 's/^.*: \([0-9.]*\) requests.*/\1/')
  usec=$(cli INFO commandstats | tr -d '\r' \
    | grep "^cmdstat_$(echo "$command" | tr '[:upper:]' '[:lower:]'):" | sed 's/.*usec_per_call=\([0-9.]*\).*/\1/')
  printf '%s %s\n' "$usec" "$rps"
}

echo "| function | Lua µs/call | wasm µs/call | wasm / Lua | Lua calls/s | wasm calls/s |"
echo "|---|---|---|---|---|---|"
for entry in "${cases[@]}"; do
  IFS='|' read -r name args requests <<< "$entry"
  read -r lua_usec lua_rps < <(measure FCALL "$name" "$args" "$requests")
  read -r wasm_usec wasm_rps < <(measure WASM.CALL "$name" "$args" "$requests")
  ratio=$(awk -v w="$wasm_usec" -v l="$lua_usec" 'BEGIN { printf "%.2f", w / l }')
  printf '| %s | %s | %s | %s | %.0f | %.0f |\n' "$name" "$lua_usec" "$wasm_usec" "$ratio" "$lua_rps" "$wasm_rps"
done
