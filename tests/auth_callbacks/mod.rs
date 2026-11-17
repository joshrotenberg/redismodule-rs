use super::utils::{self, Connection, TestConnection};
use anyhow::Result;
use redis::{ConnectionLike, Value};
use std::time::Duration;

fn prepare(server: &mut TestConnection) -> Result<()> {
    redis::cmd("auth_callbacks.prepare").query::<()>(server)?;
    Ok(())
}
fn auth(client: &mut Connection, user: &[u8], password: &[u8]) -> redis::RedisResult<()> {
    redis::cmd("AUTH").arg(user).arg(password).query(client)
}
fn stats(server: &mut TestConnection) -> Result<Vec<i64>> {
    Ok(redis::cmd("auth_callbacks.stats").query(server)?)
}
fn identity(client: &mut Connection) -> redis::RedisResult<String> {
    redis::cmd("ACL").arg("WHOAMI").query(client)
}
fn drain(server: &mut TestConnection) -> Result<()> {
    redis::cmd("auth_callbacks.drain").query::<()>(server)?;
    Ok(())
}
fn unload(server: &mut TestConnection, name: &str) -> redis::RedisResult<()> {
    redis::cmd("MODULE").arg(("UNLOAD", name)).query(server)
}
fn account(server: &mut TestConnection, name: &str, password: &str) -> Result<()> {
    redis::cmd("ACL")
        .arg(("SETUSER", name, "on", "+@all", "~*", format!(">{password}")))
        .query::<()>(server)?;
    Ok(())
}

#[test]
fn test_auth_callbacks_accept_failure_binary_and_explicit_fallback() -> Result<()> {
    let mut server = TestConnection::new("auth_callbacks");
    let mut client = server.connection()?;
    auth(&mut client, b"stub", b"stub-password").expect_err("explicit preparation required");
    prepare(&mut server)?;
    auth(&mut client, b"stub", b"stub-password")?;
    assert_eq!(identity(&mut client)?, "stub-active");
    redis::cmd("SET")
        .arg(("auth:key", "value"))
        .query::<()>(&mut server)?;
    assert_eq!(
        redis::cmd("GET")
            .arg("auth:key")
            .query::<String>(&mut client)?,
        "value"
    );
    redis::cmd("GET")
        .arg("outside:key")
        .query::<String>(&mut client)
        .expect_err("bounded key permission");
    for user in [b"stub".as_slice(), b"disabled", b"stale", b"error"] {
        auth(
            &mut client,
            user,
            if user == b"stub" {
                b"wrong"
            } else {
                b"stub-password"
            },
        )
        .expect_err("handled denial");
        assert_eq!(
            identity(&mut client)?,
            "stub-active",
            "failed reauth preserves identity"
        );
    }
    auth(&mut client, b"binary\0\xff", b"\0binary\xffpassword")?;
    assert_eq!(identity(&mut client)?, "stub-active");
    auth(&mut client, b"binary\0\xff", b"\0binary").expect_err("native byte lengths are preserved");
    account(&mut server, "fallback", "fallback-password")?;
    auth(&mut client, b"fallback", b"fallback-password")?;
    assert_eq!(identity(&mut client)?, "fallback");
    auth(&mut client, b"fallback", b"wrong").expect_err("Redis fallback denial");
    account(&mut server, "deny", "would-succeed")?;
    auth(&mut client, b"deny", b"would-succeed").expect_err("Deny never falls back");
    auth(&mut client, b"", b"").expect_err("empty binary credentials");
    assert_eq!(redis::cmd("PING").query::<String>(&mut client)?, "PONG");
    drain(&mut server)?;
    unload(&mut server, "auth_callbacks")?;
    Ok(())
}

