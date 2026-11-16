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
    for rules in [
        "reset on +@all \"unterminated",
        "reset on +@all 'unterminated",
        "reset on +@all \"closed\"suffix",
        "reset on +@all 'closed'suffix",
    ] {
        redis::cmd("auth_users.acl")
            .arg(("owner", rules))
            .query::<()>(&mut con)
            .expect_err("malformed lexical ACL string");
        redis::cmd("SET")
            .arg(("allowed:key", "x"))
            .query::<()>(&mut client)
            .expect_err("lexical error preserves +get only");
        redis::cmd("GET")
            .arg("denied:key")
            .query::<Option<String>>(&mut client)
            .expect_err("lexical error preserves key pattern");
    }
    redis::cmd("auth_users.acl")
        .arg(("owner", "reset 'on' \"+get\" ~allowed:*"))
        .query::<()>(&mut con)?;
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
    redis::cmd("auth_users.attach")
        .arg(("owner", "none"))
        .query::<i64>(&mut no_callback)?;
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
fn test_module_users_worker_refusal_and_main_thread_cleanup_panics() -> Result<()> {
    let mut con = TestConnection::new("auth_users");
    create(&mut con, "owner")?;
    let mut client = con.connection()?;
    let id: i64 = redis::cmd("auth_users.attach")
        .arg(("owner", "track"))
        .query(&mut client)?;
    redis::cmd("auth_users.worker")
        .arg(id)
        .query::<()>(&mut con)?;
    utils::wait_until(Duration::from_secs(2), "worker refusal probes", || {
        Ok(redis::cmd("auth_users.finish").query::<i64>(&mut con)? == 1)
    })?;
    let results: Vec<String> = redis::cmd("auth_users.worker_results").query(&mut con)?;
    assert_eq!(
        results[..5],
        vec!["ERR client identity operations require the Redis main thread"; 5]
    );
    assert_eq!(results[5], "0");
    assert_eq!(stats(&mut con)?, [0, 0]);
    assert_eq!(redis::cmd("PING").query::<String>(&mut client)?, "PONG");
    redis::cmd("auth_users.revoke")
        .arg(id)
        .query::<()>(&mut con)?;
    for (index, mode) in ["panic", "drop-panic"].iter().enumerate() {
        let mut client = con.connection()?;
        let id: i64 = redis::cmd("auth_users.attach")
            .arg(("owner", *mode))
            .query(&mut client)?;
        redis::cmd("auth_users.revoke")
            .arg(id)
            .query::<()>(&mut con)?;
        let count = index as i64 + 2;
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

#[test]
fn test_module_users_scoped_context_reentry_retains_native_lifetimes() -> Result<()> {
    for action in [
        "same-client",
        "remove-incoming",
        "delete-acl",
        "attach-again",
    ] {
        let mut con = TestConnection::new("auth_users");
        create(&mut con, "owner")?;
        redis::cmd("ACL")
            .arg(("SETUSER", "reentry-acl", "on", "+@all", "~*"))
            .query::<()>(&mut con)?;
        let mut client = con.connection()?;
        redis::cmd("auth_users.reentry")
            .arg(action)
            .query::<i64>(&mut client)?;
        assert_eq!(
            redis::cmd("auth_users.reentry_result").query::<i64>(&mut con)?,
            1,
            "{action}"
        );
        assert_eq!(stats(&mut con)?, [1, 1], "{action}");
        if action != "attach-again" {
            redis::cmd("PING")
                .query::<String>(&mut client)
                .expect_err("reentrant revocation");
        }
        redis::cmd("MODULE")
            .arg(("UNLOAD", "auth_users"))
            .query::<()>(&mut con)?;
    }
    Ok(())
}

#[test]
fn test_module_users_passive_handle_drop_and_runtime_capabilities() -> Result<()> {
    let mut con = TestConnection::new("auth_users");
    create(&mut con, "original")?;
    redis::cmd("auth_users.alias")
        .arg(("original", "clone"))
        .query::<()>(&mut con)?;
    redis::cmd("auth_users.forget")
        .arg("original")
        .query::<()>(&mut con)?;
    let mut client = con.connection()?;
    redis::cmd("auth_users.attach")
        .arg(("clone", "track"))
        .query::<i64>(&mut client)?;
    assert_eq!(redis::cmd("PING").query::<String>(&mut client)?, "PONG");
    let info: String = redis::cmd("INFO").arg("server").query(&mut con)?;
    let legacy = info
        .lines()
        .any(|line| line.starts_with("redis_version:6."));
    let outcomes: Vec<String> = redis::cmd("auth_users.capabilities")
        .arg("clone")
        .query(&mut con)?;
    if legacy {
        assert_eq!(
            outcomes,
            [
                "ERR RedisModule_SetModuleUserACLString is not available in this Redis server",
                "ERR RedisModule_RedactClientCommandArgument is not available in this Redis server"
            ]
        );
    } else {
        assert_eq!(outcomes, ["OK", "OK"]);
    }
    redis::cmd("auth_users.create")
        .arg(b"invalid\0name".as_slice())
        .query::<()>(&mut con)
        .expect_err("NUL name");
    // A script's internal client cannot acquire an attached network identity.
    redis::cmd("EVAL")
        .arg((
            "return redis.call('auth_users.attach_acl', 'default', 'track')",
            0,
        ))
        .query::<i64>(&mut con)
        .expect_err("internal client attachment");
    assert_eq!(stats(&mut con)?, [0, 1]);
    redis::cmd("auth_users.remove")
        .arg("clone")
        .query::<()>(&mut con)?;
    utils::wait_until(Duration::from_secs(2), "clone native owner removal", || {
        Ok(stats(&mut con)? == [1, 2])
    })?;
    Ok(())
}

#[test]
fn test_module_users_native_post_load_rejection_allows_reload() -> Result<()> {
    let mut con = TestConnection::new("auth_users");
    redis::cmd("MODULE")
        .arg(("UNLOAD", "auth_users"))
        .query::<()>(&mut con)?;
    let initialized = con.server_log()?.matches("auth_users initialized").count();
    redis::cmd("MODULE")
        .arg("LOADEX")
        .arg(utils::module_path("auth_users")?)
        .arg(("CONFIG", "auth_users.unknown", "1"))
        .query::<()>(&mut con)
        .expect_err("post-OnLoad unknown configuration");
    assert_eq!(
        con.server_log()?.matches("auth_users initialized").count(),
        initialized + 1
    );
    redis::cmd("MODULE")
        .arg("LOAD")
        .arg(utils::module_path("auth_users")?)
        .query::<()>(&mut con)?;
    create(&mut con, "reloaded")?;
    redis::cmd("MODULE")
        .arg(("UNLOAD", "auth_users"))
        .query::<()>(&mut con)?;
    Ok(())
}

#[test]
fn test_module_users_early_refusals_contain_capture_destructors() -> Result<()> {
    let mut con = TestConnection::new("auth_users");
    create(&mut con, "owner")?;
    redis::cmd("auth_users.remove")
        .arg("owner")
        .query::<()>(&mut con)?;
    let error = redis::cmd("auth_users.attach")
        .arg(("owner", "drop-panic"))
        .query::<i64>(&mut con)
        .expect_err("stale handle callback capture");
    assert!(error.to_string().contains("stale or foreign"));
    assert!(!error.to_string().contains("panicked"));
    assert_eq!(stats(&mut con)?, [0, 1]);
    create(&mut con, "owner")?;
    let errors: Vec<String> = redis::cmd("auth_users.absent_api").query(&mut con)?;
    assert!(
        errors
            .iter()
            .all(|error| error.contains("is not available") && !error.contains("panicked"))
    );
    assert_eq!(stats(&mut con)?, [0, 3]);
    redis::cmd("auth_users.early_unload").query::<()>(&mut con)?;
    redis::cmd("MODULE")
        .arg(("UNLOAD", "auth_users"))
        .query::<()>(&mut con)?;
    assert!(
        con.server_log()?
            .contains("auth_users inactive callback drop contained")
    );
    Ok(())
}

#[test]
fn test_module_users_pubsub_destruction_defers_consumers_across_native_calls() -> Result<()> {
    let modes = vec!["module", "call", "typed"];
    #[cfg(any(
        feature = "min-redis-compatibility-version-7-2",
        feature = "min-redis-compatibility-version-7-4",
        feature = "min-redis-compatibility-version-8-0"
    ))]
    let modes = {
        let mut modes = modes;
        modes.push("blocking");
        modes
    };
    for mode in modes {
        let mut con = TestConnection::new("auth_users");
        let name = "pubsub-owner";
        if mode == "module" {
            create(&mut con, name)?;
            redis::cmd("auth_users.rule")
                .arg((name, "&*"))
                .query::<()>(&mut con)?;
        } else {
            redis::cmd("ACL")
                .arg(("SETUSER", name, "on", "+@all", "~*", "&*"))
                .query::<()>(&mut con)?;
        }
        let mut client = con.connection()?;
        if mode != "module" {
            create(&mut con, "explicit-bootstrap")?;
            redis::cmd("auth_users.attach")
                .arg(("explicit-bootstrap", "none"))
                .query::<i64>(&mut client)?;
        }
        redis::cmd("auth_users.attach_pubsub_cleanup")
            .arg((if mode == "module" { "module" } else { "acl" }, name))
            .query::<i64>(&mut client)?;
        redis::cmd("SUBSCRIBE")
            .arg("channel")
            .query::<redis::Value>(&mut client)?;
        redis::cmd("auth_users.restrict_pubsub")
            .arg((mode, name))
            .query::<()>(&mut con)?;
        utils::wait_until(Duration::from_secs(2), "native Pub/Sub cleanup", || {
            Ok(redis::cmd("auth_users.reentry_result").query::<i64>(&mut con)? == 1)
        })?;
        assert_eq!(stats(&mut con)?, [1, 1], "{mode}");
        let timing: Vec<i64> = redis::cmd("auth_users.cleanup_timing").query(&mut con)?;
        assert!(timing == [1, 0] || timing == [0, 1], "{mode}: {timing:?}");
        let raw_errors: Vec<String> = redis::cmd("auth_users.raw_probe_result").query(&mut con)?;
        if mode == "module" && timing == [1, 0] {
            assert_eq!(
                raw_errors,
                vec!["ERR module user is in a native operation"; 3]
            );
        } else {
            assert!(raw_errors.is_empty());
        }
        if timing == [1, 0] {
            if mode == "module" {
                redis::cmd("auth_users.remove")
                    .arg(name)
                    .query::<()>(&mut con)
                    .expect_err("synchronous consumer removed owner");
            } else {
                assert_eq!(
                    redis::cmd("ACL")
                        .arg(("GETUSER", name))
                        .query::<Option<redis::Value>>(&mut con)?,
                    None
                );
            }
        } else {
            // Asynchronous destruction delivers cleanup after the scoped
            // context ended. Verify the policy was applied before removing it.
            let mut probe = con.connection()?;
            if mode != "module" {
                redis::cmd("auth_users.attach")
                    .arg(("explicit-bootstrap", "none"))
                    .query::<i64>(&mut probe)?;
            }
            redis::cmd(if mode == "module" {
                "auth_users.attach"
            } else {
                "auth_users.attach_acl"
            })
            .arg((name, "none"))
            .query::<i64>(&mut probe)?;
            redis::cmd("PUBLISH")
                .arg(("channel", "message"))
                .query::<i64>(&mut probe)
                .expect_err("new channel restrictions retained");
            if mode == "module" {
                redis::cmd("auth_users.remove")
                    .arg(name)
                    .query::<()>(&mut con)?;
            } else {
                redis::cmd("ACL")
                    .arg(("DELUSER", name))
                    .query::<i64>(&mut con)?;
            }
        }
        redis::cmd("MODULE")
            .arg(("UNLOAD", "auth_users"))
            .query::<()>(&mut con)?;
    }
    Ok(())
}

