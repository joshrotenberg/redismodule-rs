2026-10-27, Darwin arm64, Redis server v=8.10.2 sha=00000000:1
wasmtime 36.0.19; RAYON_NUM_THREADS=2
module target/memory/libredis_module_wasm-redisalloc.dylib (12.0 MiB)
hello target/memory/hello.wasm (37686 bytes)
javascript target/memory/library.wasm (10.7 MiB)

mem_allocator libc, wasm_max_memory 64.0 MiB

| phase | used_memory | used_memory_peak | used_memory_rss | ps RSS | libraries | functions | component MiB | code MiB | compiling | instances | instance memory MiB | note |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 1. idle, module loaded | 1.0 | 1.0 | 8.3 | 8.3 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 |  |
| 2. hello loaded | 1.5 | 9.9 | 23.0 | 23.0 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 1.1 |  |
| 3. hello: 10,000 calls of counter | 1.6 | 9.9 | 23.0 | 23.0 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 1.1 | counter now 10001 |
| 4. hello: grow 16 MiB | 1.6 | 9.9 | 39.0 | 39.0 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 17.2 | memory.size 17.2 MiB |
| 4. hello: grow 40 MiB | 1.6 | 9.9 | 63.0 | 63.0 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 57.2 | memory.size 57.2 MiB |
| 4. hello: a call after growing | 1.6 | 9.9 | 63.0 | 63.0 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 57.2 | counter 10002: the instance is kept |
| 4. hello: grow 80 MiB, past wasm.max-memory | 1.6 | 9.9 | 23.0 | 23.0 | 1 | 22 | 0.0 | 0.3 | 0 | 0 | 0.0 | ERR function 'grow' failed: the instance would use 143982592 bytes of memory, more than wasm.max-memory (67108864) |
| 4. hello: the next call | 1.6 | 9.9 | 23.1 | 23.1 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 1.1 | counter 1: a new instance |
| 5. after WASM.DELETE hello | 1.5 | 9.9 | 22.8 | 22.8 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 |  |
| 2. JavaScript: during WASM.LOAD (sampled maxima) | 544.5 | 610.3 | 822.6 | 777.3 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 | 64 samples, 7.45 s |
| 2. JavaScript loaded | 23.2 | 610.3 | 726.3 | 726.3 | 1 | 7 | 10.7 | 90.2 | 0 | 1 | 7.3 |  |
| 3. JavaScript: 10,000 calls of counter | 23.2 | 610.3 | 727.4 | 727.4 | 1 | 7 | 10.7 | 90.2 | 0 | 1 | 9.1 | counter now 10001 |
| 5. after WASM.DELETE js | 11.3 | 610.3 | 629.8 | 629.8 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 |  |
| 5. after a second load and WASM.FLUSH | 13.2 | 615.5 | 817.5 | 817.5 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 |  |
| 6. after 5 JavaScript load and delete cycles | 13.2 | 621.5 | 809.9 | 809.9 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 |  |
| 7. maxmemory 77.2: hello grown 48 MiB | 13.4 | 621.5 | 723.1 | 723.1 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 49.2 | SET: OK |
| 7. maxmemory 77.2: during JavaScript WASM.LOAD | 529.4 | 621.5 | 833.9 | 961.0 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 | 59 samples, 7.65 s; SET: 3 OK, 56 OOM |
| 7. maxmemory 77.2: JavaScript loaded | 29.7 | 621.5 | 939.4 | 939.4 | 1 | 7 | 10.7 | 90.2 | 0 | 1 | 7.3 | SET: OK |