#[test]
fn test_auth_callbacks_two_argument_auth_preserves_native_rules() -> Result<()> {
    let mut server = TestConnection::new("auth_callbacks");
    let mut client = server.connection()?;
    redis::cmd("AUTH")
        .arg("unconfigured-password")
        .query::<()>(&mut client)
        .expect_err("native default-nopass rule");
    assert_eq!(
        stats(&mut server)?[0],
        0,
        "Redis rejects before auth dispatch"
    );
    redis::cmd("CONFIG")
        .arg(("SET", "requirepass", "default-test-password"))
        .query::<()>(&mut server)?;
    redis::cmd("AUTH")
        .arg("default-test-password")
        .query::<()>(&mut client)?;
    assert_eq!(identity(&mut client)?, "default");
    redis::cmd("AUTH")
        .arg("wrong-password")
        .query::<()>(&mut client)
        .expect_err("native fallback verifies default password");
    assert_eq!(stats(&mut server)?[0], 2);
    let before = stats(&mut server)?[0];
    redis::cmd("HELLO")
        .arg(999)
        .query::<Value>(&mut client)
        .expect_err("native malformed HELLO");
    assert_eq!(
        stats(&mut server)?[0],
        before,
        "invalid HELLO does not invoke the callback"
    );
    unload(&mut server, "auth_callbacks")?;
    Ok(())
}

#[test]
fn test_auth_callbacks_hello_protocol_name_and_denial_alignment() -> Result<()> {
    let mut server = TestConnection::new("auth_callbacks");
    prepare(&mut server)?;
    for version in [2, 3] {
        let mut client = server.connection()?;
        let reply: Value = redis::cmd("HELLO")
            .arg(version)
            .arg(("SETNAME", "before", "AUTH", "stub", "stub-password"))
            .query(&mut client)?;
        assert!(matches!(reply, Value::Map(_)) == (version == 3));
        assert!(matches!(reply, Value::Array(_)) == (version == 2));
        assert_eq!(identity(&mut client)?, "stub-active");
        let error = redis::cmd("HELLO")
            .arg(if version == 2 { 3 } else { 2 })
            .arg(("AUTH", "public", "private-attempt", "SETNAME", "after"))
            .query::<Value>(&mut client)
            .expect_err("handled HELLO denial");
        assert!(error.to_string().contains("Stub denied authentication"));
        assert_eq!(identity(&mut client)?, "stub-active");
        // Native HELLO leaves both protocol and name unchanged on auth failure.
        auth(&mut client, b"fallback-to-default", b"wrong").expect_err("normal fallback failure");
        let metadata: Value = redis::cmd("HELLO").query(&mut client)?;
        assert!(matches!(metadata, Value::Map(_)) == (version == 3));
        let info: String = redis::cmd("CLIENT").arg("INFO").query(&mut server)?;
        assert!(!info.is_empty());
        let id: i64 = redis::cmd("CLIENT").arg("ID").query(&mut client)?;
        let clients: String = redis::cmd("CLIENT")
            .arg("LIST")
            .arg("ID")
            .arg(id)
            .query(&mut server)?;
        assert!(clients.contains("name=before"));
        assert!(!clients.contains("name=after"));
        assert_eq!(redis::cmd("PING").query::<String>(&mut client)?, "PONG");
    }
    drain(&mut server)?;
    Ok(())
}

#[test]
fn test_auth_callbacks_acl_target_needs_installed_sdk_identity() -> Result<()> {
    let mut server = TestConnection::new("auth_callbacks");
    prepare(&mut server)?;
    account(&mut server, "existing", "native-password")?;
    let mut client = server.connection()?;
    auth(&mut client, b"acl", b"stub-password").expect_err("no implicit bootstrap");
    assert_eq!(identity(&mut client)?, "default");
    auth(&mut client, b"stub", b"stub-password")?;
    auth(&mut client, b"acl", b"stub-password")
        .expect_err("native-auth history cannot prove the callback slot");
    redis::cmd("auth_callbacks.command_identity")
        .arg("acl")
        .query::<()>(&mut client)
        .expect_err("public ACL helper also rejects native-auth history");
    redis::cmd("auth_callbacks.command_identity").query::<()>(&mut client)?;
    auth(&mut client, b"acl", b"stub-password")?;
    assert_eq!(identity(&mut client)?, "existing");
    redis::cmd("ACL")
        .arg(("SETUSER", "existing", "off"))
        .query::<()>(&mut server)?;
    auth(&mut client, b"acl", b"stub-password").expect_err("disabled ACL target");
    assert_eq!(identity(&mut client)?, "existing");
    assert_eq!(&stats(&mut server)?[1..], &[1, 4, 1]);
    drain(&mut server)?;
    redis::cmd("PING")
        .query::<String>(&mut client)
        .expect_err("explicit drain revokes ACL handoff");
    unload(&mut server, "auth_callbacks")?;
    Ok(())
}

