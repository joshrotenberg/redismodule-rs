#!/usr/bin/env bash
# Measures the wasm host's memory on throwaway redis-servers and prints a
# markdown table: Redis's accounting (used_memory, used_memory_peak and
# used_memory_rss from INFO memory), the process RSS from ps, and the INFO wasm
# fields, after each phase:
#
#   1. an idle server with the module;
#   2. loading the Rust and the JavaScript guest, sampling INFO and ps while
#      the JavaScript component compiles;
#   3. 10,000 calls of counter;
#   4. growing the Rust guest's linear memory with grow, up to and past
#      wasm.max-memory;
#   5. WASM.DELETE and WASM.FLUSH;
#   6. repeated loads and deletes of the JavaScript library;
#   7. a maxmemory probe: SETs against a maxmemory 64 MiB above used_memory
#      while a guest grows and while the JavaScript component compiles;
#   8. MODULE UNLOAD and MODULE LOAD cycles with wasm.persist no, on a second
#      server.
#
# Usage: wasm/bench/memory.sh [module] [hello.wasm] [library.wasm]
#
#   module        the host module; by default a release build of this checkout
#   hello.wasm    the Rust example guest (guests/hello) as a component; by
#                 default built from this checkout
#   library.wasm  the JavaScript guest (guests/js); by default
#                 wasm/guests/js/library.wasm. Without it the JavaScript phases
#                 are skipped.
#
# Environment: PORT, the first port tried (default 6710; the first free one
# from there is used); CYCLES, the JavaScript load and delete checkpoints
# (default "5 10 20"); RELOADS, the MODULE UNLOAD and LOAD checkpoints
# (default "1 5 10 20 30", empty to skip); REDIS_SERVER, the server binary.
# The servers inherit the environment, so RAYON_NUM_THREADS limits wasmtime's
# parallel compilation.
#
# MiB throughout. used_memory_rss is refreshed by the server's cron every
# 100 ms; the ps column is read live. used_memory_peak never goes down.
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
case $(uname) in Darwin) ext=dylib ;; *) ext=so ;; esac
server_bin=${REDIS_SERVER:-redis-server}
cycles=${CYCLES:-5 10 20}
reloads=${RELOADS-1 5 10 20 30}
dir=$(mktemp -d)
server=

cleanup() {
  if [ -n "$server" ]; then
    kill "$server" 2>/dev/null || true
    wait "$server" 2>/dev/null || true
  fi
  rm -rf "$dir"
}
trap cleanup EXIT

fail() {
  echo "memory.sh: $*" >&2
  exit 1
}

module=${1:-}
if [ -z "$module" ]; then
  (cd "$root" && cargo build --quiet --release -p redis-module-wasm)
  module=$root/target/release/libredis_module_wasm.$ext
fi
hello=${2:-}
if [ -z "$hello" ]; then
  (cd "$root" && cargo build --quiet --release --target wasm32-unknown-unknown \
    --manifest-path wasm/guests/hello/Cargo.toml)
  (cd "$root" && cargo run --quiet --release -p redis-module-wasm --example componentize -- \
    wasm/guests/hello/target/wasm32-unknown-unknown/release/hello_guest.wasm "$dir/hello.wasm")
  hello=$dir/hello.wasm
fi
js=${3:-$root/wasm/guests/js/library.wasm}
[ -f "$module" ] || fail "no module at $module"
[ -f "$hello" ] || fail "no component at $hello"
# The server runs in its own directory, so it needs an absolute module path.
module=$(cd "$(dirname "$module")" && pwd)/$(basename "$module")
if [ ! -f "$js" ]; then
  echo "No JavaScript component at $js: skipping its phases (see wasm/README.md to build it)." >&2
  js=
fi

# The first free port from PORT, never 6398.
port=
for p in $(seq "${PORT:-6710}" "$((${PORT:-6710} + 89))"); do
  [ "$p" = 6398 ] && continue
  if ! (exec 3<>"/dev/tcp/127.0.0.1/$p") 2>/dev/null; then
    port=$p
    break
  fi
done
[ -n "$port" ] || fail "no free port from ${PORT:-6710}"

# Error replies are checked by ok, not by redis-cli's exit status.
cli() { redis-cli -p "$port" "$@" || true; }

# Fails on an empty or error reply.
ok() {
  case $1 in
    "") fail "no reply" ;;
    ERR* | *"(error)"* | OOM* | WRONG* | NOPERM*) fail "unexpected reply: $1" ;;
  esac
}

start_server() {
  local data=$1
  shift
  mkdir -p "$data"
  "$server_bin" --port "$port" --bind 127.0.0.1 --save '' --appendonly no \
    --dir "$data" --logfile "$data/redis.log" "$@" &
  server=$!
  local _
  for _ in $(seq 100); do
    kill -0 "$server" 2>/dev/null || fail "redis-server did not start; see $data/redis.log"
    if [ "$(cli PING 2>/dev/null)" = PONG ]; then
      [ "$(cli INFO server | tr -d '\r' | sed -n 's/^process_id://p')" = "$server" ] \
        || fail "another server answers on port $port"
      return
    fi
    sleep 0.1
  done
  fail "redis-server did not answer on port $port"
}

