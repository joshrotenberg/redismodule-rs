//! What a function's flags mean, as for a Lua function: the checks before it
//! starts, and how the commands it calls run.
//!
//! The checks follow Redis's `scriptPrepareForRun`, in its order and with its
//! error codes; the messages say "function" where Lua's say "script". Two of
//! Lua's checks have no module API: whether writes are denied by a disk error
//! (`MISCONF`) or by `min-replicas-to-write` (`NOREPLICAS`). For those a function
//! that writes starts, and its first write command fails with that error.

use redis_module::{Context, ContextFlags, RedisError, RedisString, RedisValue, cluster};

use crate::host::FunctionFlags;

/// How the commands a guest calls run, for one call.
#[derive(Clone, Copy, Default)]
pub(crate) struct Mode {
    /// A library is loading: guest code may not call Redis.
    pub(crate) loading: bool,
    /// Write commands fail: the function has `no-writes`, or runs from
    /// `WASM.CALL_RO`.
    pub(crate) read_only: bool,
    /// Commands are not checked against `maxmemory`: `allow-oom`.
    pub(crate) allow_oom: bool,
    /// On a cluster node without `allow-cross-slot-keys`: the keys of every
    /// command must hash to one slot.
    pub(crate) one_slot: bool,
    /// That slot, once known: the declared keys', or the first command's.
    pub(crate) slot: Option<u16>,
}

impl Mode {
    /// While a library loads: instantiating it and listing its functions.
    pub(crate) fn loading() -> Self {
        Self {
            loading: true,
            ..Self::default()
        }
    }
}

/// Checks that a function with `flags` may start, as `scriptPrepareForRun`
/// does for Lua, and returns how its calls run. `read_only` is set for
/// `WASM.CALL_RO`.
pub(crate) fn check(
    ctx: &Context,
    flags: FunctionFlags,
    read_only: bool,
    keys: &[Vec<u8>],
) -> Result<Mode, RedisError> {
    let state = ctx.get_flags();
    let cluster = state.contains(ContextFlags::CLUSTER);
    // The primary's link and the AOF replay commands that already ran.
    let obey = state.intersects(ContextFlags::REPLICATED | ContextFlags::LOADING);

    if flags.contains(FunctionFlags::NO_CLUSTER) && cluster {
        return Err(RedisError::Str(
            "ERR Can not run function on cluster, 'no-cluster' flag is set.",
        ));
    }
    if !flags.contains(FunctionFlags::ALLOW_STALE) && running_stale(ctx, state) {
        return Err(RedisError::Str(
            "MASTERDOWN Link with MASTER is down, replica-serve-stale-data is set to 'no' \
             and 'allow-stale' flag is not set on the function.",
        ));
    }
    let no_writes = flags.contains(FunctionFlags::NO_WRITES);
    if !no_writes {
        if state.contains(ContextFlags::SLAVE | ContextFlags::READONLY) && !obey {
            return Err(RedisError::Str(
                "READONLY Can not run function with write flag on readonly replica",
            ));
        }
        if read_only {
            return Err(RedisError::Str(
                "ERR Can not execute a function with write flag using WASM.CALL_RO.",
            ));
        }
        if !flags.contains(FunctionFlags::ALLOW_OOM) && state.contains(ContextFlags::OOM) && !obey {
            return Err(RedisError::Str(
                "OOM allow-oom flag is not set on the function, \
                 can not run it when used memory > 'maxmemory'",
            ));
        }
    }

    let one_slot = cluster && !obey && !flags.contains(FunctionFlags::ALLOW_CROSS_SLOT_KEYS);
    Ok(Mode {
        loading: false,
        read_only: no_writes || read_only,
        allow_oom: flags.contains(FunctionFlags::ALLOW_OOM),
        one_slot,
        // Redis has already refused declared keys in different slots.
        slot: if one_slot {
            keys.first().map(|key| cluster::key_slot(key))
        } else {
            None
        },
    })
}

/// Whether this is a replica that has lost its primary and must not serve
/// stale data.
fn running_stale(ctx: &Context, state: ContextFlags) -> bool {
    if !state.contains(ContextFlags::SLAVE | ContextFlags::REPLICA_IS_STALE) {
        return false;
    }
    // No context flag says whether stale data may be served; this runs only
    // while the link is down.
    match ctx.call("CONFIG", &["GET", "replica-serve-stale-data"]) {
        Ok(RedisValue::Array(values)) => values.iter().any(
            |value| matches!(value, RedisValue::BulkString(text) | RedisValue::SimpleString(text) if text == "no"),
        ),
        _ => false,
    }
}

/// The hash slot of the keys `command` would use with `args`, or `None` if it
/// uses none or Redis does not know it. Redis refuses a command whose own keys
/// are in different slots, so the first key's slot stands for all of them.
pub(crate) fn command_slot(ctx: &Context, command: &str, args: &[&[u8]]) -> Option<u16> {
    let argv: Vec<RedisString> = std::iter::once(command.as_bytes())
        .chain(args.iter().copied())
        .map(|arg| ctx.create_string(arg))
        .collect();
    let first = *ctx.get_command_keys(&argv).ok()?.first()?;
    Some(cluster::key_slot(argv.get(first)?.as_slice()))
}

/// The flags in the order and spelling of `FUNCTION LIST`.
const NAMES: [(FunctionFlags, &str); 5] = [
    (FunctionFlags::NO_WRITES, "no-writes"),
    (FunctionFlags::ALLOW_OOM, "allow-oom"),
    (FunctionFlags::ALLOW_STALE, "allow-stale"),
    (FunctionFlags::NO_CLUSTER, "no-cluster"),
    (
        FunctionFlags::ALLOW_CROSS_SLOT_KEYS,
        "allow-cross-slot-keys",
    ),
];

/// The names of the flags set in `flags`, for `WASM.LIST`.
pub(crate) fn names(flags: FunctionFlags) -> impl Iterator<Item = &'static str> {
    NAMES
        .into_iter()
        .filter(move |(flag, _)| flags.contains(*flag))
        .map(|(_, name)| name)
}

/// The flags as the RDB keeps them: bit i is the i-th flag of `NAMES`, so the
/// format does not depend on the bindings.
pub(crate) fn to_bits(flags: FunctionFlags) -> u64 {
    NAMES
        .iter()
        .enumerate()
        .filter(|(_, (flag, _))| flags.contains(*flag))
        .fold(0, |bits, (i, _)| bits | 1 << i)
}

pub(crate) fn from_bits(bits: u64) -> FunctionFlags {
    NAMES
        .iter()
        .enumerate()
        .filter(|(i, _)| bits & 1 << i != 0)
        .fold(FunctionFlags::empty(), |flags, (_, (flag, _))| {
            flags | *flag
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_round_trip_through_bits_and_names() {
        let flags = FunctionFlags::NO_WRITES | FunctionFlags::ALLOW_CROSS_SLOT_KEYS;
        assert_eq!(to_bits(flags), 0b10001);
        assert_eq!(from_bits(to_bits(flags)), flags);
        assert_eq!(
            names(flags).collect::<Vec<_>>(),
            ["no-writes", "allow-cross-slot-keys"]
        );
        assert_eq!(from_bits(0), FunctionFlags::empty());
    }
}