#[test]
fn test_auth_callbacks_native_order_and_static_unregistration() -> Result<()> {
    let mut server = TestConnection::new("auth_callbacks");
    prepare(&mut server)?;
    redis::cmd("MODULE")
        .arg("LOAD")
        .arg(utils::module_path("auth_callbacks_order")?)
        .query::<()>(&mut server)?;
    let mut client = server.connection()?;
    let error =
        auth(&mut client, b"stub", b"stub-password").expect_err("newest denial stops chain");
    assert!(error.to_string().contains("Later module denied"));
    assert_eq!(stats(&mut server)?[0], 0);
    auth(&mut client, b"binary\0\xff", b"\0binary\xffpassword")?;
    assert_eq!(stats(&mut server)?[0], 1);
    assert_eq!(
        redis::cmd("auth_callbacks_order.calls").query::<i64>(&mut server)?,
        2
    );
    drain(&mut server)?;
    unload(&mut server, "auth_callbacks")?;
    redis::cmd("MODULE")
        .arg("LOAD")
        .arg(utils::module_path("auth_callbacks")?)
        .query::<()>(&mut server)?;
    prepare(&mut server)?;
    let mut newer = server.connection()?;
    auth(&mut newer, b"stub", b"stub-password")?;
    assert_eq!(
        redis::cmd("auth_callbacks_order.calls").query::<i64>(&mut server)?,
        2,
        "newer Accept stops chain"
    );
    unload(&mut server, "auth_callbacks_order")?;
    auth(&mut newer, b"stub", b"stub-password")?;
    drain(&mut server)?;
    unload(&mut server, "auth_callbacks")?;
    Ok(())
}

#[test]
fn test_auth_callbacks_drain_unload_reauth_and_disconnect() -> Result<()> {
    let mut server = TestConnection::new("auth_callbacks");
    prepare(&mut server)?;
    let mut client = server.connection()?;
    let mut no_notify = server.connection()?;
    auth(&mut no_notify, b"no-notify", b"stub-password")?;
    auth(&mut client, b"stub", b"stub-password")?;
    auth(&mut client, b"stub", b"stub-password")?;
    assert_eq!(&stats(&mut server)?[1..], &[1, 1, 1]);
    unload(&mut server, "auth_callbacks").expect_err("live resources refuse unload");
    assert_eq!(identity(&mut client)?, "stub-active");
    drop(client);
    utils::wait_until(Duration::from_secs(2), "auth disconnect cleanup", || {
        Ok(stats(&mut server)?[3] == 0)
    })?;
    unload(&mut server, "auth_callbacks")
        .expect_err("created users still require explicit removal");
    drain(&mut server)?;
    redis::cmd("PING")
        .query::<String>(&mut no_notify)
        .expect_err("drain consumes no-consumer installation");
    redis::cmd("auth_callbacks.refuse").query::<()>(&mut server)?;
    unload(&mut server, "auth_callbacks")
        .expect_err("deinit refusal preserves active empty module");
    prepare(&mut server)?;
    drain(&mut server)?;
    unload(&mut server, "auth_callbacks")?;
    redis::cmd("MODULE")
        .arg("LOAD")
        .arg(utils::module_path("auth_callbacks")?)
        .query::<()>(&mut server)?;
    let mut client = server.connection()?;
    auth(&mut client, b"stub", b"stub-password").expect_err("reload requires explicit preparation");
    prepare(&mut server)?;
    auth(&mut client, b"stub", b"stub-password")?;
    drain(&mut server)?;
    unload(&mut server, "auth_callbacks")?;
    Ok(())
}

