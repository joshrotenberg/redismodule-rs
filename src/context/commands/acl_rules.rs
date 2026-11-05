//! Refuses `MODULE UNLOAD` while an ACL rule names one of this module's
//! commands.
//!
//! From Redis 7.2, the unload removes the module's commands and then
//! reapplies every user's command rules, asserting that each one applies. A
//! rule that names a removed command does not, and the server stops. Before
//! 7.2 the unload leaves the user's bit for the command behind, and the next
//! description of that user, as `ACL LIST` gives, stops the server instead.
//! Rules on ACL categories are safe: a category a module adds stays
//! registered after the unload.
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::{RedisError, RedisValue, RuntimeContext};

// The top-level commands this load registered through the SDK. A subcommand
// rule names its container. Registration and unload hold the Redis lock.
static REGISTERED: Mutex<Vec<Box<[u8]>>> = Mutex::new(Vec::new());

fn registered() -> MutexGuard<'static, Vec<Box<[u8]>>> {
    REGISTERED.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(super) fn record_registered_command(name: &[u8]) {
    registered().push(name.into());
}

/// Internal binding for `redis_module!`: forgets the registered command names
/// when a load starts or fails, and when an unload goes ahead.
#[doc(hidden)]
pub fn forget_registered_commands() {
    *registered() = Vec::new();
}

/// Internal binding for `redis_module!`: refuses the unload while an ACL rule
/// names a command this module registered.
///
/// The rules come from `ACL LIST`. A rule names a command with `+` or `-`,
/// and a subcommand or first argument with `+cmd|sub` or `-cmd|sub`, at the
/// user's top level or inside a selector. Names match ASCII
/// case-insensitively, as Redis looks commands up. When the rules cannot be
/// read, the unload is refused too.
#[doc(hidden)]
pub fn refuse_unload_for_acl_rules(ctx: &RuntimeContext) -> Result<(), RedisError> {
    if registered().is_empty() {
        return Ok(());
    }
    let unreadable = |detail: String| {
        RedisError::String(format!(
            "ERR cannot read the ACL rules ({detail}), so the unload is refused: \
             a rule that names a command of this module would stop the server"
        ))
    };
    let users = match ctx.call("ACL", &["LIST"]) {
        Ok(RedisValue::Array(users)) => users,
        Ok(other) => return Err(unreadable(format!("unexpected reply {other:?}"))),
        Err(error) => return Err(unreadable(error.to_string())),
    };
    let names = registered();
    for user in &users {
        let line = match user {
            RedisValue::BulkString(text) | RedisValue::SimpleString(text) => text.as_bytes(),
            RedisValue::StringBuffer(bytes) => bytes.as_slice(),
            other => return Err(unreadable(format!("unexpected user entry {other:?}"))),
        };
        let found = rule_naming_a_command(line, &names)
            .ok_or_else(|| unreadable(format!("unexpected user entry {user:?}")))?;
        if let Some((user, rule)) = found {
            return Err(RedisError::String(format!(
                "ERR ACL user {} has the rule {}, which names a command of this module; \
                 remove the rule before unloading the module",
                String::from_utf8_lossy(user),
                String::from_utf8_lossy(rule)
            )));
        }
    }
    Ok(())
}

/// The user and first rule in an `ACL LIST` entry that name one of `names`.
/// `None` when the entry is not `user <name> <rules...>`.
fn rule_naming_a_command<'a>(
    line: &'a [u8],
    names: &[Box<[u8]>],
) -> Option<Option<(&'a [u8], &'a [u8])>> {
    let mut tokens = line.split(|byte| *byte == b' ').filter(|t| !t.is_empty());
    if tokens.next()? != b"user" {
        return None;
    }
    let user = tokens.next()?;
    // A selector is `(rules...)` and its command rules come last; key and
    // channel patterns, which can hold parentheses, start with ~, % or &.
    Some(tokens.find_map(|token| {
        let token = token.strip_prefix(b"(").unwrap_or(token);
        token
            .strip_suffix(b")")
            .into_iter()
            .chain([token])
            .find(|rule| names_a_command(rule, names))
            .map(|rule| (user, rule))
    }))
}

fn names_a_command(rule: &[u8], names: &[Box<[u8]>]) -> bool {
    let Some(body) = rule.strip_prefix(b"+").or_else(|| rule.strip_prefix(b"-")) else {
        return false;
    };
    if body.starts_with(b"@") {
        return false;
    }
    let command = body.split(|byte| *byte == b'|').next().unwrap_or(body);
    names
        .iter()
        .any(|name| name.eq_ignore_ascii_case(command) || name.eq_ignore_ascii_case(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Vec<Box<[u8]>> {
        vec![b"hello.mul".as_slice().into(), b"Sub.Kv".as_slice().into()]
    }

    fn found(line: &str) -> Option<(String, String)> {
        rule_naming_a_command(line.as_bytes(), &names())
            .expect("a user entry")
            .map(|(user, rule)| {
                (
                    String::from_utf8_lossy(user).into_owned(),
                    String::from_utf8_lossy(rule).into_owned(),
                )
            })
    }

    #[test]
    fn rules_that_name_a_command_are_found() {
        for (line, rule) in [
            (
                "user u on nopass resetchannels -@all +hello.mul",
                "+hello.mul",
            ),
            (
                "user u on nopass resetchannels +@all -HELLO.MUL",
                "-HELLO.MUL",
            ),
            ("user u on nopass -@all +hello.mul|2", "+hello.mul|2"),
            ("user u on nopass -@all +sub.kv|get", "+sub.kv|get"),
            ("user u on nopass +@all -sub.kv|set", "-sub.kv|set"),
            ("user u on nopass -@all +sub.kv +get", "+sub.kv"),
            (
                "user u on nopass resetchannels -@all (~* resetchannels -@all +hello.mul)",
                "+hello.mul",
            ),
            (
                "user u on nopass -@all (~k resetchannels +@all -sub.kv|set +get)",
                "-sub.kv|set",
            ),
            (
                "user u on nopass -@all (~* resetchannels +sub.kv|get)",
                "+sub.kv|get",
            ),
            ("user u on nopass -@all (+hello.mul)", "+hello.mul"),
        ] {
            assert_eq!(found(line), Some(("u".into(), rule.into())), "{line}");
        }
    }

    #[test]
    fn other_rules_are_not_found() {
        for line in [
            "user default on nopass sanitize-payload ~* &* +@all",
            "user u on nopass -@all +@read +@hello.mul +get +hello.mulx -hello",
            "user u on nopass ~hello.mul &+hello.mul %R~+hello.mul -@all",
            "user u on nopass -@all (~(+hello.mul) resetchannels +get)",
            "user +hello.mul on nopass -@all",
        ] {
            assert_eq!(found(line), None, "{line}");
        }
    }

    #[test]
    fn other_entries_are_unexpected() {
        assert_eq!(rule_naming_a_command(b"", &names()), None);
        assert_eq!(
            rule_naming_a_command(b"default on +hello.mul", &names()),
            None
        );
        assert_eq!(rule_naming_a_command(b"user", &names()), None);
    }
}
