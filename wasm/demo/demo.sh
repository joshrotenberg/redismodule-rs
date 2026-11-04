#!/usr/bin/env bash
# A walk through the wasm host: builds the module and the example guests,
# starts throwaway Redis servers and runs the WASM.* commands against them.
#
#   wasm/demo/demo.sh                 run every step
#   DEMO_STEP=1 wasm/demo/demo.sh     wait for Enter before each section
#
# Environment:
#   REDIS_SERVER, REDIS_CLI   the binaries to use (default: from PATH)
#   DEMO_PORT                 the first port to try (default 7400); the
#                             script takes the first free ports from there
#   DEMO_JS=0                 skip the JavaScript guest even if Node.js is
#                             installed
#   DEMO_PROFILE=release      build the host with --release (default: the
#                             debug build the tests use); the JavaScript
#                             component compiles faster with it
#
# Every run starts over in target/wasm-demo, stops servers a previous run
# left behind, and stops its own servers on exit. It works with the macOS
# /bin/bash (3.2).
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"

REDIS_SERVER=${REDIS_SERVER:-redis-server}
REDIS_CLI=${REDIS_CLI:-redis-cli}
DEMO_PORT=${DEMO_PORT:-7400}
DEMO_STEP=${DEMO_STEP:-0}
DEMO_PROFILE=${DEMO_PROFILE:-debug}
target=${CARGO_TARGET_DIR:-$root/target}
demo=$target/wasm-demo
case $DEMO_PROFILE in
  debug) profile_flag= ;;
  release) profile_flag=--release ;;
  *) echo "demo: DEMO_PROFILE is debug or release" >&2; exit 1 ;;
esac

if [ -t 1 ]; then
  bold=$(printf '\033[1m')
  dim=$(printf '\033[2m')
  plain=$(printf '\033[0m')
else
  bold= dim= plain=
fi

section() {
  if [ "$DEMO_STEP" = 1 ] && [ -r /dev/tty ]; then
    printf '%s(Enter for: %s)%s' "$dim" "$*" "$plain"
    read -r _ </dev/tty || true
  fi
  printf '\n%s== %s%s\n\n' "$bold" "$*" "$plain"
}

note() {
  printf '%s# %s%s\n' "$dim" "$*" "$plain"
}

# A path as it reads from the repository root.
rel() {
  case $1 in
    "$root"/*) echo "${1#"$root"/}" ;;
    *) echo "$1" ;;
  esac
}

die() {
  printf 'demo: %s\n' "$*" >&2
  exit 1
}

# Milliseconds since the epoch, for timing the JavaScript load.
now_ms() {
  if [ -n "${EPOCHREALTIME:-}" ]; then
    local t=${EPOCHREALTIME/./}
    echo $((10#$t / 1000))
  elif command -v perl >/dev/null; then
    perl -MTime::HiRes=time -e 'printf "%d\n", time * 1000'
  else
    echo $(($(date +%s) * 1000))
  fi
}

# ---------------------------------------------------------------------------
# Servers

port_in_use() {
  (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null
}

# The first free port from $1, never 6398 and never $2.
free_port() {
  local port=$1
  while [ "$port" = 6398 ] || [ "$port" = "${2:-}" ] || port_in_use "$port"; do
    port=$((port + 1))
  done
  echo "$port"
}

port_of() {
  case $1 in
    primary) echo "$primary_port" ;;
    replica) echo "$replica_port" ;;
    *) die "no server named $1" ;;
  esac
}

cli() {
  local port
  port=$(port_of "$1")
  shift
  "$REDIS_CLI" -p "$port" "$@"
}

# reply_has <name> <text> <command ...>: whether the raw reply to the command
# contains text. The reply is read whole, so no pipe closes on redis-cli.
reply_has() {
  local name=$1 text=$2 out
  shift 2
  out=$(cli "$name" "$@" 2>&1) || true
  case $out in *"$text"*) return 0 ;; esac
  return 1
}

# Prints a command as typed at a prompt, then runs it and prints the reply as
# redis-cli formats it for a terminal.
show() {
  local who=$1
  shift
  printf '%s%s>%s %s\n' "$bold" "$who" "$plain" "$*"
  cli "$who" --no-raw "$@" 2>&1 || true
}

stop_pid() {
  local pid=$1
  local command
  command=$(ps -p "$pid" -o command= 2>/dev/null) || return 0
  if case $command in *redis-server*) true ;; *) false ;; esac; then
    kill "$pid" 2>/dev/null || true
    for _ in 1 2 3 4 5 6 7 8 9 10; do
      kill -0 "$pid" 2>/dev/null || return 0
      sleep 0.2
    done
    kill -9 "$pid" 2>/dev/null || true
  fi
}

cleanup() {
  local pidfile
  for pidfile in "$demo"/*/pid; do
    [ -e "$pidfile" ] || continue
    stop_pid "$(cat "$pidfile")"
    rm -f "$pidfile"
  done
}

