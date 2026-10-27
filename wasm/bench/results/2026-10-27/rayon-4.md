2026-10-27, Darwin arm64, Redis server v=8.10.2 sha=00000000:1
wasmtime 36.0.19; RAYON_NUM_THREADS=4
module target/memory/libredis_module_wasm-redisalloc.dylib (12.0 MiB)
hello target/memory/hello.wasm (37686 bytes)
javascript target/memory/library.wasm (10.7 MiB)

mem_allocator libc, wasm_max_memory 64.0 MiB

| phase | used_memory | used_memory_peak | used_memory_rss | ps RSS | libraries | functions | component MiB | code MiB | compiling | instances | instance memory MiB | note |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 1. idle, module loaded | 1.0 | 1.0 | 8.3 | 8.3 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 |  |
| 2. hello loaded | 1.7 | 15.1 | 26.7 | 26.7 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 1.1 |  |
| 3. hello: 10,000 calls of counter | 1.7 | 15.1 | 26.8 | 26.8 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 1.1 | counter now 10001 |
| 4. hello: grow 16 MiB | 1.7 | 15.1 | 42.8 | 42.8 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 17.2 | memory.size 17.2 MiB |
| 4. hello: grow 40 MiB | 1.7 | 15.1 | 66.8 | 66.8 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 57.2 | memory.size 57.2 MiB |
| 4. hello: a call after growing | 1.7 | 15.1 | 66.8 | 66.8 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 57.2 | counter 10002: the instance is kept |
| 4. hello: grow 80 MiB, past wasm.max-memory | 1.7 | 15.1 | 26.7 | 26.7 | 1 | 22 | 0.0 | 0.3 | 0 | 0 | 0.0 | ERR function 'grow' failed: the instance would use 143982592 bytes of memory, more than wasm.max-memory (67108864) |
| 4. hello: the next call | 1.7 | 15.1 | 26.8 | 26.8 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 1.1 | counter 1: a new instance |
| 5. after WASM.DELETE hello | 1.7 | 15.1 | 26.5 | 26.5 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 |  |
| 2. JavaScript: during WASM.LOAD (sampled maxima) | 532.4 | 619.4 | 850.2 | 751.8 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 | 38 samples, 4.68 s |
| 2. JavaScript loaded | 34.2 | 619.4 | 783.8 | 783.8 | 1 | 7 | 10.7 | 90.2 | 0 | 1 | 7.3 |  |
| 3. JavaScript: 10,000 calls of counter | 34.2 | 619.4 | 784.8 | 784.8 | 1 | 7 | 10.7 | 90.2 | 0 | 1 | 9.1 | counter now 10001 |
| 5. after WASM.DELETE js | 22.3 | 619.4 | 687.2 | 687.2 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 |  |
| 5. after a second load and WASM.FLUSH | 24.9 | 718.4 | 902.2 | 902.2 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 |  |
| 6. after 5 JavaScript load and delete cycles | 25.7 | 732.6 | 987.0 | 987.0 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 |  |
| 7. maxmemory 89.7: hello grown 48 MiB | 25.8 | 732.6 | 1035.4 | 999.5 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 49.2 | SET: OK |
| 7. maxmemory 89.7: during JavaScript WASM.LOAD | 596.7 | 732.6 | 973.2 | 925.2 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 | 28 samples, 4.40 s; SET: 1 OK, 27 OOM |
| 7. maxmemory 89.7: JavaScript loaded | 38.2 | 732.6 | 1019.8 | 1019.8 | 1 | 7 | 10.7 | 90.2 | 0 | 1 | 7.3 | SET: OK |