stop_server() {
  cli SHUTDOWN NOSAVE >/dev/null 2>&1 || true
  wait "$server" 2>/dev/null || true
  server=
}

fields="used_memory used_memory_peak used_memory_rss wasm_libraries wasm_functions"
fields="$fields wasm_component_bytes wasm_code_bytes wasm_compiling wasm_instances"
fields="$fields wasm_instance_memory_bytes wasm_max_memory"

# Reads INFO memory, INFO wasm and ps into used, peak, rss, libs, fns, comp,
# code, compiling, inst, instmem, maxmem and psrss.
snap() {
  read -r used peak rss libs fns comp code compiling inst instmem maxmem < <(
    cli INFO memory wasm | tr -d '\r' | awk -F: -v names="$fields" '
      { v[$1] = $2 }
      END {
        n = split(names, k, " ")
        for (i = 1; i <= n; i++) printf "%s ", (k[i] in v ? v[k[i]] : "-")
        print ""
      }')
  psrss=$(($(ps -o rss= -p "$server") * 1024))
}

# Waits for the cron to refresh used_memory_rss, then reads.
settle() {
  sleep 0.3
  snap
}

mib() {
  awk -v b="$1" 'BEGIN { if (b == "-") print "-"; else printf "%.1f", b / 1048576 }'
}

emit() {
  printf '| %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s |\n' \
    "$1" "$(mib "$used")" "$(mib "$peak")" "$(mib "$rss")" "$(mib "$psrss")" \
    "$libs" "$fns" "$(mib "$comp")" "$(mib "$code")" "$compiling" "$inst" \
    "$(mib "$instmem")" "${2:-}"
}

now() { perl -MTime::HiRes=time -e 'printf "%.2f\n", time'; }

# Loads a component and samples INFO and ps until the load replies. With
# "probe", each sample also runs a SET and counts its replies. Leaves the
# INFO wasm fields of the sample with the most used_memory, the sampled maxima
# of used, rss and psrss, peak read after the load, and a note in load_note.
watch_load() {
  local name=$1 file=$2 probe=${3:-} start job reply set_ok=0 set_oom=0 samples=0
  start=$(now)
  cli -x WASM.LOAD "$name" <"$file" >"$dir/load.out" 2>&1 &
  job=$!
  max_used=0 max_rss=0 max_ps=0 at_max="- - - - - - -"
  while kill -0 "$job" 2>/dev/null; do
    snap
    samples=$((samples + 1))
    if [ "$used" -gt "$max_used" ]; then
      max_used=$used
      at_max="$libs $fns $comp $code $compiling $inst $instmem"
    fi
    [ "$rss" -gt "$max_rss" ] && max_rss=$rss
    [ "$psrss" -gt "$max_ps" ] && max_ps=$psrss
    if [ -n "$probe" ]; then
      reply=$(cli SET probe x)
      case $reply in OK) set_ok=$((set_ok + 1)) ;; OOM*) set_oom=$((set_oom + 1)) ;; esac
    fi
    sleep 0.05
  done
  wait "$job" || true
  ok "$(head -c 200 "$dir/load.out")"
  used=$max_used rss=$max_rss psrss=$max_ps
  read -r libs fns comp code compiling inst instmem <<<"$at_max"
  peak=$(cli INFO memory | tr -d '\r' | sed -n 's/^used_memory_peak://p')
  load_note="$samples samples, $(awk -v a="$start" -v b="$(now)" 'BEGIN { printf "%.2f", b - a }') s"
  [ -n "$probe" ] && load_note="$load_note; SET: $set_ok OK, $set_oom OOM"
  return 0
}

load() {
  ok "$(cli -x WASM.LOAD "$1" <"$2" | head -c 200)"
}

# The server's mappings of the module file: whether MODULE UNLOAD unmapped it.
mapped() {
  if [ -r "/proc/$server/maps" ]; then
    grep -c "$(basename "$module")" "/proc/$server/maps" || true
  else
    lsof -p "$server" 2>/dev/null | grep -c "$(basename "$module")" || true
  fi
}

header() {
  echo "| phase | used_memory | used_memory_peak | used_memory_rss | ps RSS | libraries | functions | component MiB | code MiB | compiling | instances | instance memory MiB | note |"
  echo "|---|---|---|---|---|---|---|---|---|---|---|---|---|"
}

wasmtime=$(awk '/^name = "wasmtime"$/ { getline; gsub(/[^0-9.]/, ""); print; exit }' "$root/Cargo.lock" 2>/dev/null || true)
echo "$(date +%F), $(uname -sm), $("$server_bin" --version | cut -d' ' -f1-4)"
echo "wasmtime ${wasmtime:-unknown}; RAYON_NUM_THREADS=${RAYON_NUM_THREADS:-unset}"
echo "module $module ($(mib "$(wc -c <"$module" | tr -d ' ')") MiB)"
echo "hello $hello ($(wc -c <"$hello" | tr -d ' ') bytes)"
[ -n "$js" ] && echo "javascript $js ($(mib "$(wc -c <"$js" | tr -d ' ')") MiB)"
echo

