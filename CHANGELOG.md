# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `RedisKey::hash_get_by_string` and `RedisKeyWritable::hash_get_by_string`: `HGET` with the field passed as an existing `RedisString`, avoiding the per-call `CString` and the transient field object Redis creates for C-string fields.

### Changed (local fork F09; unpublished)

- `RedisString` holds a private non-null native reference without a stored context;
  replace `.inner` reads with `.as_ptr()`. No safe field fabrication/mutation.
- Raw `new`, `from_redis_module_string` and `decode_args` require unsafe;
  adoption is no longer const. Explicit `from_owned_ptr`/`retain_from_ptr`
  distinguish transferred ownership from borrowed acquisition.
- `create`, `create_from_slice` and `from_raw_parts` preserve their byte/text
  conversion policy but create independent null-context owners. Added `from_bytes`.
- `Context::get_current_user` returns `Option<RedisString>`. Ordinary command
  results stay present; native context/host legality remains required.
- `KeysCursor::scan` takes `&mut F`; `ScanKeyCursor::scan/for_each` expose optional
  values (sets have None), use exclusive mutable state and true producer context.

### Fixed (local fork F09; unpublished)

- INFO fields adopt one owned reference with a null allocation context, following
  upstream PRs 487/488 with attribution in the F09 record. Other native-owned
  string producers resolve AM while their actual producer context is valid.
- Scan callbacks balance temporary retained string references. Consuming outbound
  transfer and defrag replacement no longer require writable/null owner storage.
- Corrected extra retains can make fresh producer strings appendable; retained
  aliases still follow native shared-string rejection. An existing configuration
  getter leak is documented as a separate follow-up, not fixed by this chunk.

## [2.0.8](https://github.com/RedisLabsModules/redismodule-rs/compare/redis-module-v2.0.7...redis-module-v2.0.8) - 2026-05-04

### Fixed

- CI: Revamp the release pipeline, moving over to Trusted Publishing for all crates in the repository.
