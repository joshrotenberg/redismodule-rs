2026-10-27, Darwin arm64, Redis server v=8.10.2 sha=00000000:1
wasmtime 36.0.19; RAYON_NUM_THREADS=unset
module target/memory/libredis_module_wasm-redisalloc.dylib (12.0 MiB)
hello target/memory/hello.wasm (37686 bytes)
javascript target/memory/library.wasm (10.7 MiB)

mem_allocator libc, wasm_max_memory 64.0 MiB

| phase | used_memory | used_memory_peak | used_memory_rss | ps RSS | libraries | functions | component MiB | code MiB | compiling | instances | instance memory MiB | note |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 1. idle, module loaded | 1.0 | 1.0 | 8.3 | 8.3 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 |  |
| 2. hello loaded | 2.0 | 17.7 | 30.1 | 30.1 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 1.1 |  |
| 3. hello: 10,000 calls of counter | 2.1 | 17.7 | 30.2 | 30.2 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 1.1 | counter now 10001 |
| 4. hello: grow 16 MiB | 2.1 | 17.7 | 46.2 | 46.2 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 17.2 | memory.size 17.2 MiB |
| 4. hello: grow 40 MiB | 2.1 | 17.7 | 70.2 | 70.2 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 57.2 | memory.size 57.2 MiB |
| 4. hello: a call after growing | 2.1 | 17.7 | 70.2 | 70.2 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 57.2 | counter 10002: the instance is kept |
| 4. hello: grow 80 MiB, past wasm.max-memory | 2.1 | 17.7 | 30.1 | 30.1 | 1 | 22 | 0.0 | 0.3 | 0 | 0 | 0.0 | ERR function 'grow' failed: the instance would use 143982592 bytes of memory, more than wasm.max-memory (67108864) |
| 4. hello: the next call | 2.1 | 17.7 | 30.2 | 30.2 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 1.1 | counter 1: a new instance |
| 5. after WASM.DELETE hello | 2.0 | 17.7 | 29.9 | 29.9 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 |  |
| 2. JavaScript: during WASM.LOAD (sampled maxima) | 795.0 | 810.6 | 838.3 | 856.5 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 | 18 samples, 3.08 s |
| 2. JavaScript loaded | 46.5 | 810.6 | 863.4 | 863.4 | 1 | 7 | 10.7 | 90.2 | 0 | 1 | 7.3 |  |
| 3. JavaScript: 10,000 calls of counter | 46.5 | 810.6 | 864.5 | 864.5 | 1 | 7 | 10.7 | 90.2 | 0 | 1 | 9.1 | counter now 10001 |
| 5. after WASM.DELETE js | 34.6 | 810.6 | 766.9 | 766.9 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 |  |
| 5. after a second load and WASM.FLUSH | 42.4 | 810.6 | 777.4 | 777.4 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 |  |
| 6. after 5 JavaScript load and delete cycles | 50.3 | 916.1 | 1136.5 | 1136.5 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 |  |
| 6. after 10 JavaScript load and delete cycles | 59.4 | 916.1 | 1089.8 | 1089.8 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 |  |
| 6. after 20 JavaScript load and delete cycles | 57.0 | 937.1 | 1081.5 | 1081.5 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 |  |
| 7. maxmemory 121.0: hello grown 48 MiB | 55.8 | 937.1 | 1106.4 | 1076.3 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 49.2 | SET: OK |
| 7. maxmemory 121.0: during JavaScript WASM.LOAD | 690.5 | 937.1 | 1023.5 | 1018.0 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 | 17 samples, 3.51 s; SET: 1 OK, 16 OOM |
| 7. maxmemory 121.0: JavaScript loaded | 75.8 | 937.1 | 1116.1 | 1115.9 | 1 | 7 | 10.7 | 90.2 | 0 | 1 | 7.3 | SET: OK |
| 8. reload server: hello loaded | 2.2 | 13.9 | 28.8 | 28.8 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 1.1 | module mappings 1 |
| 8. after 1 MODULE UNLOAD and LOAD cycles | 2.1 | 17.2 | 32.6 | 32.6 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 1.1 | module mappings 1 |
| 8. after 5 MODULE UNLOAD and LOAD cycles | 2.1 | 18.3 | 37.6 | 37.6 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 1.1 | module mappings 1 |
| 8. after 10 MODULE UNLOAD and LOAD cycles | 2.1 | 19.6 | 39.7 | 39.7 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 1.1 | module mappings 1 |
| 8. after 20 MODULE UNLOAD and LOAD cycles | 2.8 | 19.6 | 42.8 | 42.8 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 1.1 | module mappings 1 |
| 8. after 30 MODULE UNLOAD and LOAD cycles | 2.5 | 20.8 | 35.1 | 35.1 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 1.1 | module mappings 1 |
| 8. after a last MODULE UNLOAD | 1.0 | 20.8 | 34.6 | 34.6 | - | - | - | - | - | - | - | module mappings 1 |
