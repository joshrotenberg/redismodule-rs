2026-10-27, Darwin arm64, Redis server v=8.10.2 sha=00000000:1
wasmtime 36.0.19; RAYON_NUM_THREADS=unset
module target/memory/libredis_module_wasm-system.dylib (12.0 MiB)
hello target/memory/hello.wasm (37686 bytes)
javascript target/memory/library.wasm (10.7 MiB)

mem_allocator libc, wasm_max_memory 64.0 MiB

| phase | used_memory | used_memory_peak | used_memory_rss | ps RSS | libraries | functions | component MiB | code MiB | compiling | instances | instance memory MiB | note |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 1. idle, module loaded | 1.0 | 1.0 | 8.2 | 8.2 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 |  |
| 2. hello loaded | 1.0 | 1.1 | 33.4 | 33.4 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 1.1 |  |
| 3. hello: 10,000 calls of counter | 1.0 | 1.2 | 33.4 | 33.4 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 1.1 | counter now 10001 |
| 4. hello: grow 16 MiB | 1.0 | 1.2 | 49.0 | 49.0 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 17.2 | memory.size 17.2 MiB |
| 4. hello: grow 40 MiB | 1.0 | 1.2 | 65.6 | 65.6 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 57.2 | memory.size 57.2 MiB |
| 4. hello: a call after growing | 1.0 | 1.2 | 65.6 | 65.6 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 57.2 | counter 10002: the instance is kept |
| 4. hello: grow 80 MiB, past wasm.max-memory | 1.0 | 1.2 | 25.6 | 25.6 | 1 | 22 | 0.0 | 0.3 | 0 | 0 | 0.0 | ERR function 'grow' failed: the instance would use 143982592 bytes of memory, more than wasm.max-memory (67108864) |
| 4. hello: the next call | 1.0 | 1.2 | 18.7 | 18.7 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 1.1 | counter 1: a new instance |
| 5. after WASM.DELETE hello | 1.1 | 1.2 | 18.4 | 18.4 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 |  |
| 2. JavaScript: during WASM.LOAD (sampled maxima) | 22.4 | 22.4 | 833.2 | 854.8 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 | 19 samples, 3.26 s |
| 2. JavaScript loaded | 1.1 | 22.4 | 848.5 | 848.5 | 1 | 7 | 10.7 | 90.2 | 0 | 1 | 7.3 |  |
| 3. JavaScript: 10,000 calls of counter | 1.1 | 22.4 | 837.0 | 837.0 | 1 | 7 | 10.7 | 90.2 | 0 | 1 | 9.1 | counter now 10001 |
| 5. after WASM.DELETE js | 1.1 | 22.4 | 739.4 | 739.4 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 |  |
| 5. after a second load and WASM.FLUSH | 1.1 | 24.4 | 778.5 | 778.5 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 |  |
| 6. after 5 JavaScript load and delete cycles | 1.1 | 24.4 | 1087.9 | 1087.9 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 |  |
| 6. after 10 JavaScript load and delete cycles | 1.1 | 24.4 | 1018.4 | 1018.5 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 |  |
| 6. after 20 JavaScript load and delete cycles | 1.1 | 24.4 | 1058.8 | 1058.9 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 |  |
| 7. maxmemory 65.1: hello grown 48 MiB | 1.1 | 24.4 | 999.8 | 999.8 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 49.2 | SET: OK |
| 7. maxmemory 65.1: during JavaScript WASM.LOAD | 11.9 | 24.4 | 988.0 | 1102.6 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 | 15 samples, 3.54 s; SET: 15 OK, 0 OOM |
| 7. maxmemory 65.1: JavaScript loaded | 1.1 | 24.4 | 1057.6 | 1057.6 | 1 | 7 | 10.7 | 90.2 | 0 | 1 | 7.3 | SET: OK |
| 8. reload server: hello loaded | 1.0 | 1.1 | 31.6 | 31.6 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 1.1 | module mappings 1 |
| 8. after 1 MODULE UNLOAD and LOAD cycles | 1.1 | 1.1 | 33.7 | 33.7 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 1.1 | module mappings 1 |
| 8. after 5 MODULE UNLOAD and LOAD cycles | 1.1 | 1.1 | 37.9 | 37.9 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 1.1 | module mappings 1 |
| 8. after 10 MODULE UNLOAD and LOAD cycles | 1.1 | 1.1 | 38.9 | 38.9 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 1.1 | module mappings 1 |
| 8. after 20 MODULE UNLOAD and LOAD cycles | 1.1 | 1.1 | 41.1 | 41.1 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 1.1 | module mappings 1 |
| 8. after 30 MODULE UNLOAD and LOAD cycles | 1.1 | 1.1 | 41.4 | 41.4 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 1.1 | module mappings 1 |
| 8. after a last MODULE UNLOAD | 1.0 | 1.1 | 41.0 | 41.0 | - | - | - | - | - | - | - | module mappings 1 |
