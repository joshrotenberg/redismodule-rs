# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [3.0.0] - unreleased

Released with redis-module 3.0.0.

### Changed

- The `api!` macro's required APIs panic with the missing API's name instead of unwrapping `None`.
- Edition 2024; `rust-version = "1.87"`.

## [2.0.8](https://github.com/RedisLabsModules/redismodule-rs/compare/redis-module-macros-internals-v2.0.7...redis-module-macros-internals-v2.0.8) - 2026-05-04

### Fixed

- CI: Revamp the release pipeline, moving over to Trusted Publishing for all crates in the repository.
