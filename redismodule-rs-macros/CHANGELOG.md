# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [3.0.0] - unreleased

Released with redis-module 3.0.0; see its changelog and docs/MIGRATING-3.0.md.

### Added

- `#[shutdown_event_handler]`, for the shutdown server event.

### Changed

- `#[command]` handlers that panic reply `ERR command panicked: <message>` instead of aborting the server.
- Modules no longer need a direct `linkme` dependency for the registration attributes.
- The generated code follows redis-module 3.0's `RedisString` and `Context` APIs.
- Edition 2024; `rust-version = "1.87"`. Depends on redis-module-common 0.1.2.

## [2.0.8](https://github.com/RedisLabsModules/redismodule-rs/compare/redis-module-macros-v2.0.7...redis-module-macros-v2.0.8) - 2026-05-04

### Fixed

- CI: Revamp the release pipeline, moving over to Trusted Publishing for all crates in the repository.
