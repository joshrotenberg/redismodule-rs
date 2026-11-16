use super::utils::{self, TestConnection};
use anyhow::Result;
use std::time::Duration;

fn stats(con: &mut TestConnection) -> Result<Vec<i64>> {
    Ok(redis::cmd("auth_users.stats").query(con)?)
}
fn create(con: &mut TestConnection, name: &str) -> Result<()> {
    redis::cmd("auth_users.create").arg(name).query::<()>(con)?;
    redis::cmd("auth_users.defaults")
        .arg(name)
        .query::<()>(con)?;
    Ok(())
}

#[test]
fn test_module_users_attachment_reauth_failure_and_disconnect() -> Result<()> {
    let mut con = TestConnection::new("auth_users");
    create(&mut con, "owner")?;
    redis::cmd("auth_users.create")
        .arg("owner")
        .query::<()>(&mut con)
        .expect_err("duplicate name");
    let mut client = con.connection()?;
    let id: i64 = redis::cmd("auth_users.attach")
        .arg(("owner", "track"))
        .query(&mut client)?;
    assert!(id > 0);
    assert_eq!(
        redis::cmd("ACL")
            .arg("WHOAMI")
            .query::<String>(&mut client)?,
        "owner"
    );
    assert_eq!(stats(&mut con)?, [0, 0]);
    redis::cmd("auth_users.attach")
        .arg(("owner", "track"))
        .query::<i64>(&mut client)?;
    assert_eq!(stats(&mut con)?, [1, 1]);
    // Failed attachment reclaims its owned callback without consuming the prior one.
    redis::cmd("auth_users.attach_acl")
        .arg(("missing-account", "track"))
        .query::<i64>(&mut client)
        .expect_err("missing account");
    assert_eq!(stats(&mut con)?, [1, 2]);
    redis::cmd("ACL")
        .arg(("SETUSER", "disabled-account", "off", "+@all"))
        .query::<()>(&mut con)?;
    redis::cmd("auth_users.attach_acl")
        .arg(("disabled-account", "track"))
        .query::<i64>(&mut client)
        .expect_err("disabled account");
    assert_eq!(stats(&mut con)?, [1, 3]);
    redis::cmd("AUTH")
        .arg(("default", "anything"))
        .query::<()>(&mut client)?;
    assert_eq!(stats(&mut con)?, [2, 4]);
    redis::cmd("auth_users.attach")
        .arg(("owner", "track"))
        .query::<i64>(&mut client)?;
    drop(client);
    utils::wait_until(
        Duration::from_secs(2),
        "identity disconnect cleanup",
        || Ok(stats(&mut con)? == [3, 5]),
    )?;
    Ok(())
}

#[test]
fn test_module_users_acl_transaction_stale_handles_and_redaction() -> Result<()> {
    let mut con = TestConnection::new("auth_users");
    create(&mut con, "owner")?;
    redis::cmd("auth_users.acl")
        .arg(("owner", "reset on +get ~allowed:*"))
        .query::<()>(&mut con)?;
    let mut client = con.connection()?;
    redis::cmd("auth_users.attach")
        .arg(("owner", "track"))
        .query::<i64>(&mut client)?;
    assert_eq!(
        redis::cmd("GET")
            .arg("allowed:key")
            .query::<Option<String>>(&mut client)?,
        None
    );
    redis::cmd("GET")
        .arg("denied:key")
        .query::<Option<String>>(&mut client)
        .expect_err("key ACL");
    let error = redis::cmd("auth_users.acl")
        .arg((
            "owner",
            "reset on +@all >private-test-password invalid-rule-secret",
        ))
        .query::<()>(&mut con)
        .expect_err("invalid ACL");
    assert_eq!(error.to_string(), "ResponseError: invalid ACL rules");
    redis::cmd("SET")
        .arg(("allowed:key", "x"))
        .query::<()>(&mut client)
        .expect_err("failed atomic update retained +get");
    redis::cmd("GET")
        .arg("denied:key")
        .query::<Option<String>>(&mut client)
        .expect_err("failed atomic update retained key pattern");
    // Disabling is a flag change; already attached clients retain their rules.
    redis::cmd("auth_users.rule")
        .arg(("owner", "off"))
        .query::<()>(&mut con)?;
    assert_eq!(
        redis::cmd("GET")
            .arg("allowed:key")
            .query::<Option<String>>(&mut client)?,
        None
    );
    let mut denied = con.connection()?;
    redis::cmd("auth_users.attach")
        .arg(("owner", "track"))
        .query::<i64>(&mut denied)
        .expect_err("disabled created user");
    redis::cmd("auth_users.remove")
        .arg("owner")
        .query::<()>(&mut con)?;
    utils::wait_until(
        Duration::from_secs(2),
        "removed user client cleanup",
        || Ok(stats(&mut con)? == [1, 2]),
    )?;
    for command in [
        "auth_users.remove",
        "auth_users.rule",
        "auth_users.acl",
        "auth_users.attach",
    ] {
        let error = redis::cmd(command)
            .arg(("owner", "on"))
            .query::<redis::Value>(&mut con)
            .expect_err("stale handle");
        assert!(error.to_string().contains("stale or foreign"));
    }
    // A new native user with the same name does not revive the stale generation.
    create(&mut con, "owner")?;
    for pos in [0_u64, 4, u64::MAX] {
        redis::cmd("auth_users.redact")
            .arg((pos, "secret"))
            .query::<()>(&mut con)
            .expect_err("invalid redaction position");
    }
    redis::cmd("CONFIG")
        .arg(("SET", "slowlog-log-slower-than", "0"))
        .query::<()>(&mut con)?;
    redis::cmd("auth_users.redact")
        .arg((2, "redaction-secret-value"))
        .query::<()>(&mut con)?;
    let log: redis::Value = redis::cmd("SLOWLOG").arg(("GET", 10)).query(&mut con)?;
    let text = format!("{log:?}");
    assert!(!text.contains("redaction-secret-value"));
    assert!(text.contains("redacted"));
    let id: i64 = redis::cmd("CLIENT").arg("ID").query(&mut con)?;
    assert_eq!(
        redis::cmd("auth_users.certificate")
            .arg(id)
            .query::<Option<Vec<u8>>>(&mut con)?,
        None
    );
    assert_eq!(
        redis::cmd("auth_users.certificate")
            .arg(u64::MAX)
            .query::<Option<Vec<u8>>>(&mut con)?,
        None
    );
    redis::cmd("auth_users.revoke")
        .arg(u64::MAX)
        .query::<()>(&mut con)
        .expect_err("missing client");
    Ok(())
}