#[test]
fn test_auth_callbacks_omitted_notification_reconciles_after_disappearance() -> Result<()> {
    for no_consumer in [false, true] {
        let mut server = TestConnection::new("auth_callbacks");
        prepare(&mut server)?;
        let mut client = server.connection()?;
        let id: u64 = redis::cmd("CLIENT").arg("ID").query(&mut client)?;
        redis::cmd("auth_callbacks.probe")
            .arg("omit-callback")
            .query::<()>(&mut server)?;
        auth(
            &mut client,
            if no_consumer { b"no-notify" } else { b"stub" },
            b"stub-password",
        )?;
        redis::cmd("auth_callbacks.probe")
            .arg("reset")
            .query::<()>(&mut server)?;
        unload(&mut server, "auth_callbacks").expect_err("retained native installation");
        if no_consumer {
            redis::cmd("auth_callbacks.remove_active").query::<()>(&mut server)?;
        } else {
            // Native close is ordinary and safe. Its missing notification must
            // leave the indexed cookie retained through this command's return.
            let error = redis::cmd("auth_callbacks.drain")
                .query::<()>(&mut server)
                .unwrap_err();
            assert!(error.to_string().contains("cleanup is still pending"));
            assert_eq!(&stats(&mut server)?[1..], &[0, 0, 1]);
        }
        drop(client);
        utils::wait_until(
            Duration::from_secs(2),
            "omitted callback client disappearance",
            || {
                let clients: String = redis::cmd("CLIENT")
                    .arg("LIST")
                    .arg("ID")
                    .arg(id)
                    .query(&mut server)?;
                Ok(clients.is_empty())
            },
        )?;
        drain(&mut server)?;
        let expected = if no_consumer {
            vec![0, 0, 0]
        } else {
            vec![1, 1, 0]
        };
        assert_eq!(&stats(&mut server)?[1..], expected);
        drain(&mut server)?;
        assert_eq!(&stats(&mut server)?[1..], expected);
        unload(&mut server, "auth_callbacks")?;
    }
    Ok(())
}

#[test]
fn test_auth_callbacks_denial_audit_panic_and_public_messages() -> Result<()> {
    let mut server = TestConnection::with_args("auth_callbacks", &["quiet-hook"]);
    prepare(&mut server)?;
    account(&mut server, "panic", "would-succeed")?;
    redis::cmd("ACL")
        .arg(("LOG", "RESET"))
        .query::<()>(&mut server)?;
    let mut client = server.connection()?;
    for (user, message) in [
        ("public", "Stub denied"),
        ("panic", "Module authentication failed"),
        ("stale", "Module authentication failed"),
        ("sanitize", "public  message tail"),
    ] {
        let password: &[u8] = if user == "stale" {
            b"stub-password"
        } else {
            b"private-password-sentinel"
        };
        let error = auth(&mut client, user.as_bytes(), password).expect_err("handled denial");
        assert!(error.to_string().contains(message));
        assert!(!error.to_string().contains("private-"));
    }
    let log: Value = redis::cmd("ACL").arg("LOG").query(&mut server)?;
    let text = format!("{log:?}");
    assert_eq!(text.matches("module-auth").count(), 4);
    assert!(!text.contains("private-"));
    assert!(!text.contains("Stub denied"));
    auth(&mut client, b"stub", b"stub-password")?;
    auth(&mut client, b"default", b"anything")?;
    let log: Value = redis::cmd("ACL").arg("LOG").query(&mut server)?;
    assert_eq!(
        format!("{log:?}").matches("module-auth").count(),
        4,
        "success and NotHandled add no denial audit"
    );
    let log = server.server_log()?;
    assert!(log.contains("module authentication callback panicked"));
    assert!(!log.contains("private-provider-panic-sentinel"));
    drain(&mut server)?;
    Ok(())
}

