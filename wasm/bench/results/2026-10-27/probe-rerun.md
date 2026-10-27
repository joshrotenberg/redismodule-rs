2026-10-27, after review: phase 7 rerun with the revised probe (the Rust guest
loaded and settled before `maxmemory` is set 16 MiB above `used_memory`), from
`CYCLES=1 RELOADS= wasm/bench/memory.sh target/memory/<build> target/memory/hello.wasm target/memory/library.wasm`.
Columns as in the other tables.

Redis allocator (`libredis_module_wasm-redisalloc.dylib`):

| 7. maxmemory 58.5, 16 MiB headroom: hello grown 48 MiB | 42.5 | 785.1 | 1051.1 | 1051.1 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 49.2 | SET: OK |
| 7. maxmemory 106.4: during JavaScript WASM.LOAD | 753.7 | 980.6 | 1036.5 | 1144.7 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 | 18 samples, 3.54 s; SET: 1 OK, 17 OOM |
| 7. maxmemory 106.4: JavaScript loaded | 61.9 | 980.6 | 1260.0 | 1260.0 | 1 | 7 | 10.7 | 90.2 | 0 | 1 | 7.3 | SET: OK |

System allocator (`libredis_module_wasm-system.dylib`):

| 7. maxmemory 17.1, 16 MiB headroom: hello grown 48 MiB | 1.1 | 24.6 | 966.7 | 966.4 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 49.2 | SET: OK |
| 7. maxmemory 65.1: during JavaScript WASM.LOAD | 11.8 | 24.6 | 926.1 | 949.5 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 | 16 samples, 3.51 s; SET: 16 OK, 0 OOM |
| 7. maxmemory 65.1: JavaScript loaded | 1.1 | 24.6 | 1118.4 | 1118.4 | 1 | 7 | 10.7 | 90.2 | 0 | 1 | 7.3 | SET: OK |