#[test]
fn test_module_users_yielded_auth_defers_scoped_context_cleanup() -> Result<()> {
    let mut con = TestConnection::new("auth_users");
    redis::cmd("CONFIG")
        .arg(("SET", "busy-reply-threshold", 1))
        .query::<()>(&mut con)?;
    let name = "yield-acl";
    redis::cmd("ACL")
        .arg(("SETUSER", name, "on", "nopass", "+@all", "~*", "&*"))
        .query::<()>(&mut con)?;
    create(&mut con, "explicit-bootstrap")?;
    let mut client = con.connection()?;
    redis::cmd("auth_users.attach")
        .arg(("explicit-bootstrap", "none"))
        .query::<i64>(&mut client)?;
    redis::cmd("auth_users.attach_pubsub_cleanup")
        .arg(("acl", name))
        .query::<i64>(&mut client)?;
    let ready = std::env::temp_dir().join(format!(
        "sdk-auth-yield-{}-{}",
        std::process::id(),
        con.port()
    ));
    let mut admin = con.connection()?;
    admin.set_read_timeout(Some(Duration::from_secs(7)))?;
    let marker = ready.clone();
    let yielding = std::thread::spawn(move || {
        redis::cmd("auth_users.yield_reentry")
            .arg(marker.to_str().unwrap())
            .query::<()>(&mut admin)
    });
    utils::wait_until(Duration::from_secs(2), "auth yield scoped context", || {
        Ok(ready.exists())
    })?;
    let auth = redis::cmd("AUTH")
        .arg((name, "anything"))
        .query::<()>(&mut client);
    if let Err(error) = auth {
        assert!(error.is_io_error(), "{error}");
    }
    yielding.join().expect("yield client thread panicked")?;
    std::fs::remove_file(ready)?;
    assert_eq!(
        redis::cmd("auth_users.reentry_result").query::<i64>(&mut con)?,
        1
    );
    assert_eq!(stats(&mut con)?, [1, 1]);
    redis::cmd("MODULE")
        .arg(("UNLOAD", "auth_users"))
        .query::<()>(&mut con)?;
    Ok(())
}