#[test]
fn test_auth_callbacks_zero_retention_skips_native_audit() -> Result<()> {
    let mut server = TestConnection::new("auth_callbacks");
    let mut client = server.connection()?;
    redis::cmd("auth_callbacks.probe")
        .arg("audit-start")
        .query::<()>(&mut server)?;
    redis::cmd("CONFIG")
        .arg(("SET", "acllog-max-len", "0"))
        .query::<()>(&mut server)?;
    for user in [b"deny".as_slice(), b"public"] {
        auth(&mut client, user, b"private-denial").expect_err("denial survives disabled audit");
    }
    assert_eq!(
        redis::cmd("auth_callbacks.probe")
            .arg("audit-count")
            .query::<i64>(&mut server)?,
        0
    );
    redis::cmd("CONFIG")
        .arg(("SET", "acllog-max-len", "128"))
        .query::<()>(&mut server)?;
    for mode in ["query-missing", "query-error"] {
        redis::cmd("auth_callbacks.probe")
            .arg(mode)
            .query::<()>(&mut server)?;
        auth(&mut client, b"public", b"private-denial")
            .expect_err("denial survives unavailable audit read");
        redis::cmd("auth_callbacks.probe")
            .arg("query-reset")
            .query::<()>(&mut server)?;
        assert_eq!(
            redis::cmd("auth_callbacks.probe")
                .arg("audit-count")
                .query::<i64>(&mut server)?,
            0
        );
    }
    auth(&mut client, b"deny", b"private-denial").expect_err("handled denial");
    assert_eq!(
        redis::cmd("auth_callbacks.probe")
            .arg("audit-count")
            .query::<i64>(&mut server)?,
        1
    );
    redis::cmd("auth_callbacks.probe")
        .arg("audit-reset")
        .query::<()>(&mut server)?;
    unload(&mut server, "auth_callbacks")?;
    Ok(())
}

#[test]
fn test_auth_callbacks_loading_internal_and_native_refusal_preflight() -> Result<()> {
    let mut server = TestConnection::new("auth_callbacks");
    prepare(&mut server)?;
    let mut client = server.connection()?;
    for mode in ["loading", "async", "missing"] {
        let before = stats(&mut server)?[0];
        redis::cmd("auth_callbacks.probe")
            .arg(mode)
            .query::<()>(&mut server)?;
        let error =
            auth(&mut client, b"stub", b"stub-password").expect_err("unsupported invocation phase");
        assert!(error.to_string().contains("Module authentication failed"));
        assert_eq!(stats(&mut server)?[0], before, "consumer was not invoked");
        assert_eq!(
            redis::cmd("auth_callbacks.probe")
                .arg("count")
                .query::<i64>(&mut server)?,
            0,
            "no native attachment"
        );
        assert_eq!(identity(&mut client)?, "default");
        redis::cmd("auth_callbacks.probe")
            .arg("reset")
            .query::<()>(&mut server)?;
    }
    redis::cmd("auth_callbacks.probe")
        .arg("normal")
        .query::<()>(&mut server)?;
    auth(&mut client, b"stub", b"stub-password").expect_err("native refusal is handled denial");
    assert_eq!(
        redis::cmd("auth_callbacks.probe")
            .arg("count")
            .query::<i64>(&mut server)?,
        1
    );
    let error: String = redis::cmd("auth_callbacks.internal").query(&mut server)?;
    assert!(error.contains("Module authentication failed"));
    assert_eq!(
        redis::cmd("auth_callbacks.probe")
            .arg("count")
            .query::<i64>(&mut server)?,
        1,
        "internal nested auth cannot attach"
    );
    assert_eq!(identity(&mut client)?, "default");
    redis::cmd("auth_callbacks.probe")
        .arg("reset")
        .query::<()>(&mut server)?;
    auth(&mut client, b"stub", b"stub-password")?;
    drain(&mut server)?;
    unload(&mut server, "auth_callbacks")?;
    Ok(())
}

