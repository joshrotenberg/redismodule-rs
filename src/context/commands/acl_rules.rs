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
/// case-insensitively, as Redis looks commands up.
///
/// When the rules cannot be read, for example because `ACL` is renamed, this
/// logs a warning and lets the unload go ahead. Refusing would not help:
/// Redis also unloads a module whose `MODULE LOADEX` configuration it
/// rejected, asserts that the unload succeeds, and no rule can name commands
/// that did not exist before that load.
#[doc(hidden)]
pub fn refuse_unload_for_acl_rules(ctx: &RuntimeContext) -> Result<(), RedisError> {
    if registered().is_empty() {
        return Ok(());
    }
    let unchecked = |detail: &str| {
        ctx.log_warning(&format!(
            "cannot read the ACL rules ({detail}); unloading without checking \
             whether a rule names a command of this module"
        ));
    };
    let users = match ctx.call("ACL", &["LIST"]) {
        Ok(RedisValue::Array(users)) => users,
        Ok(_) => {
            unchecked("ACL LIST did not reply with an array");
            return Ok(());
        }
        Err(error) => {
            unchecked(&error.to_string());
            return Ok(());
        }
    };
    let names = registered();
    for (index, user) in users.iter().enumerate() {
        let line = match user {
            RedisValue::BulkString(text) | RedisValue::SimpleString(text) => Some(text.as_bytes()),
            RedisValue::StringBuffer(bytes) => Some(bytes.as_slice()),
            _ => None,
        };
        // The entry holds password hashes, so the warning does not quote it.
        let Some(found) = line.and_then(|line| rule_naming_a_command(line, &names)) else {
            unchecked(&format!("ACL LIST entry {index} is not a user"));
            continue;
        };
        if let Some(found) = found {
            return Err(RedisError::String(format!(
                "ERR ACL user {} has the rule {}{}, which names a command of this module; \
                 remove the rule before unloading the module",
                String::from_utf8_lossy(found.user),
                String::from_utf8_lossy(found.rule),
                if found.in_selector {
                    " in a selector"
                } else {
                    ""
                },
            )));
        }
    }
    Ok(())
}

#[derive(Debug, PartialEq)]
struct Found<'a> {
    user: &'a [u8],
    rule: &'a [u8],
    in_selector: bool,
}

/// The user and first rule in an `ACL LIST` entry that name one of `names`.
/// `None` when the entry is not `user <name> <rules...>`.
fn rule_naming_a_command<'a>(line: &'a [u8], names: &[Box<[u8]>]) -> Option<Option<Found<'a>>> {
    let mut tokens = line.split(|byte| *byte == b' ').filter(|t| !t.is_empty());
    if tokens.next()? != b"user" {
        return None;
    }
    let user = tokens.next()?;
    // A selector is `(rules...)`: key and channel patterns, which can hold
    // parentheses but start with ~, % or &, then the command rules.
    let mut in_selector = false;
    for token in tokens {
        let token = match token.strip_prefix(b"(") {
            Some(rest) => {
                in_selector = true;
                rest
            }
            None => token,
        };
        let inner = token.strip_suffix(b")");
        if let Some(rule) = inner
            .into_iter()
            .chain([token])
            .find(|rule| names_a_command(rule, names))
        {
            return Some(Some(Found {
                user,
                rule,
                in_selector,
            }));
        }
        // A selector's last token is a command rule.
        if inner.is_some() && matches!(token.first(), Some(b'+' | b'-')) {
            in_selector = false;
        }
    }
    Some(None)
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

    fn found(line: &str) -> Option<(String, String, bool)> {
        rule_naming_a_command(line.as_bytes(), &names())
            .expect("a user entry")
            .map(|found| {
                (
                    String::from_utf8_lossy(found.user).into_owned(),
                    String::from_utf8_lossy(found.rule).into_owned(),
                    found.in_selector,
                )
            })
    }

    #[test]
    fn rules_that_name_a_command_are_found() {
        for (line, rule, in_selector) in [
            (
                "user u on nopass resetchannels -@all +hello.mul",
                "+hello.mul",
                false,
            ),
            (
                "user u on nopass resetchannels +@all -HELLO.MUL",
                "-HELLO.MUL",
                false,
            ),
            ("user u on nopass -@all +hello.mul|2", "+hello.mul|2", false),
            ("user u on nopass -@all +sub.kv|get", "+sub.kv|get", false),
            ("user u on nopass +@all -sub.kv|set", "-sub.kv|set", false),
            ("user u on nopass -@all +sub.kv +get", "+sub.kv", false),
            (
                "user u on nopass resetchannels -@all (~* resetchannels -@all +hello.mul)",
                "+hello.mul",
                true,
            ),
            (
                "user u on nopass -@all (~k resetchannels +@all -sub.kv|set +get)",
                "-sub.kv|set",
                true,
            ),
            (
                "user u on nopass -@all (~* resetchannels +sub.kv|get)",
                "+sub.kv|get",
                true,
            ),
            ("user u on nopass -@all (+hello.mul)", "+hello.mul", true),
            (
                "user u on nopass -@all (~a) resetchannels +hello.mul)",
                "+hello.mul",
                true,
            ),
            (
                "user u on nopass -@all (~* resetchannels -@all +get) +sub.kv",
                "+sub.kv",
                false,
            ),
        ] {
            assert_eq!(
                found(line),
                Some(("u".into(), rule.into(), in_selector)),
                "{line}"
            );
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
