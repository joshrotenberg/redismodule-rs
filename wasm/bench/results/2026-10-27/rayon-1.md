2026-10-27, Darwin arm64, Redis server v=8.10.2 sha=00000000:1
wasmtime 36.0.19; RAYON_NUM_THREADS=1
module target/memory/libredis_module_wasm-redisalloc.dylib (12.0 MiB)
hello target/memory/hello.wasm (37686 bytes)
javascript target/memory/library.wasm (10.7 MiB)

mem_allocator libc, wasm_max_memory 64.0 MiB

| phase | used_memory | used_memory_peak | used_memory_rss | ps RSS | libraries | functions | component MiB | code MiB | compiling | instances | instance memory MiB | note |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 1. idle, module loaded | 1.0 | 1.0 | 8.2 | 8.2 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 |  |
| 2. hello loaded | 1.3 | 8.4 | 21.0 | 21.0 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 1.1 |  |
| 3. hello: 10,000 calls of counter | 1.4 | 8.4 | 21.1 | 21.1 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 1.1 | counter now 10001 |
| 4. hello: grow 16 MiB | 1.4 | 8.4 | 37.1 | 37.1 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 17.2 | memory.size 17.2 MiB |
| 4. hello: grow 40 MiB | 1.4 | 8.4 | 61.1 | 61.1 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 57.2 | memory.size 57.2 MiB |
| 4. hello: a call after growing | 1.4 | 8.4 | 61.1 | 61.1 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 57.2 | counter 10002: the instance is kept |
| 4. hello: grow 80 MiB, past wasm.max-memory | 1.4 | 8.4 | 21.1 | 21.1 | 1 | 22 | 0.0 | 0.3 | 0 | 0 | 0.0 | ERR function 'grow' failed: the instance would use 143982592 bytes of memory, more than wasm.max-memory (67108864) |
| 4. hello: the next call | 1.4 | 8.4 | 21.1 | 21.1 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 1.1 | counter 1: a new instance |
| 5. after WASM.DELETE hello | 1.3 | 8.4 | 20.8 | 20.8 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 |  |
| 2. JavaScript: during WASM.LOAD (sampled maxima) | 577.2 | 609.9 | 694.2 | 695.5 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 | 119 samples, 13.53 s |
| 2. JavaScript loaded | 19.2 | 609.9 | 694.2 | 694.2 | 1 | 7 | 10.7 | 90.2 | 0 | 1 | 7.3 |  |
| 3. JavaScript: 10,000 calls of counter | 19.2 | 609.9 | 695.3 | 695.3 | 1 | 7 | 10.7 | 90.2 | 0 | 1 | 9.1 | counter now 10001 |
| 5. after WASM.DELETE js | 7.3 | 609.9 | 597.7 | 597.7 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 |  |
| 5. after a second load and WASM.FLUSH | 7.3 | 609.9 | 817.1 | 817.1 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 |  |
| 6. after 5 JavaScript load and delete cycles | 7.3 | 611.8 | 815.5 | 815.7 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 |  |
| 7. maxmemory 71.3: hello grown 48 MiB | 7.5 | 611.8 | 864.2 | 864.2 | 1 | 22 | 0.0 | 0.3 | 0 | 1 | 49.2 | SET: OK |
| 7. maxmemory 71.3: during JavaScript WASM.LOAD | 503.1 | 611.8 | 818.3 | 914.8 | 0 | 0 | 0.0 | 0.0 | 0 | 0 | 0.0 | 107 samples, 13.42 s; SET: 8 OK, 99 OOM |
| 7. maxmemory 71.3: JavaScript loaded | 20.4 | 611.8 | 914.8 | 914.8 | 1 | 7 | 10.7 | 90.2 | 0 | 1 | 7.3 | SET: OK |