start_server "$dir/main" --loadmodule "$module"
snap
echo "mem_allocator $(cli INFO memory | tr -d '\r' | sed -n 's/^mem_allocator://p'), wasm_max_memory $(mib "$maxmem") MiB"
echo
header

settle
emit "1. idle, module loaded"

# 2 and 3 with the Rust guest.
load hello "$hello"
settle
emit "2. hello loaded"
redis-benchmark -p "$port" -c 10 -n 10000 -q WASM.CALL counter 0 >/dev/null
settle
emit "3. hello: 10,000 calls of counter" "counter now $(cli WASM.CALL counter 0)"

# 4. Linear memory growth. grow fills a vector of that many bytes, so its
# pages are touched; linear memory does not shrink when the vector is freed.
# The guest's allocator does not reuse the freed 16 MiB for the larger
# request, so the second grow adds about 40 MiB to memory.size.
for mb in 16 40; do
  reply=$(cli WASM.CALL grow 0 $((mb << 20)))
  ok "$reply"
  settle
  emit "4. hello: grow $mb MiB" "memory.size $(mib "$reply") MiB"
done
reply=$(cli WASM.CALL counter 0)
settle
emit "4. hello: a call after growing" "counter $reply: the instance is kept"
reply=$(cli WASM.CALL grow 0 $((80 << 20)) 2>&1 | head -c 120 || true)
settle
emit "4. hello: grow 80 MiB, past wasm.max-memory" "${reply//|/ }"
reply=$(cli WASM.CALL counter 0)
settle
emit "4. hello: the next call" "counter $reply: a new instance"

# 5. Delete.
ok "$(cli WASM.DELETE hello)"
settle
emit "5. after WASM.DELETE hello"

if [ -n "$js" ]; then
  watch_load js "$js"
  emit "2. JavaScript: during WASM.LOAD (sampled maxima)" "$load_note"
  settle
  emit "2. JavaScript loaded"
  redis-benchmark -p "$port" -c 10 -n 10000 -q WASM.CALL counter 0 >/dev/null
  settle
  emit "3. JavaScript: 10,000 calls of counter" "counter now $(cli WASM.CALL counter 0)"
  ok "$(cli WASM.DELETE js)"
  settle
  emit "5. after WASM.DELETE js"
  load js "$js"
  ok "$(cli WASM.FLUSH)"
  settle
  emit "5. after a second load and WASM.FLUSH"

  # 6. Load and delete cycles.
  done_cycles=0
  for checkpoint in $cycles; do
    while [ "$done_cycles" -lt "$checkpoint" ]; do
      load js "$js"
      ok "$(cli WASM.CALL counter 0)"
      ok "$(cli WASM.DELETE js)"
      done_cycles=$((done_cycles + 1))
    done
    settle
    emit "6. after $checkpoint JavaScript load and delete cycles"
  done
fi

# 7. The maxmemory probe.
snap
limit=$((used + (64 << 20)))
ok "$(cli CONFIG SET maxmemory "$limit")"
load hello "$hello"
ok "$(cli WASM.CALL grow 0 $((48 << 20)))"
reply=$(cli SET probe x)
settle
emit "7. maxmemory $(mib "$limit"): hello grown 48 MiB" "SET: $reply"
ok "$(cli WASM.DELETE hello)"
if [ -n "$js" ]; then
  watch_load js "$js" probe
  emit "7. maxmemory $(mib "$limit"): during JavaScript WASM.LOAD" "$load_note"
  reply=$(cli SET probe x)
  settle
  emit "7. maxmemory $(mib "$limit"): JavaScript loaded" "SET: $reply"
  ok "$(cli WASM.DELETE js)"
fi
ok "$(cli CONFIG SET maxmemory 0)"
stop_server

# 8. Unload and reload, on a fresh server.
if [ -n "$reloads" ]; then
  start_server "$dir/reload" --enable-module-command local --loadmodule "$module" persist no
  load hello "$hello"
  settle
  emit "8. reload server: hello loaded" "module mappings $(mapped)"
  done_reloads=0
  for checkpoint in $reloads; do
    while [ "$done_reloads" -lt "$checkpoint" ]; do
      ok "$(cli MODULE UNLOAD wasm)"
      ok "$(cli MODULE LOAD "$module" persist no)"
      load hello "$hello"
      ok "$(cli WASM.CALL counter 0)"
      done_reloads=$((done_reloads + 1))
    done
    settle
    emit "8. after $checkpoint MODULE UNLOAD and LOAD cycles" "module mappings $(mapped)"
  done
  ok "$(cli MODULE UNLOAD wasm)"
  settle
  emit "8. after a last MODULE UNLOAD" "module mappings $(mapped)"
  stop_server
fi