#[test]
fn test_auth_callbacks_native_auth_hello_redaction() -> Result<()> {
    let mut server = TestConnection::new("auth_callbacks");
    prepare(&mut server)?;
    redis::cmd("CONFIG")
        .arg(("SET", "slowlog-log-slower-than", "0"))
        .query::<()>(&mut server)?;
    redis::cmd("SLOWLOG")
        .arg("RESET")
        .query::<()>(&mut server)?;
    let mut monitor = server.connection()?;
    redis::cmd("MONITOR").query::<()>(&mut monitor)?;
    let mut client = server.connection()?;
    auth(&mut client, b"stub", b"stub-password")?;
    auth(&mut client, b"public", b"private-wire-sentinel").expect_err("auth denial");
    for proto in [2, 3] {
        redis::cmd("HELLO")
            .arg(proto)
            .arg(("AUTH", "stub", "stub-password", "SETNAME", "privacy"))
            .query::<Value>(&mut client)?;
        redis::cmd("HELLO")
            .arg(proto)
            .arg((
                "SETNAME",
                "privacy",
                "AUTH",
                "public",
                "private-wire-sentinel",
            ))
            .query::<Value>(&mut client)
            .expect_err("hello denial");
    }
    redis::cmd("ECHO")
        .arg("privacy-fence")
        .query::<String>(&mut server)?;
    let mut monitored = String::new();
    for _ in 0..20 {
        let next = monitor.req_packed_command(&[])?;
        let line = format!("{next:?}");
        monitored.push_str(&line);
        if line.contains("privacy-fence") {
            break;
        }
    }
    assert!(monitored.contains("privacy-fence"));
    let slow: Value = redis::cmd("SLOWLOG").arg(("GET", 100)).query(&mut server)?;
    let slow = format!("{slow:?}");
    for text in [&monitored, &slow] {
        assert!(!text.contains("stub-password"));
        assert!(!text.contains("private-wire-sentinel"));
        assert!(text.contains("redacted"));
    }
    drain(&mut server)?;
    Ok(())
}

#[test]
fn test_auth_callbacks_missing_api_and_failed_load() -> Result<()> {
    for (arg, api) in [
        ("missing-register", "RedisModule_RegisterAuthCallback"),
        ("missing-audit", "RedisModule_ACLAddLogEntryByUserName"),
        ("fail", "auth_callbacks initialized"),
    ] {
        let mut server = TestConnection::new("hello");
        redis::cmd("MODULE")
            .arg("LOAD")
            .arg(utils::module_path("auth_callbacks")?)
            .arg(arg)
            .query::<()>(&mut server)
            .expect_err("declared hook load failure");
        assert!(server.server_log()?.contains(api));
        redis::cmd("MODULE")
            .arg("LOAD")
            .arg(utils::module_path("auth_callbacks")?)
            .query::<()>(&mut server)?;
        prepare(&mut server)?;
        let mut client = server.connection()?;
        auth(&mut client, b"stub", b"stub-password")?;
        drain(&mut server)?;
        unload(&mut server, "auth_callbacks")?;
    }
    Ok(())
}

#[test]
fn test_auth_callbacks_unsupported_runtime_refuses_declared_hook() -> Result<()> {
    let mut server = TestConnection::new("hello");
    let info: String = redis::cmd("INFO").arg("server").query(&mut server)?;
    let supported = info
        .lines()
        .find_map(|l| l.strip_prefix("redis_version:"))
        .is_some_and(|v| {
            let mut parts = v.split('.').filter_map(|p| p.parse::<u32>().ok());
            (parts.next().unwrap_or(0), parts.next().unwrap_or(0)) >= (7, 2)
        });
    let result = redis::cmd("MODULE")
        .arg("LOAD")
        .arg(utils::module_path("auth_callbacks")?)
        .query::<()>(&mut server);
    if supported {
        result?;
        unload(&mut server, "auth_callbacks")?;
    } else {
        result.expect_err("unsupported hook must fail load");
        let log = server.server_log()?;
        assert!(log.contains("RedisModule_RegisterAuthCallback"));
        assert!(
            !log.contains("auth_callbacks initialized"),
            "preflight precedes init"
        );
    }
    Ok(())
}