# start_server <name> [redis-server arguments]: starts a server with the
# module in $demo/<name>, keeping that directory's RDB, and waits until it
# has loaded its data.
start_server() {
  local name=$1
  shift
  local port dir pid
  port=$(port_of "$name")
  dir=$demo/$name
  mkdir -p "$dir"
  "$REDIS_SERVER" --port "$port" --bind 127.0.0.1 --dir "$dir" \
    --logfile "$dir/redis.log" --save '' --appendonly no \
    --enable-debug-command local --busy-reply-threshold 100 \
    --loadmodule "$module" "$@" >/dev/null 2>&1 &
  pid=$!
  echo "$pid" >"$dir/pid"
  # Redis answers INFO while it still loads an RDB, so wait for loading:0.
  for _ in $(seq 1 200); do
    kill -0 "$pid" 2>/dev/null || { tail -n 20 "$dir/redis.log" >&2; die "$name did not start"; }
    # The newline keeps async_loading:0 from matching.
    if reply_has "$name" $'\nloading:0' INFO persistence; then
      note "$name: redis-server on port $port, pid $pid, log $(rel "$dir/redis.log")"
      return 0
    fi
    sleep 0.05
  done
  tail -n 20 "$dir/redis.log" >&2
  die "$name did not finish loading"
}

wait_exit() {
  local name=$1 pid
  pid=$(cat "$demo/$name/pid")
  for _ in $(seq 1 100); do
    kill -0 "$pid" 2>/dev/null || { rm -f "$demo/$name/pid"; return 0; }
    sleep 0.1
  done
  die "$name did not exit"
}

# Waits until PING on <name> gets -BUSY, for up to ten seconds.
wait_busy() {
  for _ in $(seq 1 200); do
    if reply_has "$1" BUSY PING; then
      return 0
    fi
    sleep 0.05
  done
  die "$1 never got busy"
}

# ---------------------------------------------------------------------------
# Setup

command -v "$REDIS_SERVER" >/dev/null || die "$REDIS_SERVER not found; set REDIS_SERVER"
command -v "$REDIS_CLI" >/dev/null || die "$REDIS_CLI not found; set REDIS_CLI"
command -v cargo >/dev/null || die "cargo not found"

# A previous run that was stopped hard may have left servers; stop them and
# start over.
cleanup
rm -rf "$demo"
mkdir -p "$demo"
trap cleanup EXIT

section "Build the host module and the example guests"

# Cargo.lock is not tracked. Seed a missing one from the snapshots CI uses, so
# a clean checkout builds the versions CI tested; existing ones stay as they are.
seed_lock() {
  if [ ! -e "$2" ]; then
    cp "$1" "$2"
    note "seeded $2 from $1"
  fi
}
seed_lock wasm/ci/locks/workspace.lock Cargo.lock
seed_lock wasm/ci/locks/hello.lock wasm/guests/hello/Cargo.lock

if [ "$(uname)" = Darwin ]; then ext=dylib; else ext=so; fi
module=$target/$DEMO_PROFILE/libredis_module_wasm.$ext

note "cargo build $profile_flag -p redis-module-wasm (the first build compiles wasmtime and takes minutes)"
cargo build --quiet $profile_flag -p redis-module-wasm
cargo build --quiet -p redis-module-wasm --example componentize
[ -x "$target/$DEMO_PROFILE/redis-wasm-compiler" ] || die "the compiler helper was not built next to the module"

if command -v rustup >/dev/null; then
  case $'\n'$(rustup target list --installed 2>/dev/null)$'\n' in
    *$'\n'wasm32-unknown-unknown$'\n'*) ;;
    *) die "the wasm32-unknown-unknown target is missing: rustup target add wasm32-unknown-unknown" ;;
  esac
fi
note "cargo build --release --target wasm32-unknown-unknown (wasm/guests/hello)"
CARGO_TARGET_DIR=$target/wasm-guests cargo build --quiet --release \
  --target wasm32-unknown-unknown --manifest-path wasm/guests/hello/Cargo.toml
hello=$demo/hello.wasm
"$target/debug/examples/componentize" \
  "$target/wasm-guests/wasm32-unknown-unknown/release/hello_guest.wasm" "$hello"

