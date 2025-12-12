# Release Process

This repository uses [release-plz](https://release-plz.dev/) to automate releases and [Trusted Publishing](https://doc.rust-lang.org/cargo/reference/registries.html#trusted-publishing) to securely publish to crates.io.

## How Releases Work

1. **Merge PRs to `master`** - When PRs are merged, release-plz analyzes the commits
2. **Release PR** - release-plz opens a PR with version bumps and changelog updates
3. **Merge Release PR** - When the release PR is merged, release-plz publishes to crates.io and creates GitHub releases

## Crates Published

All crates share the same version and are published together:

| Crate | Description |
|-------|-------------|
| `redis-module` | Main crate for building Redis modules |
| `redis-module-macros` | Proc macros for Redis module development |
| `redis-module-macros-internals` | Internal macro implementation |

## Prerequisites: Trusted Publishing Setup

Before the release workflow can publish to crates.io, Trusted Publishing must be configured for each crate. This is a **one-time setup** that must be done by a crates.io owner.

### Step-by-Step Setup

For each of the three crates (`redis-module`, `redis-module-macros`, `redis-module-macros-internals`):

1. Go to crates.io and log in
2. Navigate to the crate's settings page:
   - https://crates.io/crates/redis-module/settings
   - https://crates.io/crates/redis-module-macros/settings
   - https://crates.io/crates/redis-module-macros-internals/settings
3. Scroll to "Trusted Publishing"
4. Click "Add a publisher"
5. Fill in:
   - **Repository owner**: `RedisLabsModules`
   - **Repository name**: `redismodule-rs`
   - **Workflow filename**: `release.yml`
   - **Environment**: (leave blank)
6. Click "Add"

### Verification

After setup, the "Trusted Publishing" section for each crate should show:

```
GitHub Actions
Repository: RedisLabsModules/redismodule-rs
Workflow: release.yml
```

## Manual Releases (Emergency)

If you need to publish manually (not recommended):

```bash
# Ensure you're logged into crates.io
cargo login

# Publish in order (internals first, then main, then macros)
cd redismodule-rs-macros-internals && cargo publish
cd .. && cargo publish
cd redismodule-rs-macros && cargo publish
```

## Version Management

- Versions are managed in `Cargo.toml` under `[workspace.package]`
- All crates inherit the workspace version
- release-plz handles version bumps based on conventional commits
- Changelogs are generated automatically

## Troubleshooting

### "Token not authorized" error
Trusted Publishing is not configured for one or more crates. See setup instructions above.

### "Crate already exists" error
The version has already been published. Check crates.io for the current version.

### CI failures
Check the GitHub Actions logs at: https://github.com/RedisLabsModules/redismodule-rs/actions
