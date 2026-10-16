# F26 bounded native source excerpts

Selected source lines only; omitted lines are not adjacent. This is evidence, not executable code.
The full pinned module.c/t_hash.c blobs and SHA-256 receipts are in target/f26/source/.
Redis project source attribution and upstream licenses apply; no server patch is proposed.

## Redis 4.0.14

[module.c](https://github.com/redis/redis/blob/18572a921a8d17881c3de1fc71f56362d63010d9/src/module.c), commit `18572a921a8d17881c3de1fc71f56362d63010d9`, blob `8eb3f8aa5b065a18182ba7e6fcff0a34d7258d30`.
SHA-256 `5035bb99d59ef78c1d5c0c6ca381f45bb7fd220dc4e609cb611df7d21a1ebe3a`.
t_hash.c blob `fa3a893a6f042e70ce198d89920d699135908ced`, SHA-256 `12b5233172d4b742944628a0de7aa3a4f045cce593b66599839aca547e73b960`, hashTypeSet at line 202.

```c
2173: int RM_HashSet(RedisModuleKey *key, int flags, ...) {
2175:     if (!(key->mode & REDISMODULE_WRITE)) return 0;
2176:     if (key->value && key->value->type != OBJ_HASH) return 0;
2179:     int updated = 0;
2185:             char *cfield = va_arg(ap,char*);
2189:             field = va_arg(ap,RedisModuleString*);
2207:             updated += hashTypeDelete(key->value, field->ptr);
2212:         int low_flags = HASH_SET_COPY;
2218:         updated += hashTypeSet(key->value, field->ptr, value->ptr, low_flags);
2228:     moduleDelKeyIfEmpty(key);
2229:     return updated;
```

## Redis 5.0.14

[module.c](https://github.com/redis/redis/blob/704ba5f5b22ae1ecafbcfb7a3258311c27ff94ff/src/module.c), commit `704ba5f5b22ae1ecafbcfb7a3258311c27ff94ff`, blob `5218071410da9a0475470f31954f464aab6ed136`.
SHA-256 `ca6a86653fc8790fd26a7a427f99ad8a15f899aa1431a99d00b67dcb49d3480e`.
t_hash.c blob `109522c1322f8ac59a97cc3cc499ba4edf9168a6`, SHA-256 `b019ba8a4bc0dfea7bee0d6932790d70b5f63a71cb3331e429dfa061e64ff353`, hashTypeSet at line 207.

```c
2419: int RM_HashSet(RedisModuleKey *key, int flags, ...) {
2421:     if (!(key->mode & REDISMODULE_WRITE)) return 0;
2422:     if (key->value && key->value->type != OBJ_HASH) return 0;
2425:     int updated = 0;
2431:             char *cfield = va_arg(ap,char*);
2435:             field = va_arg(ap,RedisModuleString*);
2453:             updated += hashTypeDelete(key->value, field->ptr);
2458:         int low_flags = HASH_SET_COPY;
2467:         updated += hashTypeSet(key->value, field->ptr, value->ptr, low_flags);
2477:     moduleDelKeyIfEmpty(key);
2478:     return updated;
```

## Redis 6.0.20

[module.c](https://github.com/redis/redis/blob/de0d9632b52849d9b7ea52408b1d681d771c5b46/src/module.c), commit `de0d9632b52849d9b7ea52408b1d681d771c5b46`, blob `93517494f04439b7a7ee8d2ed22dab855cebe7e7`.
SHA-256 `badb47266eb43fc2c44bd7e25d490d8633b3c91a5daee095e1532172f924990f`.
t_hash.c blob `13e65502f1454068f5f54722770ef0379f3e8142`, SHA-256 `3eb744d027727b8be50df97ae408043b1005544515882c2d59ca58c1d1f0c446`, hashTypeSet at line 207.

```c
2892: int RM_HashSet(RedisModuleKey *key, int flags, ...) {
2894:     if (!(key->mode & REDISMODULE_WRITE)) return 0;
2895:     if (key->value && key->value->type != OBJ_HASH) return 0;
2898:     int updated = 0;
2904:             char *cfield = va_arg(ap,char*);
2908:             field = va_arg(ap,RedisModuleString*);
2926:             updated += hashTypeDelete(key->value, field->ptr);
2931:         int low_flags = HASH_SET_COPY;
2940:         updated += hashTypeSet(key->value, field->ptr, value->ptr, low_flags);
2950:     moduleDelKeyIfEmpty(key);
2951:     return updated;
```

## Redis 6.2.19

[module.c](https://github.com/redis/redis/blob/fa00bd2fff1533ad6e8483d9ce8868f383df2fbb/src/module.c), commit `fa00bd2fff1533ad6e8483d9ce8868f383df2fbb`, blob `0ce9b369ab7de3d45218baa0737f0ecdcdac1dce`.
SHA-256 `684078d49820698e2dfadc8bf311def986e87fd0706f7a2b61bdd568bcd0a578`.
t_hash.c blob `7e082b3ad93d6f917d11030bd8fcee08c785f4d8`, SHA-256 `eda036ac70c160cd3960abfc7724f3925cddcc0ba1e3170f069c30e58b0c7665`, hashTypeSet at line 207.

```c
3132: int RM_HashSet(RedisModuleKey *key, int flags, ...) {
3134:     if (!key || (flags & ~(REDISMODULE_HASH_NX |
3138:         errno = EINVAL;
3139:         return 0;
3141:         errno = ENOTSUP;
3142:         return 0;
3143:     } else if (!(key->mode & REDISMODULE_WRITE)) {
3144:         errno = EBADF;
3145:         return 0;
3149:     int count = 0;
3155:             char *cfield = va_arg(ap,char*);
3159:             field = va_arg(ap,RedisModuleString*);
3177:             count += hashTypeDelete(key->value, field->ptr);
3182:         int low_flags = HASH_SET_COPY;
3191:         int updated = hashTypeSet(key->value, field->ptr, value->ptr, low_flags);
3192:         count += (flags & REDISMODULE_HASH_COUNT_ALL) ? 1 : updated;
3202:     moduleDelKeyIfEmpty(key);
3203:     if (count == 0) errno = ENOENT;
3204:     return count;
```

## Redis 7.2.8

[module.c](https://github.com/redis/redis/blob/31d93c592811dce16b2f75222afa6babc5eb9a55/src/module.c), commit `31d93c592811dce16b2f75222afa6babc5eb9a55`, blob `d58918201d76373cafde53af06ea59e797f45f4f`.
SHA-256 `75b13cc5f332c3598a8d48494d92a4413b5c4dad77b063fc5d90e14cf1a822bc`.
t_hash.c blob `b199d8c69c587e6b9e37d199509cd15bb6ce3259`, SHA-256 `e506d6f1ce57ea8fa7aa13406865fe1b5c0816357b22ffe97e8b969abe4d24f6`, hashTypeSet at line 200.

```c
5208: int RM_HashSet(RedisModuleKey *key, int flags, ...) {
5210:     if (!key || (flags & ~(REDISMODULE_HASH_NX |
5214:         errno = EINVAL;
5215:         return 0;
5217:         errno = ENOTSUP;
5218:         return 0;
5219:     } else if (!(key->mode & REDISMODULE_WRITE)) {
5220:         errno = EBADF;
5221:         return 0;
5225:     int count = 0;
5231:             char *cfield = va_arg(ap,char*);
5235:             field = va_arg(ap,RedisModuleString*);
5253:             count += hashTypeDelete(key->value, field->ptr);
5258:         int low_flags = HASH_SET_COPY;
5267:         int updated = hashTypeSet(key->value, field->ptr, value->ptr, low_flags);
5268:         count += (flags & REDISMODULE_HASH_COUNT_ALL) ? 1 : updated;
5278:     moduleDelKeyIfEmpty(key);
5279:     if (count == 0) errno = ENOENT;
5280:     return count;
```

## Redis 8.10.2

[module.c](https://github.com/redis/redis/blob/498ecd0d6d007db11ddb3aea9428552598a78622/src/module.c), commit `498ecd0d6d007db11ddb3aea9428552598a78622`, blob `ff646d5934f143699434f34980f7939bc2e5c56d`.
SHA-256 `bcfad92db0a7be63df49ed981b4ad224b31ab4ff0fdf9752d60b6d0a519d3329`.
t_hash.c blob `09a4632832b13f2d52ef8a93988ce42057864cf7`, SHA-256 `b45825a20e1acf92dd1dab67072f4cf8de52f5b805b5ba899753fa0afb725358`, hashTypeSet at line 1899.

```c
5713: int RM_HashSet(RedisModuleKey *key, int flags, ...) {
5716:     if (!key || (flags & ~(REDISMODULE_HASH_NX |
5720:         errno = EINVAL;
5721:         return 0;
5723:         errno = ENOTSUP;
5724:         return 0;
5725:     } else if (!(key->mode & REDISMODULE_WRITE)) {
5726:         errno = EBADF;
5727:         return 0;
5733:     int count = 0;
5739:             char *cfield = va_arg(ap,char*);
5743:             field = va_arg(ap,RedisModuleString*);
5757:              * yet let the operation fail. Note that moduleDelKeyIfEmpty()
5777:             count += hashTypeDelete(key->kv, field->ptr);
5784:         int low_flags = HASH_SET_COPY;
5795:         int updated = hashTypeSet(key->db, key->kv, field->ptr, value->ptr, low_flags);
5798:         count += (flags & REDISMODULE_HASH_COUNT_ALL) ? 1 : updated;
5811:     moduleDelKeyIfEmpty(key);
5812:     if (count == 0) errno = ENOENT;
5813:     return count;
```

All six hashTypeSet bodies initialize update to 0, assign 1 when replacing an existing field and return update. Caller retains input strings under HASH_SET_COPY. This restricted result matches across the selected releases; error reporting differs as described in the specification.