js=
if [ "${DEMO_JS:-1}" != 0 ] && command -v node >/dev/null && command -v npm >/dev/null; then
  if [ ! -d wasm/guests/js/node_modules ]; then
    note "npm ci (wasm/guests/js)"
    npm ci --prefix wasm/guests/js --no-audit --no-fund --silent
  fi
  note "node build.mjs (wasm/guests/js)"
  js=$demo/js.wasm
  (cd wasm/guests/js && NODE_NO_WARNINGS=1 node build.mjs "$js")
else
  note "Node.js not found (or DEMO_JS=0): skipping the JavaScript guest"
fi

note "$("$REDIS_SERVER" --version)"
note "module:  $(rel "$module") ($(wc -c <"$module" | tr -d ' ') bytes), with redis-wasm-compiler beside it"
note "hello:   $(rel "$hello") ($(wc -c <"$hello" | tr -d ' ') bytes, a Rust component)"
if [ -n "$js" ]; then
  note "js:      $(rel "$js") ($(wc -c <"$js" | tr -d ' ') bytes, JavaScript with its engine inside)"
fi

primary_port=$(free_port "$DEMO_PORT")
replica_port=$(free_port $((primary_port + 1)) "$primary_port")

section "Start a throwaway server with the module"
start_server primary
show primary MODULE LIST

# ---------------------------------------------------------------------------

section "Load a library"
note "WASM.LOAD compiles the component in a separate helper process while the client waits"
printf '%sprimary>%s WASM.LOAD hello <hello.wasm\n' "$bold" "$plain"
cli primary --no-raw -x WASM.LOAD hello <"$hello"
note "WASM.LIST names each library's functions with their flags (first lines):"
show primary WASM.LIST | head -n 12 || true

section "Call functions"
show primary WASM.CALL hello 0
show primary WASM.CALL hello 0 redis
note "the guest's log import wrote to the server log:"
grep 'hello called for redis' "$demo/primary/redis.log" | tail -n 1 || true

note "keys are declared after numkeys, as for FCALL; Redis sees them"
show primary COMMAND GETKEYS WASM.CALL incr_twice 1 visits
note "the guest calls INCR twice through the host's call import"
show primary WASM.CALL incr_twice 1 visits
show primary GET visits

note "typed replies pass through: HGETALL is a map to a RESP3 client"
show primary HSET user:1 name ada lang rust
printf '%sprimary>%s WASM.CALL hash_pairs 1 user:1   (redis-cli -3)\n' "$bold" "$plain"
cli primary --no-raw -3 WASM.CALL hash_pairs 1 user:1
note "call fails the function on an error reply, as redis.call does in Lua"
show primary WASM.CALL run 0 SET greeting hi
show primary WASM.CALL run 0 INCR greeting

section "Read-only calls"
note "WASM.CALL_RO runs only functions flagged no-writes, as FCALL_RO does"
show primary WASM.CALL_RO hello 0 ro
show primary WASM.CALL_RO set 1 k v
note "a no-writes function cannot write, even from WASM.CALL"
show primary WASM.CALL run_ro 0 SET k v
show primary WASM.CALL_RO run_ro 0 GET visits

section "A guest trap, and the server keeps serving"
note "a library keeps one instance between calls, so guest state carries over"
show primary WASM.CALL counter 0
show primary WASM.CALL counter 0
show primary WASM.CALL counter 0
note "oob reads past the end of its linear memory"
show primary WASM.CALL oob 0
show primary PING
note "the trap dropped the instance; the next call starts a fresh one"
show primary WASM.CALL counter 0

section "A busy function, and WASM.KILL"
note "spin loops forever; it yields to Redis every 10 ms"
printf '%sprimary>%s WASM.CALL spin 0   (in the background)\n' "$bold" "$plain"
cli primary --no-raw WASM.CALL spin 0 >"$demo/spin.out" 2>&1 &
spin=$!
wait_busy primary
note "past busy-reply-threshold (100 ms here) other clients get -BUSY, as for a long Lua script"
show primary PING
show primary GET visits
note "WASM.STATS and WASM.KILL are allow-busy, so they still run"
show primary WASM.STATS
show primary WASM.KILL
wait "$spin" || true
note "the spinning client's reply:"
cat "$demo/spin.out"
show primary PING
show primary WASM.KILL

section "Stats and memory"
show primary WASM.STATS
show primary WASM.CALL counter 0
show primary WASM.CALL counter 0
note "linear memory and compiled code live outside the Redis allocator; INFO wasm counts them"
printf '%sprimary>%s INFO wasm\n' "$bold" "$plain"
cli primary INFO wasm | tr -d '\r'

