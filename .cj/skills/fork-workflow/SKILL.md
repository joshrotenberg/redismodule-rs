---
name: fork-workflow
description: Working on this redismodule-rs fork with cj. Where its rules are, the project recipes (gates, wasm-sync), and which steps stay with the owner.
---

# Working on this fork

The rules are in `CONTRIBUTING.md`: the work list, branches and remotes, the
draft-PR-first flow, validation, the wasm sync, upstream tracking and
pre-releases. Read it before changing anything. This skill adds how the
project recipes fit in.

## Recipes

- `cj run gates [--dir PATH] [--server BIN] [--only STEPS]` runs the
  validation list at both floors and prints each step's result, test counts
  and log path. Put the result in the PR body.
- `cj run wasm-sync /abs/path --message "chore: sync wasm host with ..."`
  merges master into `experiment/wasm-host` in a new worktree and runs
  `wasm/ci/check.sh`. Then write the `wasm/TRACKING.md` entry, push, and
  confirm the push CI.
- `cj run review-pr N` is a read-only provider review. It does not replace the
  independent review `CONTRIBUTING.md` asks for, and its findings need
  checking against the code.

## Stays with the owner

- Merges, which need the owner's approval for each PR.
- Publishing to crates.io, tags and releases.
- Anything on upstream RedisLabsModules, which is read-only.

## In a sandbox

- The integration tests and `check.sh` bind local ports, and `check.sh` needs
  the network, so those steps need a sandbox that allows them.
- The `cj` launcher writes to `java.io.tmpdir`; in a sandbox that only allows
  `$TMPDIR`, run it as `bb -Djava.io.tmpdir="$TMPDIR" ~/.local/bin/cj ...`
  (clajent issue 76).