#[test]
fn test_module_users_acl_handoff_requires_installed_sdk_ownership() -> Result<()> {
    const REFUSAL: &str = "ERR ACL attachment requires an installed SDK client identity";
    let mut con = TestConnection::new("auth_users");
    create(&mut con, "owner")?;
    let mut client = con.connection()?;
    let first: Vec<String> = redis::cmd("auth_users.checked_probe").query(&mut client)?;
    assert_eq!(first, [REFUSAL, "0"]);
    assert_eq!(
        redis::cmd("ACL")
            .arg("WHOAMI")
            .query::<String>(&mut client)?,
        "default"
    );
    assert_eq!(stats(&mut con)?, [0, 1]);
    redis::cmd("ACL")
        .arg(("SETUSER", "foreign-acl", "on", "nopass", "+@all", "~*"))
        .query::<()>(&mut con)?;
    redis::cmd("auth_users.foreign")
        .arg("foreign-acl")
        .query::<i64>(&mut client)?;
    let foreign: Vec<String> = redis::cmd("auth_users.checked_probe").query(&mut client)?;
    assert_eq!(foreign, [REFUSAL, "0"]);
    assert_eq!(
        redis::cmd("ACL")
            .arg("WHOAMI")
            .query::<String>(&mut client)?,
        "foreign-acl"
    );
    assert_eq!(
        redis::cmd("auth_users.foreign_results").query::<Vec<String>>(&mut con)?,
        ["0"]
    );
    assert_eq!(stats(&mut con)?, [0, 2]);
    // During created-user native attachment only a pending new registration
    // exists. A raw prior callback must not mistake it for installed ownership.
    redis::cmd("auth_users.pending_probe").query::<i64>(&mut client)?;
    let errors: Vec<String> = redis::cmd("auth_users.foreign_results").query(&mut con)?;
    assert_eq!(
        &errors[..2],
        ["ERR module user is in a native operation"; 2]
    );
    let info: String = redis::cmd("INFO").arg("server").query(&mut con)?;
    if info
        .lines()
        .any(|line| line.starts_with("redis_version:6."))
    {
        assert!(errors[2].contains("SetModuleUserACLString is not available"));
    } else {
        assert_eq!(errors[2], "ERR module user is in a native operation");
    }
    assert_eq!(&errors[3..], [REFUSAL, "1"]);
    assert_eq!(
        redis::cmd("ACL")
            .arg("WHOAMI")
            .query::<String>(&mut client)?,
        "owner"
    );
    assert_eq!(stats(&mut con)?, [0, 3]);
    // The caller explicitly established an owned identity; ACL handoff is now legal.
    redis::cmd("auth_users.attach_acl")
        .arg(("default", "track"))
        .query::<i64>(&mut client)?;
    assert_eq!(
        redis::cmd("ACL")
            .arg("WHOAMI")
            .query::<String>(&mut client)?,
        "default"
    );
    redis::cmd("AUTH")
        .arg(("default", "anything"))
        .query::<()>(&mut client)?;
    assert_eq!(stats(&mut con)?, [1, 4]);
    // A lawful native overwrite consumes the old SDK installation immediately;
    // its now-foreign callback must not inherit that old ownership capability.
    redis::cmd("auth_users.attach")
        .arg(("owner", "track"))
        .query::<i64>(&mut client)?;
    redis::cmd("auth_users.foreign")
        .arg("foreign-acl")
        .query::<i64>(&mut client)?;
    assert_eq!(stats(&mut con)?, [2, 5]);
    let overwritten: Vec<String> = redis::cmd("auth_users.checked_probe").query(&mut client)?;
    assert_eq!(overwritten, [REFUSAL, "0"]);
    assert_eq!(
        redis::cmd("ACL")
            .arg("WHOAMI")
            .query::<String>(&mut client)?,
        "foreign-acl"
    );
    assert_eq!(stats(&mut con)?, [2, 6]);
    Ok(())
}