#[test]
fn test_module_users_unload_refusal_cleanup_and_reload() -> Result<()> {
    let mut con = TestConnection::new("auth_users");
    create(&mut con, "owner")?;
    let mut tracked = con.connection()?;
    let id: i64 = redis::cmd("auth_users.attach")
        .arg(("owner", "track"))
        .query(&mut tracked)?;
    // ACL-derived and no-consumer attachments still belong to the module.
    let mut no_callback = con.connection()?;
    redis::cmd("auth_users.attach_acl")
        .arg(("default", "none"))
        .query::<i64>(&mut no_callback)?;
    redis::cmd("auth_users.refuse").query::<()>(&mut con)?;
    redis::cmd("MODULE")
        .arg(("UNLOAD", "auth_users"))
        .query::<()>(&mut con)
        .expect_err("deinit refusal");
    assert_eq!(stats(&mut con)?, [0, 0]);
    assert_eq!(redis::cmd("PING").query::<String>(&mut tracked)?, "PONG");
    create(&mut con, "after-refusal")?;
    redis::cmd("MODULE")
        .arg(("UNLOAD", "auth_users"))
        .query::<()>(&mut con)?;
    redis::cmd("PING")
        .query::<String>(&mut tracked)
        .expect_err("tracked connection closed on unload");
    redis::cmd("PING")
        .query::<String>(&mut no_callback)
        .expect_err("no-consumer connection closed on unload");
    assert!(
        con.server_log()?
            .contains(&format!("auth_users cleanup {id}"))
    );
    // Failed init leaves neither native owners nor a bound SDK lifecycle.
    redis::cmd("MODULE")
        .arg("LOAD")
        .arg(utils::module_path("auth_users")?)
        .arg("fail")
        .query::<()>(&mut con)
        .expect_err("init failure");
    redis::cmd("MODULE")
        .arg("LOAD")
        .arg(utils::module_path("auth_users")?)
        .query::<()>(&mut con)?;
    create(&mut con, "owner")?;
    redis::cmd("MODULE")
        .arg(("UNLOAD", "auth_users"))
        .query::<()>(&mut con)?;
    Ok(())
}

#[test]
fn test_module_users_worker_revocation_and_cleanup_panics() -> Result<()> {
    let mut con = TestConnection::new("auth_users");
    create(&mut con, "owner")?;
    for (index, mode) in ["track", "panic", "drop-panic"].iter().enumerate() {
        let mut client = con.connection()?;
        let id: i64 = redis::cmd("auth_users.attach")
            .arg(("owner", *mode))
            .query(&mut client)?;
        redis::cmd("auth_users.worker")
            .arg(id)
            .query::<()>(&mut con)?;
        utils::wait_until(
            Duration::from_secs(2),
            "locked worker identity cleanup",
            || Ok(redis::cmd("auth_users.finish").query::<i64>(&mut con)? == 1),
        )?;
        let count = index as i64 + 1;
        assert_eq!(stats(&mut con)?, [count, count]);
        redis::cmd("PING")
            .query::<String>(&mut client)
            .expect_err("revoked connection");
    }
    redis::cmd("MODULE")
        .arg(("UNLOAD", "auth_users"))
        .query::<()>(&mut con)?;
    Ok(())
}
