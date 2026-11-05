//! `MODULE UNLOAD` while an ACL rule names one of the module's commands.
//!
//! From Redis 7.2, an unload reapplies every user's command rules, and a rule
//! that names a command which no longer exists stops the server. Before 7.2
//! the unload leaves the user's command bits behind, and the next `ACL LIST`
//! stops the server instead. The SDK's OnUnload refuses the unload while such
//! a rule exists. A server that checks the rules itself refuses before
//! OnUnload runs. `test_acl_category_survives_module_reload` unloads with a
//! rule on a category the module added, which does not block the unload.
use crate::utils::{self, TestConnection};
use anyhow::{Context, Result, ensure};
use redis::Value;
use redis_module::RedisValue;

fn server_version(con: &mut TestConnection) -> Result<(i32, i32)> {
    let info: String = redis::cmd("INFO").arg("server").query(con)?;
    let version = redis_module::Context::version_from_info(RedisValue::BulkString(info))?;
    Ok((version.major, version.minor))
}

fn module_loaded(con: &mut TestConnection, name: &str) -> Result<bool> {
    let modules: Vec<Vec<Value>> = redis::cmd("MODULE").arg("LIST").query(con)?;
    let name = Value::BulkString(name.as_bytes().to_vec());
    let key = Value::BulkString(b"name".to_vec());
    Ok(modules.iter().any(|fields| {
        fields
            .chunks(2)
            .any(|pair| pair == [key.clone(), name.clone()])
    }))
}

/// Sets `rules` on a fresh user, checks that the unload is refused and the
/// module keeps working, then deletes the user. `listed` is the rule as
/// `ACL LIST` shows it, which the refusal names, and where it is.
fn assert_unload_refused(
    con: &mut TestConnection,
    module: &str,
    rules: &[&str],
    listed: &str,
    still_works: &mut dyn FnMut(&mut TestConnection) -> Result<()>,
) -> Result<()> {
    redis::cmd("ACL")
        .arg(&["SETUSER", "rule-holder", "on", "nopass"])
        .arg(rules)
        .query::<()>(con)
        .with_context(|| format!("setting {rules:?}"))?;
    let expected = format!("ACL user rule-holder has the rule {listed},");
    let logged = |con: &TestConnection| -> Result<usize> {
        Ok(con.server_log()?.matches(&expected).count())
    };
    let logged_before = logged(con)?;
    let error = redis::cmd("MODULE")
        .arg(&["UNLOAD", module])
        .query::<()>(con)
        .expect_err("the unload went ahead while an ACL rule names a module command");
    ensure!(
        !error.is_io_error() && !error.is_connection_dropped(),
        "{rules:?}: the unload failed without a reply, so the server stopped: {error}"
    );
    assert_eq!(redis::cmd("PING").query::<String>(con)?, "PONG");
    assert!(
        module_loaded(con, module)?,
        "{rules:?}: {module} is unloaded"
    );
    still_works(con)?;
    // The server logs the module's OnUnload refusal; a server that checks
    // the rules itself refuses earlier, with its own message.
    if error.to_string().contains("operation not possible") {
        assert_eq!(
            logged(con)?,
            logged_before + 1,
            "{rules:?}: the log does not name the user and rule once ({expected}):\n{}",
            con.server_log()?
        );
    } else {
        assert!(error.to_string().contains("ACL"), "{rules:?}: {error}");
    }
    redis::cmd("ACL")
        .arg(&["DELUSER", "rule-holder"])
        .query::<()>(con)?;
    Ok(())
}

/// Unloads `module` now that no rule names it, checks that the server and its
/// ACL state survive, and loads the module again.
fn assert_unload_and_reload(con: &mut TestConnection, module: &str) -> Result<()> {
    redis::cmd("MODULE")
        .arg(&["UNLOAD", module])
        .query::<()>(con)?;
    assert_eq!(redis::cmd("PING").query::<String>(con)?, "PONG");
    // Before Redis 7.2, ACL LIST is where command bits left behind for an
    // unloaded command stop the server.
    redis::cmd("ACL").arg("LIST").query::<Vec<String>>(con)?;
    redis::cmd("MODULE")
        .arg("LOAD")
        .arg(utils::module_path(module)?)
        .query::<()>(con)?;
    Ok(())
}

#[test]
fn test_unload_is_refused_while_an_acl_rule_names_a_module_command() -> Result<()> {
    let mut con = TestConnection::new("hello");
    let version = server_version(&mut con)?;
    let mut works = |con: &mut TestConnection| -> Result<()> {
        let product: Vec<i64> = redis::cmd("hello.mul").arg(&[2, 3]).query(con)?;
        assert_eq!(product, [2, 3, 6]);
        Ok(())
    };

    let mut cases: Vec<(&[&str], &str)> = vec![
        (&["+hello.mul"], "+hello.mul"),
        (&["+HELLO.Mul"], "+hello.mul"),
        (&["+@all", "-hello.mul"], "-hello.mul"),
        // A first-argument rule names the command too.
        (&["+hello.mul|2"], "+hello.mul|2"),
    ];
    // Selectors start in Redis 7.0.
    if version >= (7, 0) {
        cases.push((&["(~*", "+hello.mul)"], "+hello.mul in a selector"));
        cases.push((&["~*", "(~* +@all -hello.mul)"], "-hello.mul in a selector"));
    }
    for (rules, listed) in cases {
        assert_unload_refused(&mut con, "hello", rules, listed, &mut works)?;
    }

    // With the rules gone, the unload goes ahead, and the default user's
    // +@all does not hold it back.
    assert_unload_and_reload(&mut con, "hello")?;
    works(&mut con)?;
    Ok(())
}

#[test]
fn test_unload_is_refused_while_an_acl_rule_names_a_module_subcommand() -> Result<()> {
    let mut con = TestConnection::new("subcommands");
    // Subcommands start in Redis 7.0; this example's categories need 7.2.
    if server_version(&mut con)? < (7, 2) {
        return Ok(());
    }
    let mut works = |con: &mut TestConnection| -> Result<()> {
        redis::cmd("subcmd.kv")
            .arg(&["set", "subcmd:key", "value"])
            .query::<()>(con)?;
        let value: String = redis::cmd("subcmd.kv")
            .arg(&["get", "subcmd:key"])
            .query(con)?;
        assert_eq!(value, "value");
        Ok(())
    };

    let cases: [(&[&str], &str); 6] = [
        (&["+subcmd.kv|get"], "+subcmd.kv|get"),
        (&["+SUBCMD.KV|Get"], "+subcmd.kv|get"),
        (&["+@all", "-subcmd.kv|set"], "-subcmd.kv|set"),
        (&["+subcmd.kv"], "+subcmd.kv"),
        (&["+subcmd.documented"], "+subcmd.documented"),
        (&["(~*", "+subcmd.kv|get)"], "+subcmd.kv|get in a selector"),
    ];
    for (rules, listed) in cases {
        assert_unload_refused(&mut con, "subcommands", rules, listed, &mut works)?;
    }

    assert_unload_and_reload(&mut con, "subcommands")?;
    works(&mut con)?;
    Ok(())
}