section "Libraries survive DEBUG RELOAD and a restart"
note "the RDB keeps each library's component as module aux data"
show primary WASM.CALL counter 0
note "DEBUG RELOAD saves an RDB, empties the server and loads the RDB again"
show primary DEBUG RELOAD
show primary WASM.STATS
show primary WASM.CALL hello 0 reloaded
note "the instance is new: only the component persists, not guest memory"
show primary WASM.CALL counter 0
show primary SAVE
printf '%sprimary>%s SHUTDOWN\n' "$bold" "$plain"
cli primary SHUTDOWN >/dev/null 2>&1 || true
wait_exit primary
start_server primary
grep 'wasm: loaded' "$demo/primary/redis.log" | tail -n 1 || true
show primary WASM.STATS
show primary WASM.CALL hello 0 restarted
show primary GET visits

section "A replica gets the libraries"
start_server replica
show replica WASM.STATS
show replica REPLICAOF 127.0.0.1 "$primary_port"
for _ in $(seq 1 200); do
  reply_has replica master_link_status:up INFO replication && break
  sleep 0.05
done
reply_has replica master_link_status:up INFO replication || die "the replica did not sync"
note "the full sync carried the library in the RDB"
show replica WASM.STATS
show replica WASM.CALL hello 0 replica
note "a read-only replica refuses a function that may write, as for Lua"
show replica WASM.CALL set 1 k v
note "a function's writes replicate as their effects"
show primary WASM.CALL incr_twice 1 visits
show primary WAIT 1 2000
show replica GET visits

if [ -n "$js" ]; then
  section "A JavaScript library"
  note "function names are global, and the JavaScript library has the same ones, so replace hello"
  show primary WASM.DELETE hello
  note "the component is 11 MB; it compiles in the helper process while the server answers"
  printf '%sprimary>%s WASM.LOAD js <js.wasm   (in the background)\n' "$bold" "$plain"
  start_ms=$(now_ms)
  cli primary --no-raw -x WASM.LOAD js <"$js" >"$demo/js-load.out" 2>&1 &
  load=$!
  pings=0
  slowest=0
  while kill -0 "$load" 2>/dev/null; do
    t0=$(now_ms)
    cli primary PING >/dev/null
    t=$(($(now_ms) - t0))
    [ "$t" -gt "$slowest" ] && slowest=$t
    pings=$((pings + 1))
    sleep 0.02
  done
  wait "$load" || true
  note "loaded in $(($(now_ms) - start_ms)) ms; $pings PINGs answered meanwhile, the slowest in $slowest ms (each a new redis-cli)"
  cat "$demo/js-load.out"
  show primary WASM.CALL hello 0 js
  show primary WASM.CALL incr_twice 1 visits
  show primary WASM.CALL counter 0
  show primary WASM.CALL counter 0
  note "the replica compiles the replicated WASM.LOAD on a thread too"
  for _ in $(seq 1 600); do
    # The replica's only library is then js, the first line of the list.
    reply_has replica $'js\n' WASM.LIST && break
    sleep 0.05
  done
  show replica WASM.CALL hello 0 js-on-the-replica
  show replica GET visits
  note "the helper process counters:"
  cli primary INFO wasm | tr -d '\r' |
    grep -E '^wasm_(compiler_(completed|failed|active|queued)|component_bytes|code_bytes):' || true
fi

section "A function that wrote cannot be killed"
note "write_then_spin SETs a key, then loops; killing it would leave its writes half done"
if [ -n "$js" ]; then
  # write_then_spin is in the Rust library only.
  show primary WASM.DELETE js
  printf '%sprimary>%s WASM.LOAD hello <hello.wasm\n' "$bold" "$plain"
  cli primary --no-raw -x WASM.LOAD hello <"$hello" | head -n 3 || true
  note "..."
fi
printf '%sprimary>%s WASM.CALL write_then_spin 1 guard   (in the background)\n' "$bold" "$plain"
cli primary --no-raw WASM.CALL write_then_spin 1 guard >"$demo/unkillable.out" 2>&1 &
spinner=$!
wait_busy primary
show primary WASM.KILL
note "as with Lua, SHUTDOWN NOSAVE is the way out. NOW skips waiting for lagging replicas:"
note "Redis checks that wait's timeout in serverCron, which does not run while a function yields"
printf '%sprimary>%s SHUTDOWN NOSAVE NOW\n' "$bold" "$plain"
cli primary SHUTDOWN NOSAVE NOW >/dev/null 2>&1 || true
wait "$spinner" || true
wait_exit primary
note "primary stopped; logs are in $(rel "$demo")"

section "Done"
note "the replica is stopped on exit; rerun this script to start over"
