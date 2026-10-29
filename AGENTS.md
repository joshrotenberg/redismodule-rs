# Agent instructions

This repository is a fork of RedisLabsModules/redismodule-rs: SDK 3.0 on
`master`, and the wasm host experiment on `experiment/wasm-host`.

- Read [CONTRIBUTING.md](CONTRIBUTING.md) before changing anything. It holds
  the rules: the issue backlog as the work list, `codex/<topic>` branches, a
  draft PR first, validation, independent review, when a PR may be merged, the
  wasm sync after each SDK merge, upstream tracking and pre-releases.
- Upstream RedisLabsModules/redismodule-rs is read-only: no pushes, pull
  requests, issues or comments there.
- `.cj/` holds project recipes for [clajent](https://github.com/joshrotenberg/clajent)'s
  `cj`: `cj run gates` runs the validation list and `cj run wasm-sync` does the
  wasm sync. `.cj/skills/fork-workflow/SKILL.md` describes them.
