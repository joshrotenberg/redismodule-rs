2026-10-27, Darwin arm64, Redis server v=8.10.2 sha=00000000:1
wasmtime 36.0.19; RAYON_NUM_THREADS=unset
module target/memory/libredis_module_wasm-redisalloc.dylib (12.0 MiB)
hello target/memory/hello.wasm (37686 bytes)
javascript target/memory/library.wasm (10.7 MiB)

mem_allocator libc, wasm_max_memory 64.0 MiB

| phase | used_memory | used_memory_peak | used_memory_rss | ps RSS | libraries | functions | component MiB | code MiB | compiling | instances | instance memory MiB | note |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 1. idle, module loaded | 1.0 | 1.0 | 8.2 | 8.2 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 |  |
| 2. hello loaded | 3.4 | 12.2 | 27.8 | 27.8 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 1.1 |  |
| 3. hello: 10,000 calls of counter | 3.5 | 12.2 | 27.9 | 27.9 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 1.1 | counter now 10001 |
| 4. hello: grow 16 MiB | 3.5 | 12.2 | 43.9 | 43.9 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 17.2 | memory.size 17.2 MiB |
| 4. hello: grow 40 MiB | 3.5 | 12.2 | 67.9 | 67.9 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 57.2 | memory.size 57.2 MiB |
| 4. hello: a call after growing | 3.5 | 12.2 | 67.9 | 67.9 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 57.2 | counter 10002: the instance is kept |
| 4. hello: grow 80 MiB, past wasm.max-memory | 3.5 | 12.2 | 27.9 | 27.9 | 1 | 22 | 0.0 | 0.3 | 0 | 0 | 0.0 | ERR function 'grow' failed: the instance would use 143982592 bytes of memory, more than wasm.max-memory (67108864) |
| 4. hello: the next call | 3.5 | 12.2 | 27.9 | 27.9 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 1.1 | counter 1: a new instance |
| 5. after WASM.DELETE hello | 3.4 | 12.2 | 27.6 | 27.6 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 |  |
| 2. JavaScript: during WASM.LOAD (sampled maxima) | 656.2 | 766.6 | 882.5 | 819.8 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 | 20 samples, 3.77 s |
| 2. JavaScript loaded | 50.1 | 766.6 | 819.8 | 817.5 | 1 | 7 | 10.7 | 90.2 | 0 | 1 | 7.3 |  |
| 3. JavaScript: 10,000 calls of counter | 50.1 | 766.6 | 818.4 | 818.4 | 1 | 7 | 10.7 | 90.2 | 0 | 1 | 9.1 | counter now 10001 |
| 5. after WASM.DELETE js | 38.2 | 766.6 | 720.8 | 684.3 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 |  |
| 5. after a second load and WASM.FLUSH | 53.9 | 766.6 | 749.9 | 749.9 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 |  |
| 6. after 5 JavaScript load and delete cycles | 62.0 | 906.2 | 1151.9 | 1152.0 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 |  |
| 6. after 10 JavaScript load and delete cycles | 58.7 | 906.2 | 1004.9 | 1004.9 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 |  |
| 6. after 20 JavaScript load and delete cycles | 66.3 | 906.2 | 943.9 | 944.0 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 |  |
| 7. maxmemory 130.3: hello grown 48 MiB | 56.2 | 906.2 | 873.3 | 873.3 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 49.2 | SET: OK |
| 7. maxmemory 130.3: during JavaScript WASM.LOAD | 678.0 | 906.2 | 932.7 | 1096.0 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 | 17 samples, 4.38 s; SET: 1 OK, 16 OOM |
| 7. maxmemory 130.3: JavaScript loaded | 74.1 | 906.2 | 1092.1 | 1092.1 | 1 | 7 | 10.7 | 90.2 | 0 | 1 | 7.3 | SET: OK |
| 8. reload server: hello loaded | 2.5 | 15.0 | 29.3 | 29.3 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 1.1 | module mappings 1 |
| 8. after 1 MODULE UNLOAD and LOAD cycles | 2.7 | 17.3 | 32.3 | 32.4 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 1.1 | module mappings 1 |
| 8. after 5 MODULE UNLOAD and LOAD cycles | 2.2 | 17.3 | 34.6 | 34.6 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 1.1 | module mappings 1 |
| 8. after 10 MODULE UNLOAD and LOAD cycles | 3.0 | 17.3 | 36.9 | 36.9 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 1.1 | module mappings 1 |
| 8. after 20 MODULE UNLOAD and LOAD cycles | 2.0 | 18.4 | 39.3 | 39.3 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 1.1 | module mappings 1 |
| 8. after 30 MODULE UNLOAD and LOAD cycles | 2.3 | 18.4 | 40.8 | 40.8 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 1.1 | module mappings 1 |
| 8. after a last MODULE UNLOAD | 1.0 | 18.4 | 40.4 | 40.4 | - | - | - | - | - | - | - | module mappings 1 |
