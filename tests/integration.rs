use std::collections::HashMap;
use std::thread;
use std::time::Duration;
use std::time::SystemTime;

use crate::utils::TestConnection;
use anyhow::Context;
use anyhow::Result;
use redis::{RedisError, RedisResult, Value, VerbatimFormat};
use redis_module::RedisValue;

mod configuration_ownership;
mod utils;

#[cfg(feature = "redis-7-2-examples")]
#[test]
fn test_notification_job_rejection_releases_captures() -> Result<()> {
    let mut primary = TestConnection::new("keyspace_to_stream");
    let accepted: Vec<i64> = redis::cmd("notify_stream.job_probe").query(&mut primary)?;
    assert_eq!(accepted, [1, 0]);
    let counts: Vec<i64> = redis::cmd("notify_stream.probe_counts").query(&mut primary)?;
    assert_eq!(counts, [1, 1]);

    let mut replica = primary.replica()?;
    let rejected: Vec<i64> = redis::cmd("notify_stream.job_probe").query(&mut replica)?;
    assert_eq!(
        rejected,
        [0, 1],
        "rejected jobs must drop captured data immediately"
    );
    let counts: Vec<i64> = redis::cmd("notify_stream.probe_counts").query(&mut replica)?;
    assert_eq!(counts, [1, 0], "rejected jobs must never run");
    Ok(())
}

#[cfg(feature = "redis-7-2-examples")]
mod notification_write_tests {
    use super::*;
    type Entries = Vec<(String, Vec<Vec<u8>>)>;

    fn entries(con: &mut TestConnection) -> Result<Entries> {
        redis::cmd("SELECT").arg(0).query::<()>(con)?;
        let reply: Value = redis::cmd("XRANGE")
            .arg(&["watched:events", "-", "+"])
            .query(con)?;
        let Value::Array(rows) = reply else {
            anyhow::bail!("XRANGE must return an array");
        };
        rows.into_iter()
            .map(|row| {
                let Value::Array(parts) = row else {
                    anyhow::bail!("stream entry must be an array");
                };
                anyhow::ensure!(parts.len() == 2, "stream entry has ID and fields");
                Ok((
                    redis::from_redis_value_ref(&parts[0])?,
                    redis::from_redis_value_ref(&parts[1])?,
                ))
            })
            .collect()
    }

    fn stats(con: &mut TestConnection) -> Result<Vec<i64>> {
        Ok(redis::cmd("notify_stream.stats").query(con)?)
    }

    #[test]
    fn test_notification_writes_keep_binary_keys_database_and_feedback_guard() -> Result<()> {
        let mut con = TestConnection::new("keyspace_to_stream");
        let config: Vec<String> = redis::cmd("CONFIG")
            .arg(&["GET", "notify-keyspace-events"])
            .query(&mut con)?;
        assert_eq!(
            config[1], "",
            "module events do not need Pub/Sub notifications"
        );
        redis::cmd("SELECT").arg(3).query::<()>(&mut con)?;
        let key = b"watched:binary\xff\0key";
        redis::cmd("SET")
            .arg(key.as_slice())
            .arg("value")
            .query::<()>(&mut con)?;
        assert_eq!(
            redis::cmd("GET")
                .arg(key.as_slice())
                .query::<String>(&mut con)?,
            "value"
        );
        assert_eq!(
            redis::cmd("EXISTS")
                .arg("watched:events")
                .query::<i64>(&mut con)?,
            0
        );
        let rows = entries(&mut con)?;
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].1,
            vec![
                b"db".to_vec(),
                b"3".to_vec(),
                b"event".to_vec(),
                b"set".to_vec(),
                b"key".to_vec(),
                key.to_vec()
            ]
        );
        assert_eq!(&stats(&mut con)?[..3], [1, 1, 0]);
        // Even a client write to the reserved output key cannot feed back.
        redis::cmd("XADD")
            .arg(&["watched:events", "*", "external", "entry"])
            .query::<String>(&mut con)?;
        assert_eq!(&stats(&mut con)?[..3], [1, 1, 0]);
        assert_eq!(entries(&mut con)?.len(), 2);
        Ok(())
    }

    #[test]
    fn test_notification_writes_replicate_once_and_skip_writable_replicas() -> Result<()> {
        let mut primary = TestConnection::new("keyspace_to_stream");
        redis::cmd("SELECT").arg(3).query::<()>(&mut primary)?;
        redis::cmd("SET")
            .arg(&["watched:before", "one"])
            .query::<()>(&mut primary)?;
        let mut replica = primary.replica()?;
        let first = entries(&mut primary)?;
        assert_eq!(entries(&mut replica)?, first);
        redis::cmd("SELECT").arg(3).query::<()>(&mut primary)?;
        redis::cmd("SET")
            .arg(&["watched:after", "two"])
            .query::<()>(&mut primary)?;
        let expected = entries(&mut primary)?;
        assert_eq!(expected.len(), 2);
        utils::wait_until(
            Duration::from_secs(3),
            "replicated notification stream",
            || Ok(entries(&mut replica)? == expected),
        )?;
        assert_eq!(&stats(&mut replica)?[..3], [0, 0, 0]);
        // The primary-only policy also applies to explicitly writable replicas.
        redis::cmd("CONFIG")
            .arg(&["SET", "replica-read-only", "no"])
            .query::<()>(&mut replica)?;
        redis::cmd("SELECT").arg(3).query::<()>(&mut replica)?;
        redis::cmd("SET")
            .arg(&["watched:local", "replica"])
            .query::<()>(&mut replica)?;
        assert_eq!(entries(&mut replica)?, expected);
        let counts = stats(&mut replica)?;
        assert_eq!(&counts[..3], [0, 0, 0]);
        assert!(counts[3] > 0);
        Ok(())
    }

    #[test]
    fn test_notification_writes_skip_rdb_loading() -> Result<()> {
        let mut con = TestConnection::new("keyspace_to_stream");
        redis::cmd("SELECT").arg(2).query::<()>(&mut con)?;
        redis::cmd("SET")
            .arg(&["watched:saved", "value"])
            .query::<()>(&mut con)?;
        let saved = entries(&mut con)?;
        con = con.restart()?;
        assert_eq!(entries(&mut con)?, saved);
        let counts = stats(&mut con)?;
        assert_eq!(&counts[..3], [0, 0, 0]);
        assert!(
            counts[3] > 0,
            "LOADED callbacks should be observed and skipped"
        );
        redis::cmd("SET")
            .arg(&["watched:new", "value"])
            .query::<()>(&mut con)?;
        assert_eq!(entries(&mut con)?.len(), 2);
        Ok(())
    }

    #[test]
    fn test_notification_write_errors_and_unload() -> Result<()> {
        let mut con = TestConnection::new("keyspace_to_stream");
        redis::cmd("SET")
            .arg(&["watched:events", "wrong-type"])
            .query::<()>(&mut con)?;
        // The source write succeeds even when the derived stream write fails.
        redis::cmd("SET")
            .arg(&["watched:first", "value"])
            .query::<()>(&mut con)?;
        assert_eq!(&stats(&mut con)?[..3], [1, 0, 1]);
        assert!(
            con.server_log()?
                .contains("notification stream append failed")
        );
        redis::cmd("DEL")
            .arg("watched:events")
            .query::<()>(&mut con)?;
        redis::cmd("SET")
            .arg(&["watched:second", "value"])
            .query::<()>(&mut con)?;
        assert_eq!(entries(&mut con)?.len(), 1);
        redis::cmd("MODULE")
            .arg(&["UNLOAD", "keyspace_to_stream"])
            .query::<()>(&mut con)?;
        redis::cmd("SET")
            .arg(&["watched:unloaded", "value"])
            .query::<()>(&mut con)?;
        assert_eq!(entries(&mut con)?.len(), 1);
        let path = utils::module_path("keyspace_to_stream")?;
        redis::cmd("MODULE")
            .arg("LOAD")
            .arg(path)
            .query::<()>(&mut con)?;
        redis::cmd("SET")
            .arg(&["watched:reloaded", "value"])
            .query::<()>(&mut con)?;
        assert_eq!(entries(&mut con)?.len(), 2);
        Ok(())
    }
}

#[test]
fn test_registration_context_registers_load_time_and_declarative_commands() -> Result<()> {
    for name in [None, Some("registration.from_args")] {
        let args = name.into_iter().collect::<Vec<_>>();
        let mut con = TestConnection::with_args("registration", &args);
        for command in [
            name.unwrap_or("registration.manual"),
            "registration.legacy",
            "registration.builder",
        ] {
            assert_eq!(redis::cmd(command).query::<String>(&mut con)?, "PONG");
        }
        assert!(
            con.server_log()?
                .contains("registration runtime helper succeeded")
        );
        assert!(
            con.server_log()?
                .contains("registration runtime init hook succeeded")
        );
    }
    Ok(())
}

fn configure_context_acl_user(con: &mut TestConnection) -> Result<()> {
    redis::cmd("ACL")
        .arg(&[
            "SETUSER",
            "context-reader",
            "on",
            "nopass",
            "~allowed:*",
            "+get",
        ])
        .query::<()>(con)?;
    redis::cmd("ACL")
        .arg(&[
            "SETUSER",
            "context-other",
            "on",
            "nopass",
            "~denied:*",
            "+get",
        ])
        .query::<()>(con)?;
    redis::cmd("MSET")
        .arg(&["allowed:item", "1", "denied:item", "2"])
        .query::<()>(con)?;
    Ok(())
}

fn assert_context_user_flags(flags: &[i64]) {
    assert!(flags.len() >= 35, "missing nested-scope checks: {flags:?}");
    assert!(
        flags[2..].iter().all(|flag| *flag == 1),
        "scope checks failed: {flags:?}"
    );
}

#[test]
fn test_client_context_user_restores_nested_scopes() -> Result<()> {
    let mut con = TestConnection::new("context_acl");
    configure_context_acl_user(&mut con)?;
    let flags: Vec<i64> = redis::cmd("context_acl.client")
        .arg("context-reader")
        .query(&mut con)?;
    assert_context_user_flags(&flags);
    Ok(())
}

#[test]
fn test_worker_context_user_enforces_acl_and_clears_on_return_and_unwind() -> Result<()> {
    for mode in ["plain", "detached"] {
        let mut con = TestConnection::new("context_acl");
        configure_context_acl_user(&mut con)?;
        let flags: Vec<i64> = redis::cmd("context_acl.worker")
            .arg(&[mode, "context-reader"])
            .query(&mut con)?;
        let baseline = &flags[..2];
        assert_context_user_flags(&flags);
        utils::wait_until(Duration::from_secs(2), "ACL worker exit", || {
            Ok(redis::cmd("context_acl.finish").query::<i64>(&mut con)? == 1)
        })?;
        // The detached context is reused. Its scoped identity must not leak
        // into a later lock invocation either.
        let repeated: Vec<i64> = redis::cmd("context_acl.worker")
            .arg(&[mode, "context-reader"])
            .query(&mut con)?;
        assert_eq!(
            &repeated[..2],
            baseline,
            "baseline changed on reused {mode}"
        );
        assert_context_user_flags(&repeated);
        utils::wait_until(Duration::from_secs(2), "second ACL worker exit", || {
            Ok(redis::cmd("context_acl.finish").query::<i64>(&mut con)? == 1)
        })?;
    }
    Ok(())
}

#[test]
fn test_event_context_user_enforces_acl_and_clears_on_return_and_unwind() -> Result<()> {
    let mut con = TestConnection::new("context_acl");
    configure_context_acl_user(&mut con)?;
    redis::cmd("context_acl.timer")
        .arg("context-reader")
        .query::<()>(&mut con)?;
    let mut flags: Option<Vec<i64>> = None;
    utils::wait_until(Duration::from_secs(2), "ACL event callback", || {
        flags = redis::cmd("context_acl.result").query(&mut con)?;
        Ok(flags.is_some())
    })?;
    assert_context_user_flags(&flags.unwrap());
    Ok(())
}

#[test]
fn test_context_user_registry_survives_module_unload_and_reload() -> Result<()> {
    let mut con = TestConnection::new("context_acl");
    configure_context_acl_user(&mut con)?;
    let before: Vec<i64> = redis::cmd("context_acl.client")
        .arg("context-reader")
        .query(&mut con)?;
    assert_context_user_flags(&before);
    for mode in ["plain", "detached"] {
        let flags: Vec<i64> = redis::cmd("context_acl.worker")
            .arg(&[mode, "context-reader"])
            .query(&mut con)?;
        assert_context_user_flags(&flags);
        utils::wait_until(Duration::from_secs(5), "scoped-user worker exit", || {
            Ok(redis::cmd("context_acl.finish").query::<i64>(&mut con)? == 1)
        })?;
        redis::cmd("MODULE")
            .arg(&["UNLOAD", "context_acl"])
            .query::<()>(&mut con)?;
        assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
        redis::cmd("MODULE")
            .arg("LOAD")
            .arg(utils::module_path("context_acl")?)
            .query::<()>(&mut con)?;
        let after: Vec<i64> = redis::cmd("context_acl.client")
            .arg("context-reader")
            .query(&mut con)?;
        assert_context_user_flags(&after);
        assert_eq!(
            &after[..2],
            &before[..2],
            "fresh registry after {mode} reload"
        );
    }
    Ok(())
}

#[test]
fn test_context_try_lock_contention_release_and_unload() -> Result<()> {
    let mut con = TestConnection::new("try_lock");
    con.set_read_timeout(Some(Duration::from_secs(5)))?;
    let module = utils::module_path("try_lock")?;
    for mode in ["thread", "detached"] {
        let contended: Vec<i64> = redis::cmd("try_lock.contended").query(&mut con)?;
        assert_eq!(contended, [1, 1], "both attempts must return on contention");
        redis::cmd("DEL")
            .arg("try_lock:count")
            .query::<()>(&mut con)?;
        redis::cmd("try_lock.start")
            .arg(mode)
            .query::<()>(&mut con)?;
        utils::wait_until(
            Duration::from_secs(5),
            "worker repeatedly acquires and releases Redis",
            || {
                Ok(redis::cmd("GET")
                    .arg("try_lock:count")
                    .query::<Option<i64>>(&mut con)?
                    .unwrap_or(0)
                    >= 2)
            },
        )?;
        // Stopping a worker while the command holds Redis's lock must finish.
        redis::cmd("try_lock.stop").query::<()>(&mut con)?;
        let stopped: i64 = redis::cmd("GET").arg("try_lock:count").query(&mut con)?;
        assert!(stopped >= 2, "{mode} worker used its guard for Redis calls");

        redis::cmd("try_lock.start")
            .arg(mode)
            .query::<()>(&mut con)?;
        utils::wait_until(
            Duration::from_secs(5),
            "restarted worker acquires Redis",
            || {
                Ok(redis::cmd("GET")
                    .arg("try_lock:count")
                    .query::<i64>(&mut con)?
                    > stopped)
            },
        )?;
        // deinit also stops and joins the live worker under Redis's lock.
        redis::cmd("MODULE")
            .arg(&["UNLOAD", "try_lock"])
            .query::<()>(&mut con)?;
        assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
        redis::cmd("MODULE")
            .arg("LOAD")
            .arg(&module)
            .query::<()>(&mut con)?;
    }
    Ok(())
}

#[test]
fn test_detached_context_is_fresh_after_unload_and_reload() -> Result<()> {
    let mut con = TestConnection::new("detached_reload");
    let module = utils::module_path("detached_reload")?;
    let exercise = |con: &mut TestConnection, round: i64| -> Result<()> {
        let _: String = redis::cmd("detached.log")
            .arg(format!("round {round}"))
            .query(con)?;
        let pong: String = redis::cmd("detached.worker").query(con)?;
        assert_eq!(pong, "PONG", "round {round}");
        utils::wait_until(Duration::from_secs(5), "detached worker exit", || {
            Ok(redis::cmd("detached.finish").query::<i64>(con)? == 1)
        })
    };
    let unload = |con: &mut TestConnection| -> Result<()> {
        Ok(redis::cmd("MODULE")
            .arg(&["UNLOAD", "detached_reload"])
            .query::<()>(con)?)
    };
    let assert_loads = |con: &mut TestConnection, expected: i64| -> Result<()> {
        let loads: i64 = redis::cmd("detached.loads").query(con)?;
        assert_eq!(
            loads, expected,
            "the image was unmapped at unload, so a reload did not reuse it"
        );
        Ok(())
    };
    for round in 0..3 {
        exercise(&mut con, round)?;
        unload(&mut con)?;
        // init installs the SDK logger again on the reloaded module's context.
        redis::cmd("MODULE")
            .arg("LOAD")
            .arg(&module)
            .query::<()>(&mut con)
            .with_context(|| format!("reload after round {round}"))?;
        assert_loads(&mut con, round + 2)?;
    }
    // A load that fails after storing its context releases it.
    unload(&mut con)?;
    assert!(
        redis::cmd("MODULE")
            .arg(&["LOAD", &module, "fail"])
            .query::<()>(&mut con)
            .is_err()
    );
    redis::cmd("MODULE")
        .arg("LOAD")
        .arg(&module)
        .query::<()>(&mut con)
        .context("reload after the failed load")?;
    assert_loads(&mut con, 6)?;
    exercise(&mut con, 3)?;
    let log = con.server_log()?;
    for round in 0..4 {
        // The module name prefix shows the logger used a module context.
        assert!(
            log.contains(&format!("<detached_reload> detached.log round {round}")),
            "round {round} log line"
        );
    }
    // A load that found an earlier load's context would have replaced it.
    assert!(
        !log.contains("replaced a detached module context"),
        "a reload found a stale detached context"
    );
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    Ok(())
}

#[test]
fn test_timer_event_context_uses_its_selected_database() -> Result<()> {
    let mut con = TestConnection::new("timer");
    redis::cmd("SELECT").arg(3).query::<()>(&mut con)?;
    redis::cmd("timer.create")
        .arg(&["1", "payload"])
        .query::<String>(&mut con)?;
    utils::wait_until(Duration::from_secs(2), "timer event callback", || {
        Ok(redis::cmd("GET")
            .arg("timer:fired")
            .query::<Option<String>>(&mut con)?
            .as_deref()
            == Some("payload"))
    })?;
    redis::cmd("SELECT").arg(0).query::<()>(&mut con)?;
    assert_eq!(
        redis::cmd("GET")
            .arg("timer:fired")
            .query::<Option<String>>(&mut con)?,
        None
    );
    Ok(())
}

fn timer_probe_counts(con: &mut TestConnection) -> Result<Vec<i64>> {
    Ok(redis::cmd("timer.probe_counts").query(con)?)
}

#[test]
fn test_typed_timer_cancellation_recovers_data_and_drops_captures_once() -> Result<()> {
    let mut con = TestConnection::new("timer");
    let id: String = redis::cmd("timer.create")
        .arg(&["60000", "payload"])
        .query(&mut con)?;
    let remaining: Option<i64> = redis::cmd("timer.info").arg(&id).query(&mut con)?;
    assert!(remaining.is_some_and(|ms| (1..=60000).contains(&ms)));
    assert_eq!(
        redis::cmd("timer.stop")
            .arg(&id)
            .query::<Option<String>>(&mut con)?,
        Some("payload".to_owned())
    );
    assert_eq!(
        redis::cmd("timer.stop")
            .arg(&id)
            .query::<Option<String>>(&mut con)?,
        None
    );
    assert_eq!(
        redis::cmd("timer.info")
            .arg(&id)
            .query::<Option<i64>>(&mut con)?,
        None
    );

    let id: String = redis::cmd("timer.probe")
        .arg(&["fire", "60000"])
        .query(&mut con)?;
    assert!(
        redis::cmd("timer.probe_info")
            .arg(&id)
            .query::<Option<i64>>(&mut con)?
            .is_some()
    );
    assert_eq!(
        redis::cmd("timer.probe_stop")
            .arg(&id)
            .query::<Option<i64>>(&mut con)?,
        Some(7)
    );
    assert_eq!(timer_probe_counts(&mut con)?, [0, 1, 1, 0, 0]);
    assert_eq!(
        redis::cmd("timer.probe_stop")
            .arg(&id)
            .query::<Option<i64>>(&mut con)?,
        None
    );
    assert_eq!(
        redis::cmd("timer.probe_info")
            .arg(&id)
            .query::<Option<i64>>(&mut con)?,
        None
    );
    assert_eq!(timer_probe_counts(&mut con)?, [0, 1, 1, 0, 0]);

    redis::cmd("timer.invalid_delay")
        .query::<Value>(&mut con)
        .expect_err("an excessive timer delay was accepted");
    assert_eq!(
        timer_probe_counts(&mut con)?,
        [0, 2, 2, 0, 0],
        "rejected creation must release both arguments"
    );
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    Ok(())
}

#[test]
fn test_typed_timer_cancellation_destructor_panic_finishes_ownership() -> Result<()> {
    let mut con = TestConnection::new("timer");
    let id: String = redis::cmd("timer.probe")
        .arg(&["cancel-panic", "60000"])
        .query(&mut con)?;
    let error = redis::cmd("timer.probe_stop")
        .arg(&id)
        .query::<Option<i64>>(&mut con)
        .expect_err("a panicking timer capture destructor was not reported");
    assert!(
        error
            .to_string()
            .contains("command panicked: timer capture drop panic")
    );
    assert_eq!(timer_probe_counts(&mut con)?, [0, 1, 1, 0, 0]);
    assert_eq!(
        redis::cmd("timer.probe_stop")
            .arg(&id)
            .query::<Option<i64>>(&mut con)?,
        None
    );
    assert_eq!(
        redis::cmd("timer.probe_info")
            .arg(&id)
            .query::<Option<i64>>(&mut con)?,
        None
    );
    assert_eq!(timer_probe_counts(&mut con)?, [0, 1, 1, 0, 0]);
    redis::cmd("MODULE")
        .arg(&["UNLOAD", "timer"])
        .query::<()>(&mut con)?;
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    Ok(())
}

#[test]
fn test_typed_timer_firing_passive_drop_panic_and_reentry() -> Result<()> {
    let mut con = TestConnection::new("timer");
    for (index, mode) in ["fire", "passive", "panic", "reenter"]
        .into_iter()
        .enumerate()
    {
        let id: String = redis::cmd("timer.probe")
            .arg(&[mode, "0"])
            .query(&mut con)?;
        let expected = i64::try_from(index + 1)?;
        utils::wait_until(
            Duration::from_secs(5),
            "timer callback and owned-value cleanup",
            || {
                let counts = timer_probe_counts(&mut con)?;
                Ok(counts[..3] == [expected, expected, expected])
            },
        )?;
        if mode != "passive" {
            assert_eq!(
                redis::cmd("timer.probe_stop")
                    .arg(&id)
                    .query::<Option<i64>>(&mut con)?,
                None
            );
            assert_eq!(
                redis::cmd("timer.probe_info")
                    .arg(&id)
                    .query::<Option<i64>>(&mut con)?,
                None
            );
        }
        assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    }
    assert_eq!(timer_probe_counts(&mut con)?, [4, 4, 4, 1, 0]);
    assert!(con.server_log()?.contains("timer probe callback panic"));
    Ok(())
}

#[test]
fn test_typed_timer_pending_unload_refusal_and_reload() -> Result<()> {
    let mut con = TestConnection::new("timer");
    let path = utils::module_path("timer")?;
    for _ in 0..2 {
        let id: String = redis::cmd("timer.probe")
            .arg(&["fire", "60000"])
            .query(&mut con)?;
        redis::cmd("MODULE")
            .arg(&["UNLOAD", "timer"])
            .query::<()>(&mut con)
            .expect_err("pending timer allowed module unload");
        assert!(
            redis::cmd("timer.probe_info")
                .arg(&id)
                .query::<Option<i64>>(&mut con)?
                .is_some()
        );
        assert_eq!(
            redis::cmd("timer.probe_stop")
                .arg(&id)
                .query::<Option<i64>>(&mut con)?,
            Some(7)
        );
        redis::cmd("MODULE")
            .arg(&["UNLOAD", "timer"])
            .query::<()>(&mut con)?;
        assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
        redis::cmd("MODULE")
            .arg("LOAD")
            .arg(&path)
            .query::<()>(&mut con)?;
        assert_eq!(timer_probe_counts(&mut con)?, [0, 0, 0, 0, 0]);
    }
    Ok(())
}

#[test]
fn test_typed_timer_init_rejects_creation_and_releases_owned_values() -> Result<()> {
    let mut con = TestConnection::new("timer");
    let path = utils::module_path("timer")?;
    redis::cmd("MODULE")
        .arg(&["UNLOAD", "timer"])
        .query::<()>(&mut con)?;
    for mode in ["init-denied", "init-error", "init-panic"] {
        let record = std::env::temp_dir().join(format!(
            "sdk-timer-rollback-{}-{}-{mode}",
            std::process::id(),
            con.port()
        ));
        let _ = std::fs::remove_file(&record);
        let result = redis::cmd("MODULE")
            .arg("LOAD")
            .arg(&path)
            .arg(mode)
            .arg(record.to_string_lossy().as_ref())
            .query::<()>(&mut con);
        if mode == "init-denied" {
            result?;
            redis::cmd("MODULE")
                .arg(&["UNLOAD", "timer"])
                .query::<()>(&mut con)?;
        } else {
            result.expect_err("timer init failure was accepted");
        }
        let bytes = std::fs::read(&record)
            .context("timer init did not record rejected creation and owned-value cleanup")?;
        let mut sorted = bytes;
        sorted.sort_unstable();
        assert_eq!(
            sorted, b"CDE",
            "creation must fail and callback and payload must each drop once"
        );
        std::fs::remove_file(&record)?;
        assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
        redis::cmd("MODULE")
            .arg("LOAD")
            .arg(&path)
            .query::<()>(&mut con)?;
        redis::cmd("MODULE")
            .arg(&["UNLOAD", "timer"])
            .query::<()>(&mut con)?;
    }
    redis::cmd("MODULE")
        .arg("LOAD")
        .arg(&path)
        .query::<()>(&mut con)?;
    Ok(())
}

#[test]
fn test_typed_timer_native_post_load_rejection_allows_reload() -> Result<()> {
    let mut con = TestConnection::new("timer");
    let path = utils::module_path("timer")?;
    redis::cmd("MODULE")
        .arg(&["UNLOAD", "timer"])
        .query::<()>(&mut con)?;
    let record = std::env::temp_dir().join(format!(
        "sdk-timer-post-load-{}-{}",
        std::process::id(),
        con.port()
    ));
    let _ = std::fs::remove_file(&record);
    // LOADEX rejects its unconsumed configuration after OnLoad returned success.
    // The SDK must survive Redis's subsequent cleanup and bind a fresh lifecycle.
    redis::cmd("MODULE")
        .arg("LOADEX")
        .arg(&path)
        .arg(&["CONFIG", "timer.unknown", "1", "ARGS", "init-denied"])
        .arg(record.to_string_lossy().as_ref())
        .query::<()>(&mut con)
        .expect_err("an unconsumed LOADEX configuration was accepted");
    let mut markers = std::fs::read(&record)
        .context("successful SDK initialization did not record rejected timer creation")?;
    markers.sort_unstable();
    assert_eq!(
        markers, b"CDE",
        "init must reject creation and release both owned values"
    );
    std::fs::remove_file(&record)?;
    let log = con.server_log()?;
    assert!(
        log.contains("Loadex configurations were not applied"),
        "{log}"
    );
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    redis::cmd("MODULE")
        .arg("LOAD")
        .arg(&path)
        .query::<()>(&mut con)?;
    let id: String = redis::cmd("timer.create")
        .arg(&["60000", "after-native-rejection"])
        .query(&mut con)?;
    assert_eq!(
        redis::cmd("timer.stop")
            .arg(&id)
            .query::<Option<String>>(&mut con)?,
        Some("after-native-rejection".to_owned())
    );
    redis::cmd("MODULE")
        .arg(&["UNLOAD", "timer"])
        .query::<()>(&mut con)?;
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    Ok(())
}

#[test]
fn test_typed_timer_unload_phase_rejects_creation_and_restores_after_refusal() -> Result<()> {
    let mut con = TestConnection::new("timer");
    let path = utils::module_path("timer")?;
    for mode in ["deinit-reject", "deinit-panic", "deinit-create"] {
        redis::cmd("MODULE")
            .arg(&["UNLOAD", "timer"])
            .query::<()>(&mut con)?;
        redis::cmd("MODULE")
            .arg("LOAD")
            .arg(&path)
            .arg(mode)
            .query::<()>(&mut con)?;
        if mode == "deinit-create" {
            redis::cmd("MODULE")
                .arg(&["UNLOAD", "timer"])
                .query::<()>(&mut con)?;
            assert_eq!(
                redis::cmd("GET")
                    .arg("timer:deinit-create-denied")
                    .query::<Option<String>>(&mut con)?,
                Some("1".to_owned())
            );
        } else {
            redis::cmd("MODULE")
                .arg(&["UNLOAD", "timer"])
                .query::<()>(&mut con)
                .expect_err("deinit refusal was ignored");
            let id: String = redis::cmd("timer.create")
                .arg(&["60000", "after-refusal"])
                .query(&mut con)?;
            assert_eq!(
                redis::cmd("timer.stop")
                    .arg(&id)
                    .query::<Option<String>>(&mut con)?,
                Some("after-refusal".to_owned())
            );
            redis::cmd("MODULE")
                .arg(&["UNLOAD", "timer"])
                .query::<()>(&mut con)?;
        }
        redis::cmd("MODULE")
            .arg("LOAD")
            .arg(&path)
            .query::<()>(&mut con)?;
    }
    Ok(())
}

#[test]
fn test_typed_timer_can_be_created_by_a_worker_notification() -> Result<()> {
    let mut con = TestConnection::with_args("timer", &["worker"]);
    for _ in 0..2 {
        redis::cmd("timer.worker").query::<()>(&mut con)?;
        utils::wait_until(Duration::from_secs(5), "timer worker join", || {
            Ok(redis::cmd("timer.finish").query::<i64>(&mut con)? == 1)
        })?;
        utils::wait_until(
            Duration::from_secs(5),
            "worker-origin timer firing and cleanup",
            || Ok(timer_probe_counts(&mut con)? == [1, 1, 1, 0, 1]),
        )?;
        redis::cmd("MODULE")
            .arg(&["UNLOAD", "timer"])
            .query::<()>(&mut con)?;
        redis::cmd("MODULE")
            .arg("LOAD")
            .arg(utils::module_path("timer")?)
            .arg("worker")
            .query::<()>(&mut con)?;
    }
    Ok(())
}

#[test]
fn test_event_context_post_notification_writes_in_selected_database() -> Result<()> {
    let mut con = TestConnection::new("events");
    redis::cmd("SELECT").arg(3).query::<()>(&mut con)?;
    redis::cmd("SET")
        .arg(&["event:source", "value"])
        .query::<()>(&mut con)?;
    assert_eq!(redis::cmd("GET").arg("num_sets").query::<i64>(&mut con)?, 1);
    redis::cmd("SELECT").arg(0).query::<()>(&mut con)?;
    assert_eq!(
        redis::cmd("GET")
            .arg("num_sets")
            .query::<Option<String>>(&mut con)?,
        None
    );
    Ok(())
}

#[test]
fn test_runtime_keyspace_mask_from_module_arguments() -> Result<()> {
    for (mask, expected) in [
        ("none", [0, 0, 0, 0, 0]),
        ("string", [1, 0, 0, 0, 0]),
        ("missed", [0, 1, 0, 0, 0]),
        ("new", [0, 0, 1, 0, 0]),
        ("string,missed,new", [1, 1, 1, 0, 0]),
    ] {
        let mut con = TestConnection::with_args("runtime_events", &[mask]);
        // Server Pub/Sub notifications remain disabled.
        let config: Vec<String> = redis::cmd("CONFIG")
            .arg(&["GET", "notify-keyspace-events"])
            .query(&mut con)?;
        assert_eq!(config[1], "");
        redis::cmd("GET").arg("missing").query::<()>(&mut con)?;
        redis::cmd("SET")
            .arg(&["key", "value"])
            .query::<()>(&mut con)?;
        let counts: Vec<i64> = redis::cmd("runtime.counts").query(&mut con)?;
        assert_eq!(counts, expected, "mask {mask}");
    }
    Ok(())
}

#[test]
fn test_runtime_keyspace_subscriptions_after_load_are_additive() -> Result<()> {
    let mut con = TestConnection::new("runtime_events");
    redis::cmd("SET")
        .arg(&["before", "value"])
        .query::<()>(&mut con)?;
    for (command, mask) in [
        ("runtime.subscribe", "string"),
        ("runtime_subscribe_proc", "missed"),
        ("runtime.subscribe_legacy", "none"),
    ] {
        redis::cmd(command).arg(mask).query::<()>(&mut con)?;
    }
    redis::cmd("GET").arg("missing").query::<()>(&mut con)?;
    redis::cmd("SET")
        .arg(&["after", "value"])
        .query::<()>(&mut con)?;
    let counts: Vec<i64> = redis::cmd("runtime.counts").query(&mut con)?;
    assert_eq!(counts, [1, 1, 0, 0, 0]);
    // Registering again adds another callback, rather than replacing the mask.
    redis::cmd("runtime.subscribe")
        .arg("string")
        .query::<()>(&mut con)?;
    redis::cmd("SET")
        .arg(&["again", "value"])
        .query::<()>(&mut con)?;
    let counts: Vec<i64> = redis::cmd("runtime.counts").query(&mut con)?;
    assert_eq!(counts, [3, 1, 0, 0, 0]);
    Ok(())
}

#[test]
fn test_runtime_keyspace_masks_filter_unsupported_flags() -> Result<()> {
    for (mask, expected) in [("unsupported", 0), ("unsupported,string", 1)] {
        let mut con = TestConnection::with_args("runtime_events", &[mask]);
        redis::cmd("SET")
            .arg(&["key", "value"])
            .query::<()>(&mut con)?;
        let counts: Vec<i64> = redis::cmd("runtime.counts").query(&mut con)?;
        assert_eq!(counts, [expected, 0, 0, 0, 0]);
        assert!(con.server_log()?.contains("aren't supported"));
        assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    }
    Ok(())
}

#[test]
fn test_runtime_keyspace_callback_preserves_bytes_and_database_and_catches_panics() -> Result<()> {
    let mut con = TestConnection::with_args("runtime_events", &["generic,string"]);
    redis::cmd("SELECT").arg(3).query::<()>(&mut con)?;
    let key = b"key\xff";
    redis::cmd("runtime.send")
        .arg(b"event\xff".as_slice())
        .arg(key.as_slice())
        .query::<()>(&mut con)?;
    let (event, saved_key, db): (String, Vec<u8>, i32) =
        redis::cmd("runtime.last").query(&mut con)?;
    assert_eq!(event, "event\u{fffd}");
    assert_eq!(saved_key, key);
    assert_eq!(db, 3);
    // Panic before taking the example's mutex so later deliveries remain usable.
    redis::cmd("SET")
        .arg(&["runtime:panic", "value"])
        .query::<()>(&mut con)?;
    redis::cmd("SET")
        .arg(&["after-panic", "value"])
        .query::<()>(&mut con)?;
    let (event, key, db): (String, String, i32) = redis::cmd("runtime.last").query(&mut con)?;
    assert_eq!(
        (event.as_str(), key.as_str(), db),
        ("set", "after-panic", 3)
    );
    assert!(
        con.server_log()?
            .contains("runtime keyspace handler panicked")
    );
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    Ok(())
}

#[test]
fn test_runtime_keyspace_subscriptions_are_removed_on_unload() -> Result<()> {
    let mut con = TestConnection::with_args("runtime_events", &["string"]);
    redis::cmd("SET")
        .arg(&["before", "value"])
        .query::<()>(&mut con)?;
    redis::cmd("MODULE")
        .arg(&["UNLOAD", "runtime_events"])
        .query::<()>(&mut con)?;
    redis::cmd("SET")
        .arg(&["unloaded", "value"])
        .query::<()>(&mut con)?;
    redis::cmd("MODULE")
        .arg("LOAD")
        .arg(utils::module_path("runtime_events")?)
        .arg("missed")
        .query::<()>(&mut con)?;
    // Some dynamic loaders keep the library's Rust statics across a reload.
    // Assert delivery relative to the baseline, independent of that behavior.
    let mut expected: Vec<i64> = redis::cmd("runtime.counts").query(&mut con)?;
    expected[1] += 1;
    redis::cmd("SET")
        .arg(&["after", "value"])
        .query::<()>(&mut con)?;
    redis::cmd("GET").arg("missing").query::<()>(&mut con)?;
    let counts: Vec<i64> = redis::cmd("runtime.counts").query(&mut con)?;
    assert_eq!(counts, expected);
    Ok(())
}

#[test]
fn test_worker_runtime_call_and_blocked_client_reply() -> Result<()> {
    let mut con = TestConnection::new("runtime_events");
    assert_eq!(
        redis::cmd("runtime.worker").query::<String>(&mut con)?,
        "PONG"
    );
    assert!(
        con.server_log()?
            .contains("worker runtime helper succeeded")
    );
    redis::cmd("SELECT").arg(3).query::<()>(&mut con)?;
    assert_eq!(
        redis::cmd("GET")
            .arg("worker:runtime")
            .query::<String>(&mut con)?,
        "value"
    );
    redis::cmd("SELECT").arg(0).query::<()>(&mut con)?;
    assert_eq!(
        redis::cmd("GET")
            .arg("worker:runtime")
            .query::<Option<String>>(&mut con)?,
        None
    );
    // Finish the test's own worker before unloading its code. A client reply
    // can arrive before the worker has returned through its final epilogue.
    utils::wait_until(Duration::from_secs(2), "runtime worker exit", || {
        Ok(redis::cmd("runtime.finish_worker").query::<i64>(&mut con)? == 1)
    })?;
    redis::cmd("SET")
        .arg(&["before-unload", "value"])
        .query::<()>(&mut con)?;
    let counts: Vec<i64> = redis::cmd("runtime.counts").query(&mut con)?;
    assert_eq!(counts, [0; 5]);
    redis::cmd("MODULE")
        .arg(&["UNLOAD", "runtime_events"])
        .query::<()>(&mut con)?;
    redis::cmd("SET")
        .arg(&["after-unload", "value"])
        .query::<()>(&mut con)?;
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    Ok(())
}

#[test]
fn test_worker_server_info_is_scoped_and_returns_owned_rust_data() -> Result<()> {
    let mut con = TestConnection::new("runtime_events");
    let info: String = redis::cmd("INFO").arg("server").query(&mut con)?;
    let expected = info
        .lines()
        .find_map(|line| line.strip_prefix("redis_version:"))
        .context("native INFO has redis_version")?;
    for mode in ["manual", "auto"] {
        let version: String = redis::cmd("runtime.worker_info")
            .arg(mode)
            .query(&mut con)?;
        assert_eq!(version, expected);
        utils::wait_until(Duration::from_secs(2), "server INFO worker exit", || {
            Ok(redis::cmd("runtime.finish_worker").query::<i64>(&mut con)? == 1)
        })?;
    }
    redis::cmd("MODULE")
        .arg(&["UNLOAD", "runtime_events"])
        .query::<()>(&mut con)?;
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    Ok(())
}

#[test]
fn test_worker_keys_close_before_unlock_and_return_owned_rust_data() -> Result<()> {
    let mut con = TestConnection::new("runtime_events");
    for mode in ["manual", "auto"] {
        let (bytes, flagged_bytes, entries): (Vec<u8>, Vec<u8>, Vec<String>) =
            redis::cmd("runtime.worker_keys")
                .arg(mode)
                .query(&mut con)?;
        assert_eq!(bytes, b"\0\xffvalue!");
        assert_eq!(flagged_bytes, bytes);
        assert_eq!(entries, ["field", "value"]);
        utils::wait_until(Duration::from_secs(2), "key worker exit", || {
            Ok(redis::cmd("runtime.finish_worker").query::<i64>(&mut con)? == 1)
        })?;
        let stored: Vec<u8> = redis::cmd("GET").arg("worker:scoped:key").query(&mut con)?;
        assert_eq!(stored, bytes);
    }
    redis::cmd("MODULE")
        .arg(&["UNLOAD", "runtime_events"])
        .query::<()>(&mut con)?;
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    Ok(())
}

#[test]
fn test_call_replies_stay_in_their_context_with_and_without_auto_memory() -> Result<()> {
    let mut con = TestConnection::new("runtime_events");
    for mode in ["manual", "auto"] {
        let bytes: Vec<u8> = redis::cmd("runtime.call_reply").arg(mode).query(&mut con)?;
        assert_eq!(bytes, b"\0\xffreply", "{mode} command");
        let bytes: Vec<u8> = redis::cmd("runtime.worker_call")
            .arg(mode)
            .query(&mut con)?;
        assert_eq!(bytes, b"\0\xffreply", "{mode} worker");
        utils::wait_until(Duration::from_secs(2), "call reply worker exit", || {
            Ok(redis::cmd("runtime.finish_worker").query::<i64>(&mut con)? == 1)
        })?;
    }
    redis::cmd("MODULE")
        .arg(&["UNLOAD", "runtime_events"])
        .query::<()>(&mut con)?;
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    Ok(())
}

#[test]
fn test_command_builders_mix_with_legacy_and_preserve_metadata() -> Result<()> {
    let mut con = TestConnection::new("command_builder");
    for command in [
        "builder.legacy",
        "builder.ping",
        "builder.legacy_after",
        "builder.optional",
        "builder.legacy_callback",
        "builder.legacy_keyless",
        "builder.callback",
        "builder.invoke",
    ] {
        assert_eq!(redis::cmd(command).query::<String>(&mut con)?, "PONG");
    }
    assert_eq!(
        redis::Cmd::new()
            .arg(&b"builder.binary_\xff"[..])
            .query::<String>(&mut con)?,
        "PONG"
    );
    redis::cmd("SET")
        .arg(&["builder:key", "value"])
        .query::<()>(&mut con)?;
    assert_eq!(
        redis::cmd("builder.get")
            .arg("builder:key")
            .query::<String>(&mut con)?,
        "value"
    );
    let metadata: Vec<Vec<Value>> = redis::cmd("COMMAND")
        .arg(&["INFO", "builder.get", "builder.ping", "builder.legacy"])
        .query(&mut con)?;
    let flags: Vec<String> = redis::from_redis_value_ref(&metadata[0][2])?;
    for flag in ["readonly", "fast", "noscript"] {
        assert!(flags.iter().any(|value| value == flag), "{flags:?}");
    }
    let positions: Vec<i64> = metadata[0][3..6]
        .iter()
        .map(redis::from_redis_value_ref)
        .collect::<Result<_, _>>()?;
    assert_eq!(positions, [1, 1, 1]);
    for command in &metadata[1..] {
        let positions: Vec<i64> = command[3..6]
            .iter()
            .map(redis::from_redis_value_ref)
            .collect::<Result<_, _>>()?;
        assert_eq!(positions, [0, 0, 0]);
    }
    Ok(())
}

#[test]
fn test_command_builder_key_ranges_and_dynamic_discovery() -> Result<()> {
    let mut con = TestConnection::new("command_builder");
    let single: Vec<String> = redis::cmd("COMMAND")
        .arg(&["GETKEYS", "builder.get", "single"])
        .query(&mut con)?;
    assert_eq!(single, ["single"]);
    for command in ["builder.keys", "builder.dynamic"] {
        let keys: Vec<String> = redis::cmd("COMMAND")
            .arg(&["GETKEYS", command, "a", "value-a", "b", "value-b"])
            .query(&mut con)?;
        assert_eq!(keys, ["a", "b"]);
        let reply: Vec<String> = redis::cmd(command)
            .arg(&["a", "value-a", "b", "value-b"])
            .query(&mut con)?;
        assert_eq!(reply, ["a", "b"]);
    }
    Ok(())
}

#[test]
fn test_command_builder_optional_acl_failure_retains_required_access() -> Result<()> {
    let mut con = TestConnection::new("command_builder");
    redis::cmd("ACL")
        .arg(&[
            "SETUSER",
            "builder-reader",
            "on",
            ">secret",
            "~*",
            "-@all",
            "+@read",
        ])
        .query::<()>(&mut con)?;
    let mut reader = con.connection()?;
    redis::cmd("AUTH")
        .arg(&["builder-reader", "secret"])
        .query::<()>(&mut reader)?;
    assert_eq!(
        redis::cmd("builder.acl").query::<String>(&mut reader)?,
        "PONG"
    );
    assert_eq!(
        redis::cmd("builder.legacy_acl").query::<String>(&mut reader)?,
        "PONG"
    );
    assert!(
        con.server_log()?
            .contains("Failed to set optional ACL categories for command builder.acl")
    );
    assert!(
        con.server_log()?
            .contains("Failed to set optional ACL categories for command builder.optional")
    );
    Ok(())
}

#[test]
fn test_command_builder_invalid_definitions_fail_the_load() {
    for (mode, expected) in [
        ("key-zero", "invalid command key range"),
        ("key-negative", "invalid command key range"),
        ("step-zero", "invalid command key range"),
        ("step-negative", "invalid command key range"),
        ("last-before-first", "invalid command key range"),
        ("last-zero", "invalid command key range"),
        ("unknown-flags", "unknown command flag bits"),
        ("required-acl", "failed to set mandatory ACL categories"),
        (
            "optional-empty",
            "optional ACL categories must not be empty",
        ),
        ("nul-acl", "nul byte"),
        ("nul-name", "nul byte"),
        ("empty-name", "command name must not be empty"),
        ("duplicate", "failed to register command builder.ping"),
    ] {
        let error = match TestConnection::try_with_args("command_builder", &[mode]) {
            Ok(_) => panic!("invalid definition {mode} loaded"),
            Err(error) => error,
        };
        let error = format!("{error:#}");
        assert!(error.contains(expected), "{mode}: {error}");
        assert!(!error.contains("panicked"), "{mode}: {error}");
    }
}

#[test]
fn test_command_builder_panics_are_caught() -> Result<()> {
    let mut con = TestConnection::new("command_builder");
    let error = redis::cmd("builder.panic")
        .query::<Value>(&mut con)
        .expect_err("panic was not reported");
    assert!(
        error.to_string().contains("builder command panicked"),
        "{error}"
    );
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    Ok(())
}

#[test]
fn test_call_protocol_resets_to_resp2_without_newer_format_flags() -> Result<()> {
    let mut con = TestConnection::new("call_reply");
    redis::cmd("HSET")
        .arg(&["protocol:hash", "field", "value"])
        .query::<()>(&mut con)?;
    // Redis 6.0 rejects RESP3/Auto call flags. Resetting to RESP2 must remove
    // them before the native call, so these also run against 6.0 unchanged.
    for first in ["2", "3", "auto"] {
        let actual: String = redis::cmd("call_reply.protocol")
            .arg(&["protocol:hash", first, "2"])
            .query(&mut con)?;
        assert_eq!(actual, "array", "{first} -> 2");
    }
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    Ok(())
}

#[test]
fn test_call_protocol_selection_uses_the_last_setting() -> Result<()> {
    let mut con = TestConnection::new("call_reply");
    con.set_read_timeout(Some(Duration::from_secs(5)))?;
    con.set_write_timeout(Some(Duration::from_secs(5)))?;
    redis::cmd("HSET")
        .arg(&["protocol:hash", "field", "value"])
        .query::<()>(&mut con)?;

    for client_resp3 in [false, true] {
        if client_resp3 {
            let hello: Value = redis::cmd("HELLO").arg(3).query(&mut con)?;
            assert!(matches!(hello, Value::Map(_)), "{hello:?}");
        }
        let default: String = redis::cmd("call_reply.protocol")
            .arg("protocol:hash")
            .query(&mut con)?;
        assert_eq!(default, "array", "the default is RESP2 for either client");
        for first in ["2", "3", "auto"] {
            for last in ["2", "3", "auto"] {
                let actual: String = redis::cmd("call_reply.protocol")
                    .arg(&["protocol:hash", first, last])
                    .query(&mut con)?;
                let expected = if last == "3" || (last == "auto" && client_resp3) {
                    "map"
                } else {
                    "array"
                };
                assert_eq!(actual, expected, "{first} -> {last}, RESP3={client_resp3}");
            }
        }
        assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    }
    Ok(())
}

#[test]
fn test_call_error_codes_classify_native_and_synthesized_errors() -> Result<()> {
    let mut con = TestConnection::new("call_reply");
    redis::cmd("SET")
        .arg(&["string-key", "value"])
        .query::<()>(&mut con)?;
    redis::cmd("XGROUP")
        .arg(&["CREATE", "stream", "group", "0", "MKSTREAM"])
        .query::<()>(&mut con)?;
    for (args, expected) in [
        (&["HGET", "string-key", "field"][..], &b"WRONGTYPE"[..]),
        (
            &["XGROUP", "CREATE", "stream", "group", "0"][..],
            &b"BUSYGROUP"[..],
        ),
        (&["NOSUCHCMD"][..], &b"ERR"[..]),
    ] {
        let code: Vec<u8> = redis::cmd("call_reply.error_code")
            .arg(args)
            .query(&mut con)?;
        assert_eq!(code, expected, "{args:?}");
    }
    let code: Option<Vec<u8>> = redis::cmd("call_reply.error_code")
        .arg(&["PING"])
        .query(&mut con)?;
    assert_eq!(code, None, "a successful call had an error code");
    Ok(())
}

#[test]
fn test_call_error_codes_do_not_require_utf8_messages() -> Result<()> {
    let mut con = TestConnection::new("call_reply");
    for (script, expected) in [
        (
            "return redis.error_reply('CUSTOM detail ' .. string.char(255))",
            &b"CUSTOM"[..],
        ),
        (
            "return redis.error_reply(string.char(255) .. ' details')",
            &b"\xff"[..],
        ),
    ] {
        let code: Vec<u8> = redis::cmd("call_reply.error_code")
            .arg(&["EVAL", script, "0"])
            .query(&mut con)?;
        assert_eq!(code, expected);
    }
    Ok(())
}

#[test]
fn test_byte_keyspace_notifications_accept_non_utf8_and_empty_names() -> Result<()> {
    let mut con = TestConnection::new("events");
    for (event, expected) in [
        (&b"ordinary"[..], "ordinary"),
        (&b"ev\xffnt"[..], "ev\u{FFFD}nt"),
        (&b""[..], ""),
    ] {
        let sent: String = redis::cmd("events.send_bytes").arg(event).query(&mut con)?;
        assert_eq!(sent, "Event sent");
        let seen: String = redis::cmd("events.last_generic_event").query(&mut con)?;
        assert_eq!(seen, expected);
    }
    Ok(())
}

#[test]
fn test_keyspace_notifications_reject_nul_without_firing_or_panicking() -> Result<()> {
    let mut con = TestConnection::new("events");
    // The legacy text helper still sends valid names.
    redis::cmd("events.send_text")
        .arg("previous")
        .query::<String>(&mut con)?;
    for (command, expected) in [
        ("events.send_bytes", "contains a NUL byte"),
        ("events.send_text", "text notification rejected"),
    ] {
        let error = redis::cmd(command)
            .arg(&b"before\0after"[..])
            .query::<Value>(&mut con)
            .expect_err("a NUL-containing event was accepted");
        assert!(error.to_string().contains(expected), "{command}: {error}");
        assert!(!error.to_string().contains("panicked"), "{error}");
        let seen: String = redis::cmd("events.last_generic_event").query(&mut con)?;
        assert_eq!(
            seen, "previous",
            "the rejected event was fired or truncated"
        );
        assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    }
    Ok(())
}

#[test]
fn test_database_selection_changes_the_client_database() -> Result<()> {
    let mut con = TestConnection::new("database");
    assert_eq!(redis::cmd("database.selected").query::<i32>(&mut con)?, 0);
    assert_eq!(
        redis::cmd("database.select")
            .arg(2)
            .query::<i32>(&mut con)?,
        2
    );
    redis::cmd("SET")
        .arg(&["same-key", "db-two"])
        .query::<()>(&mut con)?;
    redis::cmd("SELECT").arg(0).query::<()>(&mut con)?;
    assert_eq!(redis::cmd("database.selected").query::<i32>(&mut con)?, 0);
    assert_eq!(
        redis::cmd("GET")
            .arg("same-key")
            .query::<Option<String>>(&mut con)?,
        None
    );
    assert_eq!(
        redis::cmd("database.select")
            .arg(2)
            .query::<i32>(&mut con)?,
        2
    );
    assert_eq!(
        redis::cmd("GET")
            .arg("same-key")
            .query::<String>(&mut con)?,
        "db-two"
    );
    Ok(())
}

#[test]
fn test_database_scoped_reads_and_nested_restoration() -> Result<()> {
    let mut con = TestConnection::new("database");
    redis::cmd("SELECT").arg(2).query::<()>(&mut con)?;
    redis::cmd("SET")
        .arg(&["same-key", "db-two"])
        .query::<()>(&mut con)?;
    redis::cmd("SELECT").arg(3).query::<()>(&mut con)?;
    redis::cmd("SET")
        .arg(&["same-key", "db-three"])
        .query::<()>(&mut con)?;
    assert_eq!(
        redis::cmd("database.get_in")
            .arg(&["2", "same-key"])
            .query::<String>(&mut con)?,
        "db-two"
    );
    assert_eq!(redis::cmd("database.selected").query::<i32>(&mut con)?, 3);
    assert_eq!(
        redis::cmd("GET")
            .arg("same-key")
            .query::<String>(&mut con)?,
        "db-three"
    );
    assert_eq!(
        redis::cmd("database.nested")
            .arg(&[4, 5])
            .query::<Vec<i32>>(&mut con)?,
        [3, 4, 5, 4, 3]
    );
    // Selecting the same database must also preserve nested restoration.
    assert_eq!(
        redis::cmd("database.nested")
            .arg(&[3, 3])
            .query::<Vec<i32>>(&mut con)?,
        [3, 3, 3, 3, 3]
    );
    Ok(())
}

#[test]
fn test_database_invalid_selection_preserves_the_client_database() -> Result<()> {
    let mut con = TestConnection::new("database");
    redis::cmd("SELECT").arg(3).query::<()>(&mut con)?;
    let config: HashMap<String, i32> = redis::cmd("CONFIG")
        .arg(&["GET", "databases"])
        .query(&mut con)?;
    for db in [-1, config["databases"], i32::MAX] {
        for command in [
            "database.select",
            "database.scoped_error",
            "database.scoped_panic",
        ] {
            let error = redis::cmd(command)
                .arg(db)
                .query::<Value>(&mut con)
                .expect_err("invalid database was accepted");
            assert!(
                error.to_string().contains("DB index is out of range"),
                "{command}: {error}"
            );
            assert!(
                !error.to_string().contains("scoped database"),
                "invalid selection ran its closure: {error}"
            );
            assert_eq!(redis::cmd("database.selected").query::<i32>(&mut con)?, 3);
        }
        // A failed inner selection must still restore the successful outer scope.
        let error = redis::cmd("database.nested")
            .arg(&[4, db])
            .query::<Value>(&mut con)
            .expect_err("invalid inner database was accepted");
        assert!(
            error.to_string().contains("DB index is out of range"),
            "{error}"
        );
        assert_eq!(redis::cmd("database.selected").query::<i32>(&mut con)?, 3);
    }
    Ok(())
}

#[test]
fn test_database_scopes_restore_after_errors_and_panics() -> Result<()> {
    let mut con = TestConnection::new("database");
    redis::cmd("SELECT").arg(3).query::<()>(&mut con)?;
    for (command, message) in [
        ("database.scoped_error", "scoped database error"),
        ("database.scoped_panic", "scoped database panic"),
    ] {
        let error = redis::cmd(command)
            .arg(4)
            .query::<Value>(&mut con)
            .expect_err("scope unexpectedly succeeded");
        assert!(error.to_string().contains(message), "{command}: {error}");
        assert_eq!(redis::cmd("database.selected").query::<i32>(&mut con)?, 3);
        assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    }
    Ok(())
}

#[test]
fn test_harness_starts_with_module_arguments() -> Result<()> {
    let mut con = TestConnection::try_with_args(
        "configuration",
        &[
            "i64",
            "42",
            "bool",
            "YES",
            "memory",
            "2kb",
            "string",
            "two words",
        ],
    )?;
    let configs: HashMap<String, String> = redis::cmd("CONFIG")
        .arg(&["GET", "configuration.*"])
        .query(&mut con)?;
    for (name, expected) in [
        ("i64", "42"),
        ("bool", "yes"),
        ("memory", "2048"),
        ("string", "two words"),
    ] {
        assert_eq!(configs[&format!("configuration.{name}")], expected);
    }
    assert!(con.server_log()?.contains("configuration init: i64 is 42"));
    Ok(())
}

#[test]
fn test_harness_failed_startup_includes_module_diagnostic() {
    let error = match TestConnection::try_with_args("configuration", &["bool", "invalid"]) {
        Ok(_) => panic!("a module with an invalid configuration started"),
        Err(error) => error,
    };
    let diagnostic = format!("{error:#}");
    assert!(diagnostic.contains("Server log ("), "{diagnostic}");
    assert!(
        diagnostic.contains("Boolean configuration 'bool' must be 'yes' or 'no'"),
        "{diagnostic}"
    );
    assert!(
        !diagnostic.contains("configuration init:"),
        "init ran for a failed load: {diagnostic}"
    );
}

#[test]
fn test_harness_replica_receives_initial_and_later_writes() -> Result<()> {
    let mut primary = TestConnection::new("hello");
    redis::cmd("SET")
        .arg(&["before-replica", "initial value"])
        .query::<()>(&mut primary)?;
    let mut replica = primary.replica()?;
    assert_ne!(primary.port(), replica.port());
    assert_eq!(
        redis::cmd("GET")
            .arg("before-replica")
            .query::<String>(&mut replica)?,
        "initial value"
    );
    assert_eq!(
        redis::cmd("hello.mul")
            .arg(&[2, 3])
            .query::<Vec<i64>>(&mut replica)?,
        [2, 3, 6]
    );

    redis::cmd("SET")
        .arg(&["after-replica", "later value"])
        .query::<()>(&mut primary)?;
    let acknowledged: usize = redis::cmd("WAIT").arg(&[1, 5000]).query(&mut primary)?;
    assert_eq!(acknowledged, 1, "replica log: {}", replica.server_log()?);
    assert_eq!(
        redis::cmd("GET")
            .arg("after-replica")
            .query::<String>(&mut replica)?,
        "later value"
    );
    Ok(())
}

#[test]
fn test_harness_restart_preserves_data_and_module_arguments() -> Result<()> {
    let mut con = TestConnection::with_args(
        "configuration",
        &[
            "i64",
            "37",
            "string",
            "restart configuration",
            "bool",
            "YES",
        ],
    );
    let port = con.port();
    let directory: Vec<String> = redis::cmd("CONFIG").arg(&["GET", "dir"]).query(&mut con)?;
    redis::cmd("SET")
        .arg(&["restart-data", "saved value"])
        .query::<()>(&mut con)?;

    for expected_count in 1..=2 {
        redis::cmd("INCR")
            .arg("restart-count")
            .query::<i64>(&mut con)?;
        con = con.restart()?;
        assert_eq!(con.port(), port);
        assert_eq!(
            redis::cmd("CONFIG")
                .arg(&["GET", "dir"])
                .query::<Vec<String>>(&mut con)?,
            directory
        );
        assert_eq!(
            redis::cmd("GET")
                .arg("restart-count")
                .query::<i64>(&mut con)?,
            expected_count
        );
        assert_eq!(
            redis::cmd("GET")
                .arg("restart-data")
                .query::<String>(&mut con)?,
            "saved value"
        );
        let configs: HashMap<String, String> = redis::cmd("CONFIG")
            .arg(&["GET", "configuration.*"])
            .query(&mut con)?;
        assert_eq!(configs["configuration.i64"], "37");
        assert_eq!(configs["configuration.string"], "restart configuration");
        assert_eq!(configs["configuration.bool"], "yes");
        assert!(
            redis::cmd("configuration.num_changes").query::<i64>(&mut con)? > 0,
            "the module's command or configuration initialization was lost"
        );
    }
    assert_eq!(
        con.server_log()?
            .matches("configuration init: i64 is 37")
            .count(),
        3,
        "the original log and both restart logs must be retained"
    );
    Ok(())
}

#[test]
fn test_harness_wait_until_reports_timeout() {
    let mut attempts = 0;
    let error = utils::wait_until(
        Duration::from_millis(20),
        "condition that stays false",
        || {
            attempts += 1;
            Ok(false)
        },
    )
    .expect_err("an unsatisfied condition succeeded");
    assert!(attempts > 0);
    assert!(
        error.to_string().contains("condition that stays false"),
        "{error}"
    );
}

#[test]
fn test_harness_wait_until_preserves_predicate_error() {
    let mut attempts = 0;
    let error = utils::wait_until(Duration::from_secs(1), "condition with an error", || {
        attempts += 1;
        Err(anyhow::anyhow!("predicate failed"))
    })
    .expect_err("a failed predicate succeeded");
    assert_eq!(attempts, 1, "predicate errors must not be retried");
    assert_eq!(error.to_string(), "predicate failed");
}

#[test]
fn test_harness_pipeline_error_drains_remaining_replies() -> Result<()> {
    let mut con = TestConnection::new("hello");
    redis::cmd("SET")
        .arg(&["pipeline-string", "value"])
        .query::<()>(&mut con)?;
    let error = redis::pipe()
        .cmd("LPUSH")
        .arg(&["pipeline-string", "item"])
        .cmd("PING")
        .query::<Vec<Value>>(&mut *con)
        .expect_err("LPUSH accepted a string key");
    assert_eq!(error.code(), Some("WRONGTYPE"), "{error}");
    // Errors in offset replies (for example transaction setup) must not be
    // discarded before the harness drains the remaining requested responses.
    let mut commands = redis::cmd("LPUSH")
        .arg(&["pipeline-string", "item"])
        .get_packed_command();
    commands.extend(redis::cmd("PING").get_packed_command());
    let error = redis::ConnectionLike::req_packed_commands(&mut *con, &commands, 1, 1)
        .expect_err("an error in a skipped pipeline reply was ignored");
    assert_eq!(error.code(), Some("WRONGTYPE"), "{error}");
    assert_eq!(
        redis::cmd("ECHO")
            .arg("after the pipeline")
            .query::<String>(&mut con)?,
        "after the pipeline",
        "the failed pipeline left a PING reply on the connection"
    );
    Ok(())
}

#[test]
fn test_harness_nested_error_preserves_reply_alignment() -> Result<()> {
    let mut con = TestConnection::new("hello");
    let command = redis::cmd("EVAL")
        .arg("return {'before', redis.error_reply('PROBE nested failure'), 'after'}")
        .arg(0)
        .get_packed_command();
    let error = redis::ConnectionLike::req_packed_command(&mut *con, &command)
        .expect_err("a nested server error was treated as a successful reply");
    assert_eq!(error.code(), Some("PROBE"));
    assert_eq!(error.detail(), Some("nested failure"));
    assert_eq!(
        redis::cmd("ECHO")
            .arg("aligned")
            .query::<String>(&mut con)?,
        "aligned"
    );
    Ok(())
}

#[test]
fn test_harness_resp2_and_resp3_decoding() -> Result<()> {
    let mut con = TestConnection::new("hello");
    for protocol in [2, 3, 2] {
        let hello: Value = redis::cmd("HELLO").arg(protocol).query(&mut con)?;
        let resp3 = protocol == 3;
        assert_eq!(matches!(hello, Value::Map(_)), resp3, "{hello:?}");
        assert_eq!(matches!(hello, Value::Array(_)), !resp3, "{hello:?}");
        let info: HashMap<String, Value> = redis::from_redis_value(hello)?;
        assert_eq!(info["proto"], Value::Int(protocol));
        assert_eq!(
            redis::cmd("SET")
                .arg(&["resp:string", "value"])
                .query::<Value>(&mut con)?,
            Value::Okay
        );
        let bytes = b"a\0\xff\r\n";
        let echoed: Value = redis::cmd("ECHO").arg(bytes.as_slice()).query(&mut con)?;
        assert_eq!(echoed, Value::BulkString(bytes.to_vec()));
        redis::cmd("HSET")
            .arg(&["resp:hash", "field", "value"])
            .query::<()>(&mut con)?;
        let hash: Value = redis::cmd("HGETALL").arg("resp:hash").query(&mut con)?;
        let pair = (
            Value::BulkString(b"field".to_vec()),
            Value::BulkString(b"value".to_vec()),
        );
        assert_eq!(
            hash,
            if resp3 {
                Value::Map(vec![pair])
            } else {
                Value::Array(vec![pair.0, pair.1])
            }
        );
        assert_eq!(
            redis::cmd("GET")
                .arg("resp:missing")
                .query::<Value>(&mut con)?,
            Value::Nil
        );
        let error = redis::cmd("LPUSH")
            .arg(&["resp:hash", "wrong type"])
            .query::<Value>(&mut con)
            .unwrap_err();
        assert_eq!(error.code(), Some("WRONGTYPE"));
        assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    }
    Ok(())
}

#[test]
fn test_string_ownership_additional_encodings() -> Result<()> {
    let mut con = TestConnection::new("string_ownership");
    redis::cmd("CONFIG")
        .arg("SET")
        .arg("set-max-listpack-entries")
        .arg(128)
        .query::<String>(&mut con)?;
    redis::cmd("SADD")
        .arg("text-set")
        .arg(&["a", "b"])
        .query::<i64>(&mut con)?;
    let encoding: String = redis::cmd("OBJECT")
        .arg("ENCODING")
        .arg("text-set")
        .query(&mut con)?;
    assert_eq!(encoding, "listpack");
    for mode in [0, 1] {
        let response: Value = redis::cmd("owner.scan")
            .arg(mode)
            .arg("text-set")
            .arg(0)
            .query(&mut con)?;
        let Value::Array(top) = response else {
            panic!("scan array")
        };
        assert_eq!(top[0], Value::Int(2));
        let Value::Array(pairs) = &top[1] else {
            panic!("pair array")
        };
        for row in pairs {
            let Value::Array(pair) = row else {
                panic!("pair")
            };
            assert_eq!(pair[1], Value::Nil);
        }
    }
    let info: String = redis::cmd("INFO").arg("server").query(&mut con)?;
    if info.lines().any(|line| line == "redis_version:8.10.2") {
        redis::cmd("HSET")
            .arg("expires")
            .arg(&["f", "value"])
            .query::<i64>(&mut con)?;
        redis::cmd("HEXPIRE")
            .arg("expires")
            .arg(3600)
            .arg("FIELDS")
            .arg(1)
            .arg("f")
            .query::<Value>(&mut con)?;
        let encoding: String = redis::cmd("OBJECT")
            .arg("ENCODING")
            .arg("expires")
            .query(&mut con)?;
        assert_eq!(encoding, "listpackex");
        for mode in [0, 1] {
            let response: Value = redis::cmd("owner.scan")
                .arg(mode)
                .arg("expires")
                .arg(0)
                .query(&mut con)?;
            let Value::Array(top) = response else {
                panic!("scan array")
            };
            assert_eq!(top[0], Value::Int(1));
            assert_eq!(
                top[1],
                Value::Array(vec![Value::Array(vec![
                    Value::BulkString(b"f".to_vec()),
                    Value::BulkString(b"value".to_vec())
                ])])
            );
        }
    }
    Ok(())
}

#[test]
fn test_string_ownership_rdb_and_transfer() -> Result<()> {
    let mut con = TestConnection::new("string_ownership");
    for bytes in [vec![], b"a\0\xff".to_vec(), (0..=255).collect()] {
        let length: usize = redis::cmd("owner.transfer").arg(&bytes).query(&mut con)?;
        assert_eq!(length, bytes.len());
        assert_eq!(
            redis::cmd("GET")
                .arg("owner:transfer")
                .query::<Vec<u8>>(&mut con)?,
            bytes
        );
        redis::cmd("DEL")
            .arg(&["source", "restored"])
            .query::<i64>(&mut con)?;
        redis::cmd("owner.rdbset")
            .arg("source")
            .arg(&bytes)
            .query::<String>(&mut con)?;
        let encoded: Vec<u8> = redis::cmd("DUMP").arg("source").query(&mut con)?;
        redis::cmd("RESTORE")
            .arg("restored")
            .arg(0)
            .arg(encoded)
            .query::<String>(&mut con)?;
        assert_eq!(
            redis::cmd("owner.rdbget")
                .arg("restored")
                .query::<Vec<u8>>(&mut con)?,
            bytes
        );
        redis::cmd("DEL")
            .arg(&["source", "restored"])
            .query::<i64>(&mut con)?;
    }
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    Ok(())
}

#[test]
fn test_string_ownership_lifecycle() -> Result<()> {
    let mut con = TestConnection::new("string_ownership");
    for mode in [0, 1] {
        let calls: Vec<i64> = redis::cmd("owner.lifecycle").arg(mode).query(&mut con)?;
        assert_eq!(calls, [3, 1, 2, 5, 1]);
        // API call counts: create, deep-copy, retain, null-context free, producer free.
        let version: String = redis::cmd("INFO").arg("server").query(&mut con)?;
        let version = version
            .lines()
            .find_map(|s| s.strip_prefix("redis_version:"))
            .unwrap()
            .as_bytes()
            .to_vec();
        let binary = (0..=255).cycle().take(8192).collect::<Vec<u8>>();
        for (origin, key, expected) in [
            ("argv", binary.clone(), vec![binary]),
            (
                "create",
                b"unused".to_vec(),
                vec![b"context-created".to_vec()],
            ),
            ("owned", b"unused".to_vec(), vec![b"owned".to_vec()]),
            ("info", b"unused".to_vec(), vec![version]),
            ("user", b"unused".to_vec(), vec![b"default".to_vec()]),
            ("hash", b"h".to_vec(), vec![b"hv".to_vec(); 3]),
            ("list", b"l".to_vec(), vec![b"a".to_vec(), b"b".to_vec()]),
            ("stream", b"s".to_vec(), vec![b"f".to_vec(), b"sv".to_vec()]),
        ] {
            redis::cmd("DEL")
                .arg(&["h", "l", "s"])
                .query::<i64>(&mut con)?;
            redis::cmd("HSET")
                .arg("h")
                .arg("f")
                .arg("hv")
                .query::<i64>(&mut con)?;
            redis::cmd("RPUSH")
                .arg("l")
                .arg(&["a", "b"])
                .query::<i64>(&mut con)?;
            redis::cmd("XADD")
                .arg("s")
                .arg("1-0")
                .arg("f")
                .arg("sv")
                .query::<String>(&mut con)?;
            let count: usize = redis::cmd("owner.store")
                .arg(origin)
                .arg(mode)
                .arg(&key)
                .query(&mut con)?;
            assert_eq!(count, expected.len());
            // This is a separate later command. No old buffer pointer crosses phases.
            let values: Vec<Vec<u8>> = redis::cmd("owner.read").query(&mut con)?;
            assert_eq!(values, expected, "{origin}, AM={mode}");
            assert_eq!(redis::cmd("owner.clear").query::<String>(&mut con)?, "OK");
            assert!(
                redis::cmd("owner.read")
                    .query::<Vec<Vec<u8>>>(&mut con)?
                    .is_empty()
            );
        }
    }
    assert!(
        redis::cmd("owner.lifecycle")
            .arg(2)
            .query::<Value>(&mut con)
            .is_err()
    );
    assert!(
        redis::cmd("owner.lifecycle")
            .query::<Value>(&mut con)
            .is_err()
    );
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    Ok(())
}

#[test]
fn test_string_ownership_scans() -> Result<()> {
    let mut con = TestConnection::new("string_ownership");
    for mode in [0, 1] {
        for (kind, compact) in [
            ("hash", true),
            ("hash", false),
            ("zset", true),
            ("zset", false),
            ("set", true),
            ("set", false),
        ] {
            redis::cmd("DEL").arg("entries").query::<i64>(&mut con)?;
            let limit = if compact { 512 } else { 0 };
            if kind == "hash" {
                redis::cmd("CONFIG")
                    .arg("SET")
                    .arg("hash-max-listpack-entries")
                    .arg(limit)
                    .query::<String>(&mut con)?;
            }
            if kind == "zset" {
                redis::cmd("CONFIG")
                    .arg("SET")
                    .arg("zset-max-listpack-entries")
                    .arg(limit)
                    .query::<String>(&mut con)?;
            }
            if kind == "set" && !compact {
                redis::cmd("CONFIG")
                    .arg("SET")
                    .arg("set-max-intset-entries")
                    .arg(0)
                    .query::<String>(&mut con)?;
                redis::cmd("CONFIG")
                    .arg("SET")
                    .arg("set-max-listpack-entries")
                    .arg(0)
                    .query::<String>(&mut con)?;
            }
            if kind == "set" && compact {
                redis::cmd("CONFIG")
                    .arg("SET")
                    .arg("set-max-intset-entries")
                    .arg(512)
                    .query::<String>(&mut con)?;
            }
            match kind {
                "hash" => {
                    redis::cmd("HSET")
                        .arg("entries")
                        .arg(&["1", "one", "2", "two"])
                        .query::<i64>(&mut con)?;
                }
                "zset" => {
                    redis::cmd("ZADD")
                        .arg("entries")
                        .arg(&["1", "1", "2", "2"])
                        .query::<i64>(&mut con)?;
                }
                _ => {
                    redis::cmd("SADD")
                        .arg("entries")
                        .arg(&["1", "2"])
                        .query::<i64>(&mut con)?;
                }
            }
            let encoding: String = redis::cmd("OBJECT")
                .arg("ENCODING")
                .arg("entries")
                .query(&mut con)?;
            let expected_encoding = match (kind, compact) {
                ("set", true) => "intset",
                ("set", false) | ("hash", false) => "hashtable",
                ("zset", false) => "skiplist",
                _ => "listpack",
            };
            assert_eq!(encoding, expected_encoding);
            let response: Value = redis::cmd("owner.scan")
                .arg(mode)
                .arg("entries")
                .arg(0)
                .query(&mut con)?;
            let Value::Array(top) = response else {
                panic!("scan response must be an array")
            };
            let count: usize = redis::from_redis_value_ref(&top[0])?;
            let Value::Array(rows) = &top[1] else {
                panic!("scan pairs must be an array")
            };
            let mut pairs = Vec::new();
            for row in rows {
                let Value::Array(pair) = row else {
                    panic!("pair must be an array")
                };
                assert_eq!(pair.len(), 2);
                let field: Vec<u8> = redis::from_redis_value_ref(&pair[0])?;
                let value: Option<Vec<u8>> = if pair[1] == Value::Nil {
                    None
                } else {
                    Some(redis::from_redis_value_ref(&pair[1])?)
                };
                pairs.push((field, value));
            }
            pairs.sort();
            assert_eq!(count, 2);
            let expected = match kind {
                "hash" => vec![
                    (b"1".to_vec(), Some(b"one".to_vec())),
                    (b"2".to_vec(), Some(b"two".to_vec())),
                ],
                "zset" => vec![
                    (b"1".to_vec(), Some(b"1".to_vec())),
                    (b"2".to_vec(), Some(b"2".to_vec())),
                ],
                _ => vec![(b"1".to_vec(), None), (b"2".to_vec(), None)],
            };
            assert_eq!(pairs, expected);
            redis::cmd("owner.scan")
                .arg(mode)
                .arg("entries")
                .arg(1)
                .query::<Value>(&mut con)?;
            // A new completed scan verifies stopped-cursor cleanup/recovery.
            let response: Value = redis::cmd("owner.scan")
                .arg(mode)
                .arg("entries")
                .arg(0)
                .query(&mut con)?;
            let Value::Array(top) = response else {
                panic!("scan response must be an array")
            };
            let again: usize = redis::from_redis_value_ref(&top[0])?;
            assert_eq!(again, 2);
        }
    }
    let (count, keys): (usize, Vec<String>) = redis::cmd("owner.keys").query(&mut con)?;
    assert_eq!(count, keys.len());
    assert!(keys.contains(&"entries".to_owned()));
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    Ok(())
}

fn used_memory(con: &mut TestConnection) -> Result<i64> {
    let info: String = redis::cmd("INFO").arg("memory").query(con)?;
    info.lines()
        .find_map(|line| line.strip_prefix("used_memory:"))
        .context("INFO memory has no used_memory")?
        .trim()
        .parse()
        .context("used_memory is not an integer")
}

#[test]
fn test_string_ownership_does_not_leak() -> Result<()> {
    let mut con = TestConnection::new("string_ownership");
    redis::cmd("DEBUG")
        .arg("POPULATE")
        .arg(10_000)
        .query::<()>(&mut con)?;
    // Every leaked string holds at least a 16-byte object header, and each run
    // produces 100k or more strings, so one leaked string per call exceeds this.
    const SLACK: i64 = 256 * 1024;
    for (producer, iterations, produced) in [
        ("keys", 20, 200_000),
        ("list", 100_000, 100_000),
        ("info", 100_000, 100_000),
    ] {
        for mode in [0, 1] {
            let churn = redis::cmd("owner.churn")
                .arg(producer)
                .arg(mode)
                .arg(iterations)
                .clone();
            // The first run settles allocations that are made once.
            assert_eq!(churn.query::<i64>(&mut con)?, produced);
            let before = used_memory(&mut con)?;
            assert_eq!(churn.query::<i64>(&mut con)?, produced);
            let growth = used_memory(&mut con)? - before;
            assert!(
                growth < SLACK,
                "{producer}, AM={mode}: used_memory grew by {growth} bytes over {produced} strings"
            );
        }
    }
    Ok(())
}

#[test]
fn test_string_view_contracts() -> Result<()> {
    let mut con = TestConnection::new("string_views");
    for text in ["", "café 日本語", "a\0b"] {
        let reply: (Vec<u8>, i64) = redis::cmd("views.text").arg(text).query(&mut con)?;
        assert_eq!(reply, (text.as_bytes().to_vec(), 1));
    }
    // A nonempty key first makes the later empty-key notification observable.
    for bytes in [b"a\0\xff\r\n".to_vec(), vec![], (0..=255).collect()] {
        let reply: (Vec<u8>, i64) = redis::cmd("views.bytes").arg(&bytes).query(&mut con)?;
        assert_eq!(reply, (bytes.clone(), 1));
        let notified: Vec<u8> = redis::cmd("views.notify").arg(&bytes).query(&mut con)?;
        assert_eq!(notified, bytes);
    }
    let error = redis::cmd("views.text")
        .arg(b"\xff".as_slice())
        .query::<Value>(&mut con)
        .unwrap_err();
    assert!(
        error
            .detail()
            .unwrap_or_default()
            .contains("parse as UTF-8 string")
    );
    for command in ["views.text", "views.bytes", "views.notify"] {
        for args in [vec![], vec!["one", "extra"]] {
            let error = redis::cmd(command)
                .arg(args)
                .query::<Value>(&mut con)
                .unwrap_err();
            assert!(
                error.to_string().contains("wrong number of arguments"),
                "{error}"
            );
        }
    }
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    Ok(())
}

#[test]
fn test_info_string_view_migration() -> Result<()> {
    let mut con = TestConnection::new("info");
    let info: String = redis::cmd("INFO").arg("server").query(&mut con)?;
    let expected = info
        .lines()
        .find_map(|line| line.strip_prefix("redis_version:"))
        .context("native INFO includes redis_version")?;
    for args in [
        vec!["server", "redis_version"],
        vec!["server", "redis_version", "extra"],
    ] {
        let reply: String = redis::cmd("infoex").arg(args).query(&mut con)?;
        assert_eq!(reply, expected);
    }
    let absent: Option<String> = redis::cmd("infoex")
        .arg(&["server", "field_that_does_not_exist"][..])
        .query(&mut con)?;
    assert_eq!(absent, None);
    // A NUL byte in either name names nothing, as an unknown name does.
    for args in [["server", "redis\0version"], ["ser\0ver", "redis_version"]] {
        let absent: Option<String> = redis::cmd("infoex").arg(&args[..]).query(&mut con)?;
        assert_eq!(absent, None, "{args:?}");
    }
    let error = redis::cmd("infoex")
        .arg("server")
        .query::<Value>(&mut con)
        .unwrap_err();
    assert!(error.to_string().contains("wrong number of arguments"));
    let error = redis::cmd("infoex")
        .arg(b"\xff".as_slice())
        .arg("redis_version")
        .query::<Value>(&mut con)
        .unwrap_err();
    assert!(
        error
            .detail()
            .unwrap_or_default()
            .contains("parse as UTF-8 string")
    );
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    Ok(())
}

#[test]
fn test_static_data_string_view_migration() -> Result<()> {
    let mut con = TestConnection::new("threads");
    for text in ["", "café 日本語", "a\0b"] {
        let reply: String = redis::cmd("set_static_data").arg(text).query(&mut con)?;
        assert_eq!(reply, "OK");
        assert_eq!(
            redis::cmd("get_static_data").query::<String>(&mut con)?,
            text
        );
    }
    let error = redis::cmd("set_static_data")
        .arg(b"\xff".as_slice())
        .query::<Value>(&mut con)
        .unwrap_err();
    assert!(
        error
            .detail()
            .unwrap_or_default()
            .contains("parse as UTF-8 string")
    );
    assert_eq!(
        redis::cmd("get_static_data").query::<String>(&mut con)?,
        "a\0b"
    );
    assert!(
        redis::cmd("set_static_data")
            .query::<Value>(&mut con)
            .is_err()
    );
    let reply: String = redis::cmd("set_static_data")
        .arg(&["valid", "extra"][..])
        .query(&mut con)?;
    assert_eq!(reply, "OK");
    assert_eq!(
        redis::cmd("get_static_data").query::<String>(&mut con)?,
        "valid"
    );
    Ok(())
}

#[test]
fn test_borrowed_arguments() -> Result<()> {
    let mut con = TestConnection::new("args");
    for text in ["", "café 日本語", "text\0with NUL"] {
        let bytes = b"binary\0\xff";
        let reply: (i64, i64, i64, i64, i64, Vec<u8>) = redis::cmd("args.inspect")
            .arg(b"key\0\xff".as_slice())
            .arg(text)
            .arg(bytes.as_slice())
            .query(&mut con)?;
        assert_eq!(reply, (3, 1, 1, 1, 1, bytes.to_vec()));
    }
    let tail: Vec<i64> = redis::cmd("args.tail")
        .arg("first")
        .arg("second")
        .arg("third")
        .query(&mut con)?;
    assert_eq!(tail, [5, 1, 2]); // done() reports extras without consuming them.
    let tail: Vec<i64> = redis::cmd("args.tail").arg("first").query(&mut con)?;
    assert_eq!(tail, [5, 0, 0]);

    for signed in [i64::MIN, 0, i64::MAX] {
        for unsigned in [0, i64::MAX] {
            let reply: (i64, i64, f64) = redis::cmd("args.numbers")
                .arg(signed)
                .arg(unsigned)
                .arg("-1.25")
                .query(&mut con)?;
            assert_eq!(reply, (signed, unsigned, -1.25));
        }
    }
    for (signed, unsigned, float, message) in [
        ("9223372036854775808", "0", "1", "parse as integer"),
        ("0", "-1", "1", "parse negative number"),
        ("0", "18446744073709551615", "1", "parse as integer"),
        ("0", "0", "word", "parse as float"),
    ] {
        let error = redis::cmd("args.numbers")
            .arg(signed)
            .arg(unsigned)
            .arg(float)
            .query::<Value>(&mut con)
            .unwrap_err();
        assert!(
            error.detail().unwrap_or_default().contains(message),
            "{error}"
        );
    }
    for command in ["args.borrowed", "args.inspect", "args.numbers", "args.tail"] {
        let error = redis::cmd(command).query::<Value>(&mut con).unwrap_err();
        assert!(
            error.to_string().contains("wrong number of arguments"),
            "{error}"
        );
    }
    for values in [
        vec![b"expression".as_slice()],
        vec![b"expression", b"COUNT", b"1", b"extra"],
    ] {
        let error = redis::cmd("args.borrowed")
            .arg(values)
            .query::<Value>(&mut con)
            .unwrap_err();
        assert!(
            error.to_string().contains("wrong number of arguments"),
            "{error}"
        );
    }
    let error = redis::cmd("args.borrowed")
        .arg(b"\xff".as_slice())
        .arg("COUNT")
        .arg(1)
        .query::<Value>(&mut con)
        .unwrap_err();
    assert!(
        error
            .detail()
            .unwrap_or_default()
            .contains("parse as UTF-8 string")
    );
    let error = redis::cmd("args.borrowed")
        .arg("expression")
        .arg("OTHER")
        .arg(1)
        .query::<Value>(&mut con)
        .unwrap_err();
    assert!(error.to_string().contains("expected COUNT"));
    // Ordinary errors leave the same connection usable for a valid command.
    let reply: Vec<i64> = redis::cmd("args.borrowed")
        .arg("expression")
        .arg("count")
        .arg(2)
        .query(&mut con)?;
    assert_eq!(reply, [10, 2, 0, 0]);
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    Ok(())
}

#[test]
fn test_argument_parser_allocations() -> Result<()> {
    let mut con = TestConnection::new("args");
    for length in [1, 32, 4096] {
        let expression = "x".repeat(length);
        for _ in 0..10 {
            for option in ["COUNT", "count", "CoUnT"] {
                // The owned parser allocates; the borrowed one does not.
                for (command, allocations, bytes) in
                    [("args.owned", 3, length + 10), ("args.borrowed", 0, 0)]
                {
                    let reply: Vec<i64> = redis::cmd(command)
                        .arg(&expression)
                        .arg(option)
                        .arg(12)
                        .query(&mut con)?;
                    assert_eq!(reply, [length as i64, 12, allocations, bytes as i64]);
                }
            }
        }
    }
    Ok(())
}

#[test]
fn test_module_lifecycle_reload_and_failed_load_recovery() -> Result<()> {
    let mut con = TestConnection::with_args("lifecycle", &["prefix", "live:", "limit", "6"]);
    let module = utils::module_path("lifecycle")?;
    let state = |con: &mut TestConnection| -> Result<(String, i64, i64)> {
        Ok(redis::cmd("lifecycle.state").query(con)?)
    };
    assert_eq!(state(&mut con)?, ("live:".into(), 6, 0));
    redis::cmd("SET")
        .arg(&["lifecycle:kept", "value"])
        .query::<()>(&mut con)?;
    for expected in 1..=2 {
        assert_eq!(
            redis::cmd("lifecycle.visit").query::<i64>(&mut con)?,
            expected
        );
    }
    redis::cmd("CONFIG")
        .arg(&["SET", "lifecycle.limit", "9"])
        .query::<()>(&mut con)?;
    assert!(
        redis::cmd("CONFIG")
            .arg(&["SET", "lifecycle.prefix", "changed:"])
            .query::<()>(&mut con)
            .is_err()
    );
    assert_eq!(state(&mut con)?, ("live:".into(), 9, 2));
    redis::cmd("MODULE")
        .arg(&["UNLOAD", "lifecycle"])
        .query::<()>(&mut con)?;

    let assert_absent = |con: &mut TestConnection| -> Result<()> {
        let modules: Vec<HashMap<String, Value>> = redis::cmd("MODULE").arg("LIST").query(con)?;
        for module in modules {
            assert_ne!(
                redis::from_redis_value_ref::<String>(&module["name"])?,
                "lifecycle"
            );
        }
        let commands: Vec<Value> = redis::cmd("COMMAND")
            .arg(&["INFO", "lifecycle.state"])
            .query(con)?;
        assert_eq!(commands, vec![Value::Nil]);
        let configs: Vec<String> = redis::cmd("CONFIG")
            .arg(&["GET", "lifecycle.*"])
            .query(con)?;
        assert!(configs.is_empty());
        assert_eq!(
            redis::cmd("GET")
                .arg("lifecycle:kept")
                .query::<String>(con)?,
            "value"
        );
        Ok(())
    };
    assert_absent(&mut con)?;
    assert!(
        redis::cmd("MODULE")
            .arg(&["LOAD", &module, "prefix", "live:", "limit", "0"])
            .query::<()>(&mut con)
            .is_err()
    );
    assert_absent(&mut con)?;

    // Replay the saved values to recover service after the failed load.
    redis::cmd("MODULE")
        .arg(&["LOAD", &module, "prefix", "live:", "limit", "9"])
        .query::<()>(&mut con)?;
    assert_eq!(state(&mut con)?, ("live:".into(), 9, 0));
    assert_eq!(redis::cmd("lifecycle.visit").query::<i64>(&mut con)?, 1);
    redis::cmd("MODULE")
        .arg(&["UNLOAD", "lifecycle"])
        .query::<()>(&mut con)?;

    // Without replay, both configurations return to their registration defaults.
    redis::cmd("MODULE")
        .arg(&["LOAD", &module])
        .query::<()>(&mut con)?;
    assert_eq!(state(&mut con)?, ("items:".into(), 4, 0));
    redis::cmd("MODULE")
        .arg(&["UNLOAD", "lifecycle"])
        .query::<()>(&mut con)?;
    redis::cmd("MODULE")
        .arg(&["LOAD", &module, "prefix", "next:", "limit", "7"])
        .query::<()>(&mut con)?;
    assert_eq!(state(&mut con)?, ("next:".into(), 7, 0));
    assert_eq!(
        redis::cmd("GET")
            .arg("lifecycle:kept")
            .query::<String>(&mut con)?,
        "value"
    );
    // The failed load released the detached context it stored, so no later
    // load found it.
    assert!(
        !con.server_log()?
            .contains("replaced a detached module context")
    );
    Ok(())
}

#[test]
fn test_module_with_native_type_refuses_unload_without_keys() -> Result<()> {
    let mut con = TestConnection::new("data_type");
    assert_eq!(redis::cmd("DBSIZE").query::<i64>(&mut con)?, 0);
    let error = redis::cmd("MODULE")
        .arg(&["UNLOAD", "alloc"])
        .query::<()>(&mut con)
        .expect_err("a registered native type must prevent unload even without keys");
    assert!(error.to_string().contains("data types"), "{error}");
    // A refused unload leaves the existing module available.
    assert_eq!(
        redis::cmd("alloc.set")
            .arg(&["after-refusal", "3"])
            .query::<i64>(&mut con)?,
        3
    );
    assert_eq!(
        redis::cmd("alloc.get")
            .arg("after-refusal")
            .query::<String>(&mut con)?,
        "AAA"
    );
    Ok(())
}

#[test]
fn test_long_utf8_module_name_is_preserved_across_load_and_unload() -> Result<()> {
    let mut con = TestConnection::new("module_name");
    let name: String = redis::cmd("module_name.name").query(&mut con)?;
    assert_eq!(name.len(), 118);
    assert!(name.ends_with("_café"));
    let assert_registered = |con: &mut TestConnection| -> Result<()> {
        let modules: Vec<HashMap<String, Value>> = redis::cmd("MODULE").arg("LIST").query(con)?;
        assert!(
            modules
                .iter()
                .any(|module| module.get("name")
                    == Some(&Value::BulkString(name.as_bytes().to_vec())))
        );
        Ok(())
    };
    assert_registered(&mut con)?;
    redis::cmd("MODULE")
        .arg(&["UNLOAD", &name])
        .query::<()>(&mut con)?;
    let command: Vec<Value> = redis::cmd("COMMAND")
        .arg(&["INFO", "module_name.name"])
        .query(&mut con)?;
    assert_eq!(command, vec![Value::Nil]);
    let module = utils::module_path("module_name")?;
    redis::cmd("MODULE")
        .arg(&["LOAD", &module])
        .query::<()>(&mut con)?;
    assert_registered(&mut con)?;
    assert_eq!(
        redis::cmd("module_name.name").query::<String>(&mut con)?,
        name
    );
    Ok(())
}

#[test]
fn test_module_version_from_cargo() -> Result<()> {
    let mut con = TestConnection::new("hello");
    let modules: Vec<HashMap<String, Value>> = redis::cmd("MODULE").arg("LIST").query(&mut con)?;
    let hello = modules
        .iter()
        .find(|module| module.get("name") == Some(&Value::BulkString(b"hello".to_vec())))
        .context("hello must appear in MODULE LIST")?;
    let version: i32 = redis::from_redis_value_ref(&hello["ver"])?;
    assert_eq!(version, redis_module::module_version!());
    assert_eq!(
        version,
        redis_module::encode_module_version(env!("CARGO_PKG_VERSION"))
    );
    Ok(())
}

#[test]
fn test_hello() -> Result<()> {
    let mut con = TestConnection::new("hello");

    let res: Vec<i32> = redis::cmd("hello.mul")
        .arg(&[3, 4])
        .query(&mut con)
        .with_context(|| "failed to run hello.mul")?;
    assert_eq!(res, vec![3, 4, 12]);

    let res: Result<Vec<i32>, RedisError> =
        redis::cmd("hello.mul").arg(&["3", "xx"]).query(&mut con);
    if res.is_ok() {
        return Err(anyhow::Error::msg("Should return an error"));
    }

    Ok(())
}

#[test]
fn test_logging_message_text() -> Result<()> {
    let mut con = TestConnection::with_log_file("log");
    for (message, expected) in [
        ("plain text", "plain text"),
        ("café 日本語", "café 日本語"),
        ("100% ready", "100% ready"),
        ("100%% ready", "100%% ready"),
        ("a\0\0b", r"a\0\0b"),
    ] {
        let reply: String = redis::cmd("log.message").arg(message).query(&mut con)?;
        assert_eq!(reply, "Logged");
        let log = con.server_log()?;
        for route in ["context", "global", "raw", "standard"] {
            let expected = format!("log-{route}: {expected}");
            assert!(
                log.lines().any(|line| line.ends_with(&expected)),
                "Missing exact log text {expected:?} in {log}"
            );
        }
    }
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    Ok(())
}

#[test]
fn test_logging_io_message_text() -> Result<()> {
    let mut con = TestConnection::with_log_file("log");
    for (message, expected) in [
        ("plain IO", "plain IO"),
        ("100%% ready", "100%% ready"),
        ("100% ready\0café", r"100% ready\0café"),
    ] {
        let reply: String = redis::cmd("log.io").arg(message).query(&mut con)?;
        assert_eq!(reply, "Logged IO");
        let log = con.server_log()?;
        let expected = format!("log-io: {expected}");
        assert!(
            log.lines().any(|line| line.ends_with(&expected)),
            "Missing exact IO log text {expected:?} in {log}"
        );
    }
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    Ok(())
}

#[test]
fn test_command_keys_and_key_slot() -> Result<()> {
    let mut con = TestConnection::new("keys_pos");
    let keys = |con: &mut TestConnection, args: &[&str]| -> redis::RedisResult<Vec<i64>> {
        redis::cmd("command_keys").arg(args).query(con)
    };
    assert_eq!(keys(&mut con, &["SET", "k", "v"])?, [1]);
    assert_eq!(keys(&mut con, &["MSET", "a", "1", "b", "2"])?, [1, 3]);
    assert!(keys(&mut con, &["PING"])?.is_empty());
    let unknown = keys(&mut con, &["NOSUCH", "x"]).unwrap_err();
    assert!(unknown.to_string().contains("unknown command"), "{unknown}");
    let arity = keys(&mut con, &["GET"]).unwrap_err();
    assert!(
        arity.to_string().contains("wrong number of arguments"),
        "{arity}"
    );
    let empty = keys(&mut con, &[]).unwrap_err();
    assert!(empty.to_string().contains("no command"), "{empty}");
    // The server is still up.
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");

    // Values from the Redis cluster documentation.
    for (key, slot) in [("somekey", 11058), ("foo{hash_tag}", 2515)] {
        let got: i64 = redis::cmd("key_slot").arg(key).query(&mut con)?;
        assert_eq!(got, slot, "{key}");
    }
    Ok(())
}

#[test]
fn test_key_for_slot() -> Result<()> {
    let mut con = TestConnection::new("keys_pos");
    for slot in [0, 1, 1234, 8192, 16383] {
        let key: String = redis::cmd("key_for_slot").arg(slot).query(&mut con)?;
        assert_eq!(redis_module::cluster::key_slot(key.as_bytes()), slot as u16);
        assert_eq!(
            redis_module::cluster::key_slot(format!("module:{key}:events").as_bytes()),
            slot as u16
        );
    }
    for args in [
        vec![],
        vec!["-1"],
        vec!["16384"],
        vec!["65536"],
        vec!["x"],
        vec!["0", "1"],
    ] {
        assert!(
            redis::cmd("key_for_slot")
                .arg(args)
                .query::<String>(&mut con)
                .is_err()
        );
    }
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    Ok(())
}

#[test]
fn test_keys_pos() -> Result<()> {
    let mut con = TestConnection::new("keys_pos");

    let res: Vec<String> = redis::cmd("keys_pos")
        .arg(&["a", "1", "b", "2"])
        .query(&mut con)
        .with_context(|| "failed to run keys_pos")?;
    assert_eq!(res, vec!["a", "b"]);

    let res: Result<Vec<String>, RedisError> =
        redis::cmd("keys_pos").arg(&["a", "1", "b"]).query(&mut con);
    if res.is_ok() {
        return Err(anyhow::Error::msg("Should return an error"));
    }

    Ok(())
}

#[test]
fn test_dbsize() -> Result<()> {
    let mut con = TestConnection::new("test_helper");

    let dbsize = |con: &mut TestConnection| -> Result<i64> {
        redis::cmd("test_helper.dbsize")
            .query(con)
            .with_context(|| "failed to run test_helper.dbsize")
    };
    assert_eq!(dbsize(&mut con)?, 0);
    redis::cmd("MSET")
        .arg(&["a", "1", "b", "2", "c", "3"])
        .query::<()>(&mut con)?;
    assert_eq!(dbsize(&mut con)?, 3);

    // dbsize counts the selected database only.
    redis::cmd("SELECT").arg(1).query::<()>(&mut con)?;
    assert_eq!(dbsize(&mut con)?, 0);

    Ok(())
}

#[test]
fn test_helper_milliseconds() -> Result<()> {
    let mut con = TestConnection::new("test_helper");
    let server_time = |con: &mut TestConnection| -> Result<i64> {
        let (seconds, microseconds): (i64, i64) = redis::cmd("TIME").query(con)?;
        Ok(seconds * 1000 + microseconds / 1000)
    };
    for command in [
        "test_helper.milliseconds",
        "test_helper.milliseconds_worker",
    ] {
        let before = server_time(&mut con)?;
        let now: i64 = redis::cmd(command).query(&mut con)?;
        let after = server_time(&mut con)?;
        assert!(
            (before.min(after)..=before.max(after)).contains(&now),
            "{command}: {now} must match the server's Unix milliseconds between {before} and {after}"
        );
        assert!(
            redis::cmd(command)
                .arg("extra")
                .query::<i64>(&mut con)
                .is_err()
        );
    }
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    Ok(())
}

#[test]
fn test_helper_version() -> Result<()> {
    let mut con = TestConnection::new("test_helper");

    let res: Vec<i64> = redis::cmd("test_helper.version")
        .query(&mut con)
        .with_context(|| "failed to run test_helper.version")?;
    assert!(res[0] > 0);

    // Test also an internal implementation that might not always be reached
    let res2: Vec<i64> = redis::cmd("test_helper._version_rm_call")
        .query(&mut con)
        .with_context(|| "failed to run test_helper._version_rm_call")?;
    let info: String = redis::cmd("INFO").arg("server").query(&mut con)?;
    let parse = |field: &str| -> Result<Vec<i64>> {
        info.lines()
            .find_map(|line| line.strip_prefix(field))
            .with_context(|| format!("missing INFO field {field}"))?
            .split('.')
            .map(|part| part.parse().map_err(Into::into))
            .collect()
    };
    // Valkey's native API reports its product version, while redis_version in
    // INFO advertises its Redis compatibility version. Check both paths
    // against their respective fields rather than requiring them to agree.
    assert_eq!(res2, parse("redis_version:")?);
    let native_field = if info.lines().any(|line| line.starts_with("valkey_version:")) {
        "valkey_version:"
    } else {
        "redis_version:"
    };
    assert_eq!(res, parse(native_field)?);

    Ok(())
}

#[test]
fn test_command_name() -> Result<()> {
    let mut con = TestConnection::new("test_helper");

    // Call the tested command
    let res: Result<String, RedisError> = redis::cmd("test_helper.name").query(&mut con);

    // The expected result is according to redis version
    let info: String = redis::cmd("info")
        .arg(&["server"])
        .query(&mut con)
        .with_context(|| "failed to run test_helper.name")?;

    if let Ok(ver) = redis_module::Context::version_from_info(RedisValue::SimpleString(info)) {
        if ver.major > 6
            || (ver.major == 6 && ver.minor > 2)
            || (ver.major == 6 && ver.minor == 2 && ver.patch >= 5)
        {
            assert_eq!(res.unwrap(), String::from("test_helper.name"));
        } else {
            assert!(
                res.err()
                    .unwrap()
                    .to_string()
                    .contains("RedisModule_GetCurrentCommandName is not available")
            );
        }
    }

    Ok(())
}

#[test]
fn test_helper_info() -> Result<()> {
    const MODULES: [(&str, bool); 4] = [
        ("test_helper", false),
        ("info_handler_macro", false),
        ("info_handler_builder", true),
        ("info_handler_struct", true),
    ];

    MODULES
        .into_iter()
        .try_for_each(|(module, has_dictionary)| {
            let mut con = TestConnection::new(module);

            let res: String = redis::cmd("INFO")
                .arg(module)
                .query(&mut con)
                .with_context(|| format!("failed to run INFO {module}"))?;

            assert!(res.contains(&format!("{module}_field:value")));
            if has_dictionary {
                assert!(res.contains("dictionary:key=value"));
            }

            Ok(())
        })
}

#[test]
fn test_info_handler_atomic_snapshot() -> Result<()> {
    let mut con = TestConnection::new("info_handler_builder");
    let info: String = redis::cmd("INFO")
        .arg("info_handler_builder")
        .query(&mut con)?;
    assert!(
        info.lines()
            .any(|line| line == "info_handler_builder_recorded:0")
    );

    for _ in 0..3 {
        let reply: String = redis::cmd("info_handler_builder.record").query(&mut con)?;
        assert_eq!(reply, "OK");
    }
    assert!(
        redis::cmd("info_handler_builder.record")
            .arg("extra")
            .query::<String>(&mut con)
            .is_err()
    );

    // Both the protocol INFO path and a nested native GetServerInfo callback
    // see the independently synchronized scalar. No runtime context is added
    // to the INFO callback, and this does not simulate crash reporting.
    let info: String = redis::cmd("INFO").arg("modules").query(&mut con)?;
    assert!(
        info.lines()
            .any(|line| line == "info_handler_builder_recorded:3")
    );
    let snapshot: String = redis::cmd("info_handler_builder.snapshot").query(&mut con)?;
    assert_eq!(snapshot, "3");
    Ok(())
}

#[test]
fn test_info_handler_multiple_sections() -> Result<()> {
    const MODULES: [&str; 1] = ["info_handler_multiple_sections"];

    MODULES.into_iter().try_for_each(|module| {
        let mut con = TestConnection::new(module);

        let res: String = redis::cmd("INFO")
            .arg(format!("{module}_InfoSection2"))
            .query(&mut con)
            .with_context(|| format!("failed to run INFO {module}"))?;

        assert!(res.contains(&format!("{module}_field_2:value2")));
        assert!(!res.contains(&format!("{module}_field_1:value1")));

        Ok(())
    })
}

#[allow(unused_must_use)]
#[test]
fn test_test_helper_err() -> Result<()> {
    let mut con = TestConnection::new("hello");

    // Make sure embedded nulls do not cause a crash
    redis::cmd("test_helper.err")
        .arg(&["\x00\x00"])
        .query::<()>(&mut con);

    redis::cmd("test_helper.err")
        .arg(&["no crash\x00"])
        .query::<()>(&mut con);

    Ok(())
}

#[test]
fn test_key_delete_result() -> Result<()> {
    let mut con = TestConnection::new("string");

    for command in ["string.del", "string.unlink"] {
        redis::cmd("SET").arg(&["k", "v"]).query::<()>(&mut con)?;
        let deleted: i64 = redis::cmd(command).arg("k").query(&mut con)?;
        assert_eq!(deleted, 1, "{command} of an existing key");
        let exists: i64 = redis::cmd("EXISTS").arg("k").query(&mut con)?;
        assert_eq!(exists, 0);
        let deleted: i64 = redis::cmd(command).arg("k").query(&mut con)?;
        assert_eq!(deleted, 0, "{command} of a missing key");
    }

    Ok(())
}

#[test]
fn test_hash_mutation_counts() -> Result<()> {
    let mut con = TestConnection::new("hash");
    let run = |con: &mut TestConnection, cmd: &str, args: &[&[u8]]| -> Result<i64> {
        Ok(redis::cmd(cmd).arg(args).query(con)?)
    };

    // A new field replaces nothing; setting it again replaces one.
    assert_eq!(run(&mut con, "hash.set", &[b"h", b"f", b"v1"])?, 0);
    assert_eq!(run(&mut con, "hash.set", &[b"h", b"f", b"v2"])?, 1);
    let value: String = redis::cmd("HGET").arg(&["h", "f"]).query(&mut con)?;
    assert_eq!(value, "v2");
    assert_eq!(run(&mut con, "hash.del", &[b"h", b"f"])?, 1);
    assert_eq!(run(&mut con, "hash.del", &[b"h", b"f"])?, 0);

    // Byte fields and values keep every byte.
    let field: &[u8] = b"a\0b\r\n";
    let value: &[u8] = b"\0\xff value";
    assert_eq!(run(&mut con, "hash.set_bytes", &[b"h", field, value])?, 0);
    assert_eq!(run(&mut con, "hash.set_bytes", &[b"h", field, value])?, 1);
    let stored: Vec<u8> = redis::cmd("HGET").arg(b"h").arg(field).query(&mut con)?;
    assert_eq!(stored, value);
    assert_eq!(run(&mut con, "hash.del_bytes", &[b"h", field])?, 1);
    assert_eq!(run(&mut con, "hash.del_bytes", &[b"h", field])?, 0);

    // A key of another type is WRONGTYPE.
    redis::cmd("SET").arg(&["s", "v"]).query::<()>(&mut con)?;
    for (cmd, args) in [
        ("hash.set", &[&b"s"[..], b"f", b"v"][..]),
        ("hash.del", &[b"s", b"f"]),
        ("hash.set_bytes", &[b"s", b"f", b"v"]),
        ("hash.del_bytes", &[b"s", b"f"]),
    ] {
        let error = redis::cmd(cmd)
            .arg(args)
            .query::<i64>(&mut con)
            .expect_err("a string key accepted a hash mutation");
        assert_eq!(error.code(), Some("WRONGTYPE"), "{cmd}: {error}");
    }
    Ok(())
}

#[test]
fn test_event_loop() -> Result<()> {
    let mut con = TestConnection::new("event_loop");

    // Polls `query` until it returns `expected`, for up to a second.
    fn wait_for(con: &mut TestConnection, query: &redis::Cmd, expected: &[u8]) -> Result<Vec<u8>> {
        let mut value = Vec::new();
        for _ in 0..100 {
            value = query.query::<Option<Vec<u8>>>(con)?.unwrap_or_default();
            if value == expected {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(value)
    }

    // A one-shot scheduled from another thread runs on the main thread.
    redis::cmd("eventloop.set_later")
        .arg(&["later", "done"])
        .query::<()>(&mut con)?;
    let mut get = redis::cmd("GET");
    get.arg("later");
    assert_eq!(wait_for(&mut con, &get, b"done")?, b"done");

    // A readable callback reads a pipe, then removes itself.
    for text in ["first", "second"] {
        redis::cmd("eventloop.pipe")
            .arg(text)
            .query::<()>(&mut con)?;
        let last_read = redis::cmd("eventloop.last_read");
        assert_eq!(
            wait_for(&mut con, &last_read, text.as_bytes())?,
            text.as_bytes()
        );
    }
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");

    // remove refuses a descriptor it did not add, here this client's socket.
    let info: String = redis::cmd("CLIENT").arg("INFO").query(&mut con)?;
    let fd = info
        .split_whitespace()
        .find_map(|field| field.strip_prefix("fd="))
        .context("CLIENT INFO has no fd")?;
    let error = redis::cmd("eventloop.remove")
        .arg(fd)
        .query::<String>(&mut con)
        .expect_err("remove accepted a client's descriptor");
    assert!(
        error.to_string().contains("no event loop callback"),
        "{error}"
    );
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");

    Ok(())
}

#[test]
fn test_yield_lets_clients_get_busy_replies() -> Result<()> {
    let mut con = TestConnection::new("yield");
    redis::cmd("CONFIG")
        .arg(&["SET", "busy-reply-threshold", "10"])
        .query::<()>(&mut con)?;

    let mut runner = con.connection()?;
    let busy = std::thread::spawn(move || -> redis::RedisResult<i64> {
        redis::cmd("yield.busy")
            .arg(&["1000", "clients"])
            .query(&mut runner)
    });

    // PING answers PONG until yield.busy starts, then BUSY while it yields.
    // Without yielding, PING would wait for yield.busy to finish.
    let mut error = None;
    for _ in 0..90 {
        match redis::cmd("PING").query::<String>(&mut con) {
            Ok(_) => std::thread::sleep(Duration::from_millis(10)),
            Err(busy_error) => {
                error = Some(busy_error);
                break;
            }
        }
    }
    let error = error.context("PING never got a BUSY reply")?;
    assert_eq!(error.code(), Some("BUSY"), "{error}");
    assert!(
        error.to_string().contains("yield.busy is running"),
        "{error}"
    );

    let yields = busy.join().expect("yield.busy thread panicked")?;
    assert!(yields > 0);
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    Ok(())
}

/// Sends `args` as a command on a new connection and reads the raw RESP reply,
/// which must be `expected.len()` bytes.
fn raw_reply(con: &TestConnection, args: &[&str], expected: &str) -> Result<String> {
    use std::io::{Read, Write};
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", con.port()))?;
    let mut request = format!("*{}\r\n", args.len());
    for arg in args {
        request.push_str(&format!("${}\r\n{arg}\r\n", arg.len()));
    }
    stream.write_all(request.as_bytes())?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
    let mut reply = vec![0; expected.len()];
    stream.read_exact(&mut reply)?;
    Ok(String::from_utf8_lossy(&reply).into_owned())
}

#[test]
fn test_error_inside_an_array() -> Result<()> {
    // redis-rs 0.23 fails the whole reply on an error inside an array, so
    // read the RESP bytes.
    let con = TestConnection::new("response");
    let expected = "*3\r\n:1\r\n-ERR item 2 failed\r\n$5\r\nthree\r\n";
    assert_eq!(raw_reply(&con, &["errors.in_array"], expected)?, expected);
    Ok(())
}

#[test]
fn test_call_reply_errors_stay_errors() -> Result<()> {
    let con = TestConnection::new("call_reply");
    // Forwarded through Context::call, the error inside the script's array
    // reply stays an error.
    let script = "return {1, redis.error_reply('ERR inside')}";
    let expected = "*2\r\n:1\r\n-ERR inside\r\n";
    assert_eq!(
        raw_reply(&con, &["call_reply.eval", script, "0"], expected)?,
        expected
    );
    Ok(())
}

#[test]
fn test_simple_string_map_keys() -> Result<()> {
    let con = TestConnection::new("response");
    // RESP2 receives the map as a flat array; each key keeps its type, also
    // after a call reply is converted and forwarded. The forwarded map is
    // sorted by key, and String keys sort before SimpleString ones.
    for (command, expected) in [
        ("map.simple_keys", "*4\r\n+a\r\n:1\r\n$1\r\nb\r\n:2\r\n"),
        (
            "map.forward_simple_keys",
            "*4\r\n$1\r\nb\r\n:2\r\n+a\r\n:1\r\n",
        ),
    ] {
        assert_eq!(
            raw_reply(&con, &[command], expected)?,
            expected,
            "{command}"
        );
    }
    Ok(())
}

#[test]
fn test_insertion_ordered_map() -> Result<()> {
    let mut con = TestConnection::new("response");

    // RESP2 receives the map as a flat array of keys and values, in order.
    let res: Vec<redis::Value> = redis::cmd("map.ordered").query(&mut con)?;
    assert_eq!(
        res,
        [
            redis::Value::BulkString(b"zebra".to_vec()),
            redis::Value::Int(1),
            redis::Value::BulkString(b"apple".to_vec()),
            redis::Value::Int(2),
            redis::Value::BulkString(b"mango".to_vec()),
            redis::Value::Int(3),
        ]
    );

    Ok(())
}

#[test]
fn test_memory_accounting() -> Result<()> {
    let mut con = TestConnection::new("memory");

    let ratio = |con: &mut TestConnection| -> Result<f64> {
        let ratio: String = redis::cmd("memory.ratio").query(con)?;
        Ok(ratio.parse()?)
    };
    redis::cmd("CONFIG")
        .arg(&["SET", "maxmemory", "0"])
        .query::<()>(&mut con)?;
    assert_eq!(ratio(&mut con)?, 0.0);
    redis::cmd("CONFIG")
        .arg(&["SET", "maxmemory", "1gb"])
        .query::<()>(&mut con)?;
    let limited = ratio(&mut con)?;
    assert!(limited > 0.0 && limited < 1.0, "ratio {limited}");

    let text = "x".repeat(1000);
    let size: i64 = redis::cmd("memory.string_size")
        .arg(&text)
        .query(&mut con)?;
    assert!(size >= 1000, "string size {size}");

    let (buffer, aligned): (i64, i64) = redis::cmd("memory.buffer_sizes")
        .arg(10_000)
        .query(&mut con)?;
    assert!((10_000..20_000).contains(&buffer), "buffer size {buffer}");
    // 64 bytes, plus the header and padding of an over-aligned block.
    assert!(aligned >= 64 + 8, "aligned size {aligned}");

    Ok(())
}

#[test]
fn test_block_client_on_reply() -> Result<()> {
    let mut con = TestConnection::new("block");

    let res: String = redis::cmd("block.reply")
        .arg(&["50", "hello"])
        .query(&mut con)
        .with_context(|| "failed to run block.reply")?;
    assert_eq!(res, "hello");

    // The timeout passes before the thread unblocks, and the late unblock only
    // frees the value.
    let res: String = redis::cmd("block.timeout")
        .arg(&["50", "300"])
        .query(&mut con)
        .with_context(|| "failed to run block.timeout")?;
    assert_eq!(res, "timed out");
    let res: String = redis::cmd("block.timeout")
        .arg(&["2000", "50"])
        .query(&mut con)
        .with_context(|| "failed to run block.timeout")?;
    assert_eq!(res, "done");

    let error = redis::cmd("block.dropped")
        .query::<String>(&mut con)
        .expect_err("a dropped handle replied with a value");
    assert!(
        error.to_string().contains("released without a reply"),
        "{error}"
    );

    // The late unblock from the timed-out command frees its value with the
    // private data; wait up to two seconds for it.
    let mut live: i64 = -1;
    for _ in 0..200 {
        live = redis::cmd("block.live_values").query(&mut con)?;
        if live == 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(live, 0);

    // A context that cannot block gets an error instead of a handle.
    redis::cmd("MULTI").query::<()>(&mut con)?;
    redis::cmd("block.reply")
        .arg(&["10", "x"])
        .query::<()>(&mut con)?;
    // redis-rs reports the error inside EXEC's reply as the EXEC result.
    let error = redis::cmd("EXEC")
        .query::<Vec<redis::Value>>(&mut con)
        .expect_err("a command blocked inside MULTI");
    assert!(error.to_string().contains("cannot block here"), "{error}");
    let error = redis::cmd("block.timeout")
        .arg(&[&i64::MAX.to_string(), "10"])
        .query::<String>(&mut con)
        .expect_err("an out of range timeout blocked");
    assert!(
        error
            .to_string()
            .contains("blocking timeout is out of range"),
        "{error}"
    );
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");

    Ok(())
}

fn check_client_blocking_rejections(commands: &[(&str, &[&str])], calls: &[&str]) -> Result<()> {
    let mut con = TestConnection::new("block");
    for &(command, args) in commands {
        let error = redis::cmd("EVAL")
            .arg("return redis.pcall(ARGV[1], unpack(ARGV, 2))")
            .arg(0)
            .arg(command)
            .arg(args)
            .query::<Value>(&mut con)
            .expect_err("client blocked inside a script");
        assert!(error.to_string().contains("cannot block here"), "{error}");
        assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");

        redis::cmd("MULTI").query::<()>(&mut con)?;
        redis::cmd(command).arg(args).query::<()>(&mut con)?;
        let error = redis::cmd("EXEC")
            .query::<Value>(&mut con)
            .expect_err("client blocked inside a transaction");
        assert!(error.to_string().contains("cannot block here"), "{error}");
        assert_eq!(
            redis::cmd("ECHO")
                .arg("aligned")
                .query::<String>(&mut con)?,
            "aligned"
        );
    }
    for command in calls {
        let error = redis::cmd(command)
            .query::<Value>(&mut con)
            .expect_err("nested call blocked without permission");
        assert!(error.to_string().contains("cannot block here"), "{error}");
        assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    }
    assert_eq!(redis::cmd("block.direct_blocks").query::<i64>(&mut con)?, 0);
    assert_eq!(redis::cmd("block.live_values").query::<i64>(&mut con)?, 0);
    assert_eq!(
        redis::cmd("block.direct").query::<String>(&mut con)?,
        "direct reply"
    );
    assert_eq!(redis::cmd("block.direct_blocks").query::<i64>(&mut con)?, 1);
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    Ok(())
}

// This also runs against Redis 6.0 in CI to exercise its older context flags.
#[test]
fn test_direct_client_blocking_rejects_denied_invocations() -> Result<()> {
    check_client_blocking_rejections(&[("block.direct", &[])], &["block.call_direct"])
}

#[test]
fn test_callback_client_blocking_rejects_denied_invocations() -> Result<()> {
    check_client_blocking_rejections(&[("block.reply", &["0", "ignored"])], &["block.call_reply"])
}

#[test]
fn test_client_blocking_rejects_reply_and_timeout_reentry() -> Result<()> {
    let mut con = TestConnection::new("block");
    for command in ["block.reblock_reply", "block.reblock_timeout"] {
        let errors: Vec<String> = redis::cmd(command).query(&mut con)?;
        assert_eq!(errors.len(), 2);
        assert!(
            errors
                .iter()
                .all(|error| error.contains("reply or timeout callback")),
            "{errors:?}"
        );
        utils::wait_until(
            Duration::from_secs(5),
            "late blocked-client payload cleanup",
            || Ok(redis::cmd("block.live_values").query::<i64>(&mut con)? == 0),
        )?;
        assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    }
    assert_eq!(
        redis::cmd("block.direct").query::<String>(&mut con)?,
        "direct reply"
    );
    Ok(())
}

#[test]
fn test_string() -> Result<()> {
    let mut con = TestConnection::new("string");

    redis::cmd("string.set")
        .arg(&["key", "value"])
        .query::<()>(&mut con)
        .with_context(|| "failed to run string.set")?;

    let res: String = redis::cmd("string.get").arg(&["key"]).query(&mut con)?;

    assert_eq!(&res, "value");

    Ok(())
}

#[test]
fn test_string_text_and_bytes() -> Result<()> {
    let mut con = TestConnection::new("string");

    // CR and LF would end a simple string early; they become spaces.
    let res: String = redis::cmd("string.reply")
        .arg(&["simple", "caf\u{e9}\r\n+OK"])
        .query(&mut con)
        .with_context(|| "failed to run string.reply simple")?;
    assert_eq!(res, "caf\u{e9}  +OK");
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");

    let error = redis::cmd("string.reply")
        .arg(&["error", "ERR caf\u{e9}"])
        .query::<String>(&mut con)
        .expect_err("string.reply error replied with a value");
    assert!(error.to_string().contains("caf\u{e9}"), "{error}");

    let res: Vec<Vec<u8>> = redis::cmd("string.create_nul")
        .query(&mut con)
        .with_context(|| "failed to run string.create_nul")?;
    assert_eq!(res, [b"a\0b".to_vec(), b"c\0d".to_vec()]);

    Ok(())
}

#[test]
fn test_scan() -> Result<()> {
    let mut con = TestConnection::new("scan_keys");

    redis::cmd("set")
        .arg(&["x", "1"])
        .query::<()>(&mut con)
        .with_context(|| "failed to run string.set")?;

    redis::cmd("set")
        .arg(&["y", "1"])
        .query::<()>(&mut con)
        .with_context(|| "failed to run string.set")?;

    let mut res: Vec<String> = redis::cmd("scan_keys").query(&mut con)?;
    res.sort();

    assert_eq!(&res, &["x", "y"]);

    Ok(())
}

#[test]
fn test_scan_key() -> Result<()> {
    let mut con = TestConnection::new("scan_keys");
    redis::cmd("hset")
        .arg(&[
            "user:123", "name", "Alice", "age", "29", "location", "Austin",
        ])
        .query::<()>(&mut con)
        .with_context(|| "failed to hset")?;

    let res: Vec<String> = redis::cmd("scan_key")
        .arg(&["user:123"])
        .query(&mut con)
        .with_context(|| "failed scan_key")?;
    assert_eq!(&res, &["name", "Alice", "age", "29", "location", "Austin"]);
    Ok(())
}

#[test]
fn test_scan_key_for_each() -> Result<()> {
    let mut con = TestConnection::new("scan_keys");
    redis::cmd("hset")
        .arg(&[
            "user:123", "name", "Alice", "age", "29", "location", "Austin",
        ])
        .query::<()>(&mut con)
        .with_context(|| "failed to hset")?;

    let res: Vec<String> = redis::cmd("scan_key_for_each")
        .arg(&["user:123"])
        .query(&mut con)
        .with_context(|| "failed scan_key_for_each")?;
    assert_eq!(&res, &["name", "Alice", "age", "29", "location", "Austin"]);
    Ok(())
}

#[test]
fn test_panicking_commands_reply_with_an_error() -> Result<()> {
    let mut con = TestConnection::new("panic");

    for (command, message) in [
        ("panic.command", "panic.command was called"),
        ("panic_proc_macro", "panic_proc_macro was called"),
    ] {
        let error = redis::cmd(command)
            .query::<String>(&mut con)
            .expect_err("a panicking command replied");
        let error = error.to_string();
        assert!(
            error.contains("command panicked") && error.contains(message),
            "{command}: {error}"
        );
        assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    }

    // A panic in a scan callback comes back out of the scan and fails the
    // command that started it.
    redis::cmd("SET")
        .arg(&["panic:key", "value"])
        .query::<()>(&mut con)?;
    let error = redis::cmd("panic.scan")
        .query::<String>(&mut con)
        .expect_err("a panicking scan replied");
    assert!(
        error.to_string().contains("scan callback panicked"),
        "{error}"
    );
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");

    Ok(())
}

#[test]
fn test_panicking_callbacks_are_logged() -> Result<()> {
    let mut con = TestConnection::new("panic");

    // The keyspace event handler panics on this key; the SET still succeeds.
    redis::cmd("SET")
        .arg(&["panic:notify", "value"])
        .query::<()>(&mut con)?;
    let value: String = redis::cmd("GET").arg("panic:notify").query(&mut con)?;
    assert_eq!(value, "value");

    let reply: String = redis::cmd("panic.timer").query(&mut con)?;
    assert_eq!(reply, "timer created");
    thread::sleep(Duration::from_millis(100));
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");

    // A panicking setter fails the CONFIG SET; a panicking getter reads as 0.
    let error = redis::cmd("CONFIG")
        .arg(&["SET", "panic.number", "7"])
        .query::<String>(&mut con)
        .expect_err("a panicking setter was accepted");
    assert!(
        error.to_string().contains("configuration setter panicked"),
        "{error}"
    );
    redis::cmd("CONFIG")
        .arg(&["SET", "panic.number", "13"])
        .query::<()>(&mut con)?;
    let value: Vec<String> = redis::cmd("CONFIG")
        .arg(&["GET", "panic.number"])
        .query(&mut con)?;
    assert_eq!(value, ["panic.number", "0"]);
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");

    Ok(())
}

#[test]
fn test_stream_reader() -> Result<()> {
    let mut con = TestConnection::new("stream");

    let _: String = redis::cmd("XADD")
        .arg(&["s", "1-1", "foo", "bar"])
        .query(&mut con)
        .with_context(|| "failed to add data to the stream")?;

    let _: String = redis::cmd("XADD")
        .arg(&["s", "1-2", "foo", "bar"])
        .query(&mut con)
        .with_context(|| "failed to add data to the stream")?;

    let res: String = redis::cmd("STREAM_POP")
        .arg(&["s"])
        .query(&mut con)
        .with_context(|| "failed to run keys_pos")?;
    assert_eq!(res, "1-1");

    let res: String = redis::cmd("STREAM_POP")
        .arg(&["s"])
        .query(&mut con)
        .with_context(|| "failed to run keys_pos")?;
    assert_eq!(res, "1-2");

    let res: usize = redis::cmd("XLEN")
        .arg(&["s"])
        .query(&mut con)
        .with_context(|| "failed to add data to the stream")?;

    assert_eq!(res, 0);

    Ok(())
}

#[test]
fn test_stream_trim_by_id_accepts_zero_and_matches_native_counts() -> Result<()> {
    let mut con = TestConnection::new("stream");
    for mode in ["exact", "approx"] {
        redis::cmd("DEL")
            .arg(&["module", "native", "empty"])
            .query::<()>(&mut con)?;
        for key in ["module", "native"] {
            for id in ["10-1", "10-2", "20-0"] {
                redis::cmd("XADD")
                    .arg(&[key, id, "field", "value"])
                    .query::<String>(&mut con)?;
            }
        }
        for (id, exact_count) in [
            ("0-0", 0),
            ("10-1", 0),
            ("10-2", 1),
            ("20-0", 1),
            ("18446744073709551615-18446744073709551615", 1),
            ("18446744073709551615-18446744073709551615", 0),
        ] {
            let removed: i64 = redis::cmd("STREAM_TRIM_BEFORE")
                .arg(&["module", id, mode])
                .query(&mut con)?;
            let native: i64 = redis::cmd("XTRIM")
                .arg(&[
                    "native",
                    "MINID",
                    if mode == "exact" { "=" } else { "~" },
                    id,
                ])
                .query(&mut con)?;
            assert_eq!(removed, native, "{mode} trim before {id}");
            if mode == "exact" {
                assert_eq!(removed, exact_count, "exact trim before {id}");
            }
            let module_entries: redis::Value = redis::cmd("XRANGE")
                .arg(&["module", "-", "+"])
                .query(&mut con)?;
            let native_entries: redis::Value = redis::cmd("XRANGE")
                .arg(&["native", "-", "+"])
                .query(&mut con)?;
            assert_eq!(
                module_entries, native_entries,
                "entries after {mode} trim before {id}"
            );
        }
        // A stream with no entries still exists and has a successful zero trim.
        redis::cmd("XADD")
            .arg(&["empty", "1-0", "field", "value"])
            .query::<String>(&mut con)?;
        redis::cmd("XDEL")
            .arg(&["empty", "1-0"])
            .query::<i64>(&mut con)?;
        assert_eq!(
            redis::cmd("STREAM_TRIM_BEFORE")
                .arg(&["empty", "10-0", mode])
                .query::<i64>(&mut con)?,
            0
        );
        assert_eq!(redis::cmd("EXISTS").arg("empty").query::<i64>(&mut con)?, 1);

        // The module API rejects missing/wrong-type keys; these are not no-op successes.
        redis::cmd("SET")
            .arg(&["wrong-type", "unchanged"])
            .query::<()>(&mut con)?;
        for key in ["missing", "wrong-type"] {
            let error = redis::cmd("STREAM_TRIM_BEFORE")
                .arg(&[key, "10-0", mode])
                .query::<i64>(&mut con)
                .unwrap_err();
            assert_eq!(error.detail(), Some("trimming the stream"));
        }
        assert_eq!(
            redis::cmd("EXISTS").arg("missing").query::<i64>(&mut con)?,
            0
        );
        assert_eq!(
            redis::cmd("GET")
                .arg("wrong-type")
                .query::<String>(&mut con)?,
            "unchanged"
        );
    }
    Ok(())
}

#[test]
fn test_stream_iterator_stops_on_drop() -> Result<()> {
    let mut con = TestConnection::new("stream");

    for id in ["1-1", "1-2"] {
        let _: String = redis::cmd("XADD")
            .arg(&["s", id, "foo", "bar"])
            .query(&mut con)
            .with_context(|| "failed to add data to the stream")?;
    }

    // The second iterator starts only if dropping the first one stopped it.
    let res: Vec<String> = redis::cmd("STREAM_FIRST_TWICE")
        .arg(&["s"])
        .query(&mut con)
        .with_context(|| "failed to run STREAM_FIRST_TWICE")?;
    assert_eq!(res, ["1-1", "1-1"]);

    let res: usize = redis::cmd("XLEN")
        .arg(&["s"])
        .query(&mut con)
        .with_context(|| "failed to run XLEN")?;
    assert_eq!(res, 2);

    let _: () = redis::cmd("DEL").arg(&["s"]).query(&mut con)?;
    let _: String = redis::cmd("XADD")
        .arg(&["s", "1-1", "foo", "bar"])
        .query(&mut con)?;
    let _: i64 = redis::cmd("XDEL").arg(&["s", "1-1"]).query(&mut con)?;
    let res: Option<String> = redis::cmd("STREAM_POP")
        .arg(&["s"])
        .query(&mut con)
        .with_context(|| "failed to run STREAM_POP on an empty stream")?;
    assert_eq!(res, None);

    Ok(())
}

#[test]
#[cfg(any(
    feature = "min-redis-compatibility-version-8-0",
    feature = "min-redis-compatibility-version-7-4",
    feature = "min-redis-compatibility-version-7-2"
))]
fn test_call() -> Result<()> {
    let mut con = TestConnection::new("call");

    let res: String = redis::cmd("call.test")
        .query(&mut con)
        .with_context(|| "failed to run string.set")?;

    assert_eq!(&res, "pass");

    Ok(())
}

#[test]
#[cfg(any(
    feature = "min-redis-compatibility-version-8-0",
    feature = "min-redis-compatibility-version-7-4",
    feature = "min-redis-compatibility-version-7-2"
))]
fn test_call_verbatim_reply() -> Result<()> {
    let mut con = TestConnection::new("call");

    // [as_parts format, raw format, same data, data starts with "# Server"]
    let res: (String, String, i64, i64) = redis::cmd("call.verbatim")
        .query(&mut con)
        .with_context(|| "failed to run call.verbatim")?;
    assert_eq!(res, ("txt".to_owned(), "txt".to_owned(), 1, 1));

    Ok(())
}

#[test]
#[cfg(any(
    feature = "min-redis-compatibility-version-8-0",
    feature = "min-redis-compatibility-version-7-4",
    feature = "min-redis-compatibility-version-7-2"
))]
fn test_call_reply_kinds_and_dry_run() -> Result<()> {
    use redis::Value;

    let mut con = TestConnection::new("call");

    // Forwarded and converted replies keep simple versus bulk strings.
    for command in ["call.forward", "call.convert"] {
        let res: Value = redis::cmd(command)
            .arg(&["SET", "k", "v"])
            .query(&mut con)?;
        assert_eq!(res, Value::Okay, "{command} SET");
        let res: Value = redis::cmd(command).arg(&["GET", "k"]).query(&mut con)?;
        assert_eq!(res, Value::BulkString(b"v".to_vec()), "{command} GET");
        let res: Value = redis::cmd(command).arg(&["PING"]).query(&mut con)?;
        assert_eq!(
            res,
            Value::SimpleString("PONG".to_owned()),
            "{command} PING"
        );
    }
    let error = redis::cmd("call.forward")
        .arg(&["INCR", "k"])
        .query::<Value>(&mut con)
        .expect_err("INCR on a non-number replied with a value");
    assert!(error.to_string().contains("not an integer"), "{error}");

    // Without the E flag a failed call is an error, not an empty reply.
    let error = redis::cmd("call.convert")
        .arg(&["NOSUCHCMD"])
        .query::<Value>(&mut con)
        .expect_err("an unknown command returned a value");
    assert!(error.to_string().contains("unknown command"), "{error}");

    // A RESP3 map cannot go to this RESP2 client as it is.
    redis::cmd("HSET")
        .arg(&["h", "f", "v"])
        .query::<()>(&mut con)?;
    let error = redis::cmd("call.forward3")
        .arg(&["HGETALL", "h"])
        .query::<Value>(&mut con)
        .expect_err("a RESP3 map was forwarded to a RESP2 client");
    assert!(
        error.to_string().contains("cannot send this reply"),
        "{error}"
    );

    // A dry run checks the command without running it.
    let res: String = redis::cmd("call.dry_run")
        .arg(&["SET", "dry", "v"])
        .query(&mut con)?;
    assert_eq!(res, "would run");
    let exists: i64 = redis::cmd("EXISTS").arg("dry").query(&mut con)?;
    assert_eq!(exists, 0);
    let error = redis::cmd("call.dry_run")
        .arg(&["NOSUCHCMD"])
        .query::<String>(&mut con)
        .expect_err("a dry run of an unknown command passed");
    assert!(error.to_string().contains("unknown command"), "{error}");

    Ok(())
}

#[test]
fn test_ctx_flags() -> Result<()> {
    let mut con = TestConnection::new("ctx_flags");

    let res: String = redis::cmd("my_role").query(&mut con)?;

    assert_eq!(&res, "master");

    Ok(())
}

#[test]
fn test_get_current_user() -> Result<()> {
    let mut con = TestConnection::new("acl");

    let res: String = redis::cmd("get_current_user").query(&mut con)?;

    assert_eq!(&res, "default");

    Ok(())
}

#[test]
#[cfg(any(
    feature = "min-redis-compatibility-version-8-0",
    feature = "min-redis-compatibility-version-7-4"
))]
fn test_set_acl_categories() -> Result<()> {
    let mut con = TestConnection::new("acl");

    let res: Vec<String> = redis::cmd("ACL").arg("CAT").query(&mut con)?;
    assert!(res.contains(&"acl".to_owned()));

    Ok(())
}

#[test]
#[cfg(feature = "min-redis-compatibility-version-8-0")]
fn test_set_acl_categories_commands() -> Result<()> {
    let mut con = TestConnection::new("acl");

    let res: Vec<String> = redis::cmd("ACL").arg("CAT").arg("acl").query(&mut con)?;
    assert!(
        res.contains(&"verify_key_access_for_user".to_owned())
            && res.contains(&"get_current_user".to_owned())
    );

    Ok(())
}

fn supports_custom_acl_categories(con: &mut TestConnection) -> Result<bool> {
    let info: String = redis::cmd("INFO").arg("server").query(con)?;
    let version = redis_module::Context::version_from_info(RedisValue::BulkString(info))?;
    Ok((version.major, version.minor) >= (7, 4))
}

#[test]
fn test_acl_category_survives_module_reload() -> Result<()> {
    let mut con = TestConnection::with_log_file("acl");
    if !supports_custom_acl_categories(&mut con)? {
        return Ok(());
    }
    let module = utils::module_path("acl")?;
    redis::cmd("ACL")
        .arg(&[
            "SETUSER",
            "category-user",
            "on",
            ">pass",
            "~*",
            "-@all",
            "+@acl",
        ])
        .query::<()>(&mut con)?;
    let mut restricted = con.connection()?;
    redis::cmd("AUTH")
        .arg(&["category-user", "pass"])
        .query::<()>(&mut restricted)?;

    let assert_access =
        |con: &mut TestConnection, restricted: &mut utils::Connection| -> Result<()> {
            // Redis 7.4's ACL CAT <category> omits module commands. COMMAND INFO
            // reports their category membership on every version tested here.
            let command: Vec<Vec<Value>> = redis::cmd("COMMAND")
                .arg(&["INFO", "get_current_user"])
                .query(con)?;
            assert_eq!(command.len(), 1);
            let categories: Vec<String> = redis::from_redis_value_ref(
                command[0]
                    .get(6)
                    .context("COMMAND INFO includes ACL categories")?,
            )?;
            assert!(categories.contains(&"@acl".to_owned()), "{categories:?}");
            assert_eq!(
                redis::cmd("get_current_user").query::<String>(restricted)?,
                "category-user"
            );
            let error = redis::cmd("SET")
                .arg(&["acl-reload-denied", "value"])
                .query::<()>(&mut *restricted)
                .expect_err("the category grant allowed an unrelated SET");
            assert_eq!(error.code(), Some("NOPERM"), "{error}");
            Ok(())
        };
    assert_access(&mut con, &mut restricted)?;

    for _ in 0..2 {
        redis::cmd("MODULE")
            .arg(&["UNLOAD", "acl"])
            .query::<()>(&mut con)?;
        let categories: Vec<String> = redis::cmd("ACL").arg("CAT").query(&mut con)?;
        assert!(categories.contains(&"acl".to_owned()), "{categories:?}");
        let command: Vec<Value> = redis::cmd("COMMAND")
            .arg(&["INFO", "get_current_user"])
            .query(&mut con)?;
        assert_eq!(command, [Value::Nil]);

        redis::cmd("MODULE")
            .arg(&["LOAD", module.as_str()])
            .query::<()>(&mut con)
            .with_context(|| format!("failed to reload acl: {:?}", con.server_log()))?;
        // Reuse both the existing user grant and its authenticated connection.
        assert_access(&mut con, &mut restricted)?;
    }
    Ok(())
}

#[test]
fn test_acl_category_invalid_name_fails_module_load() -> Result<()> {
    let mut con = TestConnection::with_log_file("hello");
    if !supports_custom_acl_categories(&mut con)? {
        return Ok(());
    }
    let module = utils::module_path("acl_invalid_category")?;
    let error = redis::cmd("MODULE")
        .arg(&["LOAD", module.as_str()])
        .query::<()>(&mut con)
        .expect_err("a module declaring an invalid ACL category loaded");
    assert!(
        error.to_string().contains("Error loading the extension"),
        "{error}"
    );
    let log = con.server_log()?;
    assert!(
        log.contains("failed to add ACL category `invalid category`"),
        "{log}"
    );
    let modules: Vec<HashMap<String, Value>> = redis::cmd("MODULE").arg("LIST").query(&mut con)?;
    for module in modules {
        let name: String = redis::from_redis_value_ref(&module["name"])?;
        assert_ne!(name, "acl_invalid_category");
    }
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    Ok(())
}

#[test]
fn test_acl_command_and_channel_checks() -> Result<()> {
    let mut con = TestConnection::new("acl");

    redis::cmd("ACL")
        .arg(&["SETUSER", "bob", "on", ">pass", "~*", "&news:*", "+get"])
        .query::<()>(&mut con)?;

    let check = |con: &mut TestConnection, cmd: &str, args: &[&str]| {
        redis::cmd(cmd).arg(args).query::<String>(con)
    };

    // Commands.
    assert_eq!(
        check(
            &mut con,
            "verify_command_access_for_user",
            &["bob", "GET", "x"]
        )?,
        "OK"
    );
    let error = check(
        &mut con,
        "verify_command_access_for_user",
        &["bob", "SET", "x", "1"],
    )
    .expect_err("bob may not run SET");
    assert_eq!(error.code(), Some("NOPERM"), "{error}");
    let error = check(
        &mut con,
        "verify_command_access_for_user",
        &["bob", "NOSUCHCMD"],
    )
    .expect_err("an unknown command passed");
    assert!(error.to_string().contains("unknown command"), "{error}");
    let error = check(
        &mut con,
        "verify_command_access_for_user",
        &["nobody", "GET", "x"],
    )
    .expect_err("an unknown user passed");
    assert!(
        error
            .to_string()
            .contains("the user does not exist or is disabled"),
        "{error}"
    );

    // Channels.
    for (channel, access, allowed) in [
        ("news:1", "publish", true),
        ("news:1", "subscribe", true),
        ("news:*", "psubscribe", true),
        ("sports:1", "publish", false),
        ("*", "psubscribe", false),
    ] {
        let res = check(
            &mut con,
            "verify_channel_access_for_user",
            &["bob", channel, access],
        );
        match (res, allowed) {
            (Ok(reply), true) => assert_eq!(reply, "OK"),
            (Err(error), false) => assert_eq!(error.code(), Some("NOPERM"), "{error}"),
            (res, _) => panic!("{access} {channel}: unexpected {res:?}"),
        }
    }

    // The ACL log.
    assert_eq!(
        check(&mut con, "log_channel_denial", &["bob", "sports:1"])?,
        "OK"
    );
    let log: Vec<HashMap<String, redis::Value>> =
        redis::cmd("ACL").arg(&["LOG", "1"]).query(&mut con)?;
    let entry = &log[0];
    let text = |field: &str| redis::from_redis_value_ref::<String>(&entry[field]).unwrap();
    assert_eq!(text("username"), "bob");
    assert_eq!(text("object"), "sports:1");
    assert_eq!(text("reason"), "channel");
    assert_eq!(text("context"), "module");

    Ok(())
}

#[test]
fn test_verify_acl_on_user() -> Result<()> {
    let mut con = TestConnection::new("acl");

    let res: String = redis::cmd("verify_key_access_for_user")
        .arg(&["default", "x"])
        .query(&mut con)?;

    assert_eq!(&res, "OK");

    let res: String = redis::cmd("ACL")
        .arg(&["SETUSER", "alice", "on", ">pass", "~cached:*", "+get"])
        .query(&mut con)?;

    assert_eq!(&res, "OK");

    let res: String = redis::cmd("verify_key_access_for_user")
        .arg(&["alice", "cached:1"])
        .query(&mut con)?;

    assert_eq!(&res, "OK");

    let res: RedisResult<String> = redis::cmd("verify_key_access_for_user")
        .arg(&["alice", "not_allow"])
        .query(&mut con);

    let error = res.expect_err("alice may not access not_allow");
    assert_eq!(error.code(), Some("NOPERM"), "{error}");
    assert!(
        error.to_string().contains("permissions on the key"),
        "{error}"
    );

    Ok(())
}

#[test]
fn test_key_space_notifications() -> Result<()> {
    let mut con = TestConnection::new("events");

    let res: usize = redis::cmd("events.num_key_miss").query(&mut con)?;
    assert_eq!(res, 0);

    redis::cmd("GET").arg(&["x"]).query::<()>(&mut con)?;

    let res: usize = redis::cmd("events.num_key_miss").query(&mut con)?;
    assert_eq!(res, 1);

    let _: String = redis::cmd("SET").arg(&["x", "1"]).query(&mut con)?;

    let res: String = redis::cmd("GET").arg(&["num_sets"]).query(&mut con)?;
    assert_eq!(res, "1");

    Ok(())
}

#[test]
fn test_key_space_notification_with_invalid_utf8_event_name() -> Result<()> {
    let mut con = TestConnection::new("events");

    // Fires a keyspace event whose name contains the byte 0xFF, which is
    // never valid UTF-8. The event handler must not panic on it.
    let res: String = redis::cmd("events.send_invalid_utf8").query(&mut con)?;
    assert_eq!(res, "Event sent");

    // The handler received the event name with the invalid byte replaced
    // by U+FFFD (lossy conversion).
    let res: String = redis::cmd("events.last_generic_event").query(&mut con)?;
    assert_eq!(res, "ev\u{FFFD}nt");

    // The server survived.
    let res: String = redis::cmd("PING").query(&mut con)?;
    assert_eq!(res, "PONG");

    Ok(())
}

#[test]
fn test_context_mutex() -> Result<()> {
    let mut con = TestConnection::new("threads");

    let res: String = redis::cmd("set_static_data")
        .arg(&["foo"])
        .query(&mut con)?;
    assert_eq!(&res, "OK");

    let res: String = redis::cmd("get_static_data").query(&mut con)?;
    assert_eq!(&res, "foo");

    let res: String = redis::cmd("get_static_data_on_thread").query(&mut con)?;
    assert_eq!(&res, "foo");

    Ok(())
}

#[test]
fn test_server_event() -> Result<()> {
    let mut con = TestConnection::new("server_events");

    redis::cmd("flushall")
        .query::<()>(&mut con)
        .with_context(|| "failed to run flushall")?;

    let res: i64 = redis::cmd("num_flushed").query(&mut con)?;

    assert_eq!(res, 1);

    redis::cmd("flushall")
        .query::<()>(&mut con)
        .with_context(|| "failed to run string.set")?;

    let res: i64 = redis::cmd("num_flushed").query(&mut con)?;

    assert_eq!(res, 2);

    // The legacy and payload-aware handlers share one native subscription.
    assert_eq!(redis::cmd("num_flush_details").query::<i64>(&mut con)?, 2);

    redis::cmd("config")
        .arg(&["set", "maxmemory", "1"])
        .query::<()>(&mut con)
        .with_context(|| "failed to run string.set")?;

    let res: i64 = redis::cmd("num_max_memory_changes").query(&mut con)?;

    assert_eq!(res, 1);

    redis::cmd("config")
        .arg(&["set", "maxmemory", "0"])
        .query::<()>(&mut con)
        .with_context(|| "failed to run string.set")?;

    let res: i64 = redis::cmd("num_max_memory_changes").query(&mut con)?;

    assert_eq!(res, 2);

    let res: i64 = redis::cmd("num_crons").query(&mut con)?;

    assert!(res > 0);

    Ok(())
}

#[test]
fn test_server_event_flush_payloads() -> Result<()> {
    // This module has only payload-aware flush handlers, so it also verifies
    // registration when the legacy distributed slice is empty.
    let mut con = TestConnection::new("server_event_payloads");
    for mode in ["SYNC", "ASYNC"] {
        redis::cmd("SELECT").arg(4).query::<()>(&mut con)?;
        redis::cmd("MSET")
            .arg(&["a", "1", "b", "2"])
            .query::<()>(&mut con)?;
        redis::cmd("FLUSHDB").arg(mode).query::<()>(&mut con)?;
        let rows: Vec<Vec<i64>> = redis::cmd("event_payloads.drain")
            .arg("flush")
            .query(&mut con)?;
        assert_eq!(rows, vec![vec![0, 4, 4, 2], vec![1, 4, 4, 0]]);

        redis::cmd("SET")
            .arg(&["another", "db"])
            .query::<()>(&mut con)?;
        redis::cmd("SELECT").arg(0).query::<()>(&mut con)?;
        redis::cmd("SET")
            .arg(&["main", "db"])
            .query::<()>(&mut con)?;
        redis::cmd("SELECT").arg(3).query::<()>(&mut con)?;
        redis::cmd("FLUSHALL").arg(mode).query::<()>(&mut con)?;
        let rows: Vec<Vec<i64>> = redis::cmd("event_payloads.drain")
            .arg("flush")
            .query(&mut con)?;
        assert_eq!(rows, vec![vec![0, -1, 0, 1], vec![1, -1, 0, 0]]);
    }
    Ok(())
}

#[test]
fn test_server_event_swapdb_payloads() -> Result<()> {
    let mut con = TestConnection::new("server_event_payloads");
    redis::cmd("SELECT").arg(2).query::<()>(&mut con)?;
    redis::cmd("SET")
        .arg(&["first", "2"])
        .query::<()>(&mut con)?;
    redis::cmd("SELECT").arg(5).query::<()>(&mut con)?;
    redis::cmd("MSET")
        .arg(&["second", "5", "extra", "5"])
        .query::<()>(&mut con)?;
    redis::cmd("SELECT").arg(1).query::<()>(&mut con)?;
    redis::cmd("SWAPDB").arg(&[5, 2]).query::<()>(&mut con)?;
    let rows: Vec<Vec<i64>> = redis::cmd("event_payloads.drain")
        .arg("swap")
        .query(&mut con)?;
    // Command order is preserved, and the callback sees data after the swap.
    assert_eq!(rows, vec![vec![5, 2, 1, 2]]);
    assert!(
        redis::cmd("SWAPDB")
            .arg(&[5, -1])
            .query::<()>(&mut con)
            .is_err()
    );
    let rows: Vec<Vec<i64>> = redis::cmd("event_payloads.drain")
        .arg("swap")
        .query(&mut con)?;
    assert!(rows.is_empty(), "failed swaps must not invoke the handler");
    Ok(())
}

#[test]
fn test_server_event_payload_panics_and_unload() -> Result<()> {
    let mut con = TestConnection::new("server_event_payloads");
    redis::cmd("event_payloads.panics")
        .arg("yes")
        .query::<()>(&mut con)?;
    redis::cmd("SELECT").arg(2).query::<()>(&mut con)?;
    redis::cmd("FLUSHDB").arg("SYNC").query::<()>(&mut con)?;
    redis::cmd("SWAPDB").arg(&[2, 5]).query::<()>(&mut con)?;
    // Both handlers panic after recording. All must run regardless of linkme
    // ordering, which proves there is a separate panic boundary per handler.
    let counts: Vec<i64> = redis::cmd("event_payloads.panic_counts").query(&mut con)?;
    assert_eq!(counts, vec![2, 1]);
    for (event, expected) in [("flush", 2), ("swap", 1)] {
        let rows: Vec<Vec<i64>> = redis::cmd("event_payloads.drain")
            .arg(event)
            .query(&mut con)?;
        assert_eq!(rows.len(), expected);
    }
    let log = con.server_log()?;
    assert!(log.contains("flush info event handler panicked"));
    assert!(log.contains("swapdb event handler panicked"));
    redis::cmd("MODULE")
        .arg(&["UNLOAD", "server_event_payloads"])
        .query::<()>(&mut con)?;
    redis::cmd("FLUSHALL").arg("SYNC").query::<()>(&mut con)?;
    redis::cmd("SWAPDB").arg(&[2, 5]).query::<()>(&mut con)?;
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    Ok(())
}

#[test]
fn test_cron_event_hz() -> Result<()> {
    let mut con = TestConnection::new("server_events");

    redis::cmd("CONFIG")
        .arg(&["SET", "hz", "50"])
        .query::<()>(&mut con)?;
    // At 50 Hz a cron event follows within 20 ms; allow a second.
    utils::wait_until(Duration::from_secs(1), "a 50 Hz cron event", || {
        Ok(redis::cmd("last_cron_hz").query::<i64>(&mut con)? == 50)
    })?;

    Ok(())
}

#[test]
fn test_shutdown_event() -> Result<()> {
    let mut con = TestConnection::with_log_file("server_events");

    // The server exits, so the reply is a connection error.
    let _ = redis::cmd("SHUTDOWN").arg("NOSAVE").query::<()>(&mut con);
    let log = con.server_log()?;
    assert!(
        log.contains("server_events: shutdown event received"),
        "{log}"
    );

    Ok(())
}

#[test]
fn test_configuration() -> Result<()> {
    let mut con = TestConnection::new("configuration");

    let config_get = |con: &mut TestConnection, config: &str| -> Result<String> {
        let res: Vec<String> = redis::cmd("config")
            .arg(&["get", config])
            .query(con)
            .with_context(|| "failed to run flushall")?;
        Ok(res[1].clone())
    };

    let config_set = |con: &mut TestConnection, config: &str, val: &str| -> Result<()> {
        let res: String = redis::cmd("config")
            .arg(&["set", config, val])
            .query(con)
            .with_context(|| "failed to run flushall")?;
        assert_eq!(res, "OK");
        Ok(())
    };

    assert_eq!(config_get(&mut con, "configuration.i64")?, "10");
    config_set(&mut con, "configuration.i64", "100")?;
    assert_eq!(config_get(&mut con, "configuration.i64")?, "100");

    assert_eq!(config_get(&mut con, "configuration.atomic_i64")?, "10");
    config_set(&mut con, "configuration.atomic_i64", "100")?;
    assert_eq!(config_get(&mut con, "configuration.atomic_i64")?, "100");

    assert_eq!(config_get(&mut con, "configuration.bytes")?, "default");
    config_set(&mut con, "configuration.bytes", "new")?;
    assert_eq!(config_get(&mut con, "configuration.bytes")?, "new");

    assert_eq!(config_get(&mut con, "configuration.string")?, "default");
    config_set(&mut con, "configuration.string", "new")?;
    assert_eq!(config_get(&mut con, "configuration.string")?, "new");

    assert_eq!(
        config_get(&mut con, "configuration.mutex_string")?,
        "default"
    );
    config_set(&mut con, "configuration.mutex_string", "new")?;
    assert_eq!(config_get(&mut con, "configuration.mutex_string")?, "new");

    assert_eq!(config_get(&mut con, "configuration.atomic_bool")?, "yes");
    config_set(&mut con, "configuration.atomic_bool", "no")?;
    assert_eq!(config_get(&mut con, "configuration.atomic_bool")?, "no");

    assert_eq!(config_get(&mut con, "configuration.bool")?, "yes");
    config_set(&mut con, "configuration.bool", "no")?;
    assert_eq!(config_get(&mut con, "configuration.bool")?, "no");

    assert_eq!(config_get(&mut con, "configuration.enum")?, "Val1");
    config_set(&mut con, "configuration.enum", "Val2")?;
    assert_eq!(config_get(&mut con, "configuration.enum")?, "Val2");

    assert_eq!(config_get(&mut con, "configuration.enum_mutex")?, "Val1");
    config_set(&mut con, "configuration.enum_mutex", "Val2")?;
    assert_eq!(config_get(&mut con, "configuration.enum_mutex")?, "Val2");

    let res: i64 = redis::cmd("configuration.num_changes")
        .query(&mut con)
        .with_context(|| "failed to run flushall")?;
    assert_eq!(res, 18); // the first configuration initialisation is counted as well, so we will get 18 changes.

    Ok(())
}

#[test]
fn test_string_configuration_getter_releases_replies() -> Result<()> {
    let mut con = TestConnection::new("configuration_getter");
    redis::cmd("CONFIG")
        .arg(&["SET", "configuration_getter.text", "value"])
        .query::<()>(&mut con)?;
    for _ in 0..100 {
        let reply: Vec<String> = redis::cmd("CONFIG")
            .arg(&["GET", "configuration_getter.text"])
            .query(&mut con)
            .with_context(|| "failed to run CONFIG GET")?;
        assert_eq!(reply, ["configuration_getter.text", "value"]);
    }

    // Each get replaces and frees the previous reply, so only the last one
    // stays allocated.
    let counts: Vec<i64> = redis::cmd("configuration_getter.strings").query(&mut con)?;
    let (returned, live) = (counts[0], counts[1]);
    assert!(returned >= 100, "{returned} getter strings returned");
    assert!(
        live <= 1,
        "{live} of {returned} getter strings still allocated"
    );

    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    Ok(())
}

#[test]
fn test_string_configuration_unload_releases_final_cached_reply() -> Result<()> {
    let mut con = TestConnection::new("configuration_getter");
    redis::cmd("MODULE")
        .arg(&["UNLOAD", "configuration_getter"])
        .query::<()>(&mut con)?;
    let marker = std::env::temp_dir().join(format!(
        "sdk-config-cache-{}-{}",
        std::process::id(),
        con.port()
    ));
    let _ = std::fs::remove_file(&marker);
    redis::cmd("MODULE")
        .arg("LOAD")
        .arg(utils::module_path("configuration_getter")?)
        .arg(marker.to_string_lossy().as_ref())
        .query::<()>(&mut con)?;
    for _ in 0..3 {
        let values: Vec<String> = redis::cmd("CONFIG")
            .arg(&["GET", "configuration_getter.text"])
            .query(&mut con)?;
        assert_eq!(values, ["configuration_getter.text", "default"]);
    }
    let counts: Vec<i64> = redis::cmd("configuration_getter.strings").query(&mut con)?;
    assert_eq!(
        counts[1], 1,
        "the last getter reply must stay alive through native consumption"
    );
    let before = std::fs::read(&marker)?.len();
    redis::cmd("MODULE")
        .arg(&["UNLOAD", "configuration_getter"])
        .query::<()>(&mut con)?;
    assert_eq!(
        std::fs::read(&marker)?.len(),
        before + 1,
        "successful unload must release the final cached native string"
    );
    std::fs::remove_file(marker)?;
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    Ok(())
}

#[test]
fn test_memory_configuration_takes_units() -> Result<()> {
    let mut con = TestConnection::new("configuration");
    let memory = |con: &mut TestConnection| -> Result<String> {
        let res: Vec<String> = redis::cmd("CONFIG")
            .arg(&["GET", "configuration.memory"])
            .query(con)?;
        Ok(res[1].clone())
    };
    assert_eq!(memory(&mut con)?, "1024");
    let _: () = redis::cmd("CONFIG")
        .arg(&["SET", "configuration.memory", "2kb"])
        .query(&mut con)?;
    assert_eq!(memory(&mut con)?, "2048");

    // Loaded again with module arguments, which take units too.
    let _: () = redis::cmd("MODULE")
        .arg(&["UNLOAD", "configuration"])
        .query(&mut con)?;
    let module = utils::module_path("configuration")?;
    let _: () = redis::cmd("MODULE")
        .arg(&["LOAD", module.as_str(), "memory", "8mb"])
        .query(&mut con)?;
    assert_eq!(memory(&mut con)?, "8388608");
    Ok(())
}

fn assert_configuration_module_args(args: &[&str], expected: &[(&str, &str)]) -> Result<()> {
    let mut con = TestConnection::new("hello");
    let module = utils::module_path("configuration")?;
    redis::cmd("MODULE")
        .arg(&["LOAD", module.as_str()])
        .arg(args)
        .query::<()>(&mut con)
        .with_context(|| format!("failed to load configuration with {args:?}"))?;
    for (name, value) in expected {
        let name = format!("configuration.{name}");
        let config: Vec<String> = redis::cmd("CONFIG")
            .arg(&["GET", name.as_str()])
            .query(&mut con)?;
        assert_eq!(config, [name, (*value).to_owned()], "{args:?}");
    }
    Ok(())
}

fn assert_configuration_module_arg_rejected(
    name: &str,
    value: &str,
    diagnostic: &[&str],
) -> Result<()> {
    let mut con = TestConnection::with_log_file("hello");
    let module = utils::module_path("configuration")?;
    let error = redis::cmd("MODULE")
        .arg(&["LOAD", module.as_str(), name, value])
        .query::<()>(&mut con)
        .expect_err("an invalid module argument loaded");
    assert!(
        error.to_string().contains("Error loading the extension"),
        "{name} {value}: {error}"
    );
    let log = con.server_log()?;
    assert!(
        !log.contains("configuration init:"),
        "init ran for a failed load: {log}"
    );
    assert!(
        log.lines().any(|line| {
            line.contains(name) && diagnostic.iter().all(|part| line.contains(part))
        }),
        "missing diagnostic for {name} {value}: {log}"
    );
    let configs: Vec<String> = redis::cmd("CONFIG")
        .arg(&["GET", "configuration.*"])
        .query(&mut con)?;
    assert!(configs.is_empty(), "{configs:?}");
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");

    // A rejected load must leave the server able to load the same module.
    redis::cmd("MODULE")
        .arg(&["LOAD", module.as_str()])
        .query::<()>(&mut con)?;
    let config: Vec<String> = redis::cmd("CONFIG")
        .arg(&["GET", "configuration.i64"])
        .query(&mut con)?;
    assert_eq!(config, ["configuration.i64", "10"]);
    assert!(
        con.server_log()?.contains("configuration init: i64 is 10"),
        "init did not run after the valid load"
    );
    Ok(())
}

#[test]
fn test_configuration_string_storage_utf8_and_binary_values() -> Result<()> {
    let mut con = TestConnection::new("configuration");
    let bytes = b"binary\xff\0value";
    redis::cmd("CONFIG")
        .arg(&["SET", "configuration.bytes"])
        .arg(bytes.as_slice())
        .query::<()>(&mut con)?;
    let reply: HashMap<String, Vec<u8>> = redis::cmd("CONFIG")
        .arg(&["GET", "configuration.bytes"])
        .query(&mut con)?;
    assert_eq!(reply["configuration.bytes"], bytes);
    for name in ["configuration.string", "configuration.mutex_string"] {
        assert!(
            redis::cmd("CONFIG")
                .arg(&["SET", name])
                .arg(bytes.as_slice())
                .query::<()>(&mut con)
                .is_err()
        );
        let reply: HashMap<String, String> =
            redis::cmd("CONFIG").arg(&["GET", name]).query(&mut con)?;
        assert_eq!(reply[name], "default");
    }
    Ok(())
}

#[test]
fn test_binary_configuration_storage_from_clients_workers_and_reload() -> Result<()> {
    let mut con = TestConnection::new("configuration_observers");
    let names = [
        "configuration_observers.text",
        "configuration_observers.binary",
    ];
    let payloads = [
        Vec::new(),
        b"nul\0invalid\xffutf8".to_vec(),
        "café\0".as_bytes().to_vec(),
        (0..=255).cycle().take(32768).collect::<Vec<u8>>(),
    ];
    for payload in payloads {
        for name in names {
            redis::cmd("CONFIG")
                .arg(&["SET", name])
                .arg(b"client seed".as_slice())
                .query::<()>(&mut con)?;
        }
        redis::cmd("config_observers.worker")
            .arg(&payload)
            .query::<()>(&mut con)?;
        utils::wait_until(
            Duration::from_secs(5),
            "binary configuration worker exit",
            || Ok(redis::cmd("config_observers.finish").query::<i64>(&mut con)? == 1),
        )?;
        for _ in 0..3 {
            let values: HashMap<String, Vec<u8>> = redis::cmd("CONFIG")
                .arg("GET")
                .arg(&names)
                .query(&mut con)?;
            for name in names {
                assert_eq!(values[name], payload, "worker {name}");
            }
        }
        for name in names {
            redis::cmd("CONFIG")
                .arg(&["SET", name])
                .arg(b"different".as_slice())
                .query::<()>(&mut con)?;
            redis::cmd("CONFIG")
                .arg(&["SET", name])
                .arg(&payload)
                .query::<()>(&mut con)?;
        }
        let values: HashMap<String, Vec<u8>> = redis::cmd("CONFIG")
            .arg("GET")
            .arg(&names)
            .query(&mut con)?;
        for name in names {
            assert_eq!(values[name], payload, "client {name}");
        }
    }
    redis::cmd("MODULE")
        .arg(&["UNLOAD", "configuration_observers"])
        .query::<()>(&mut con)?;
    redis::cmd("MODULE")
        .arg("LOAD")
        .arg(utils::module_path("configuration_observers")?)
        .query::<()>(&mut con)?;
    let values: HashMap<String, Vec<u8>> = redis::cmd("CONFIG")
        .arg("GET")
        .arg(&names)
        .query(&mut con)?;
    assert_eq!(values[names[0]], b"");
    assert_eq!(values[names[1]], b"seed");
    redis::cmd("MODULE")
        .arg(&["UNLOAD", "configuration_observers"])
        .query::<()>(&mut con)?;
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    Ok(())
}

#[test]
fn test_configuration_loadex_precedence_and_registration_validation() -> Result<()> {
    let mut con = TestConnection::with_log_file("hello");
    let module = utils::module_path("configuration")?;
    // Module arguments become registration defaults and must pass SDK bounds
    // validation even when an explicit Redis configuration would replace them.
    assert!(
        redis::cmd("MODULE")
            .arg(&[
                "LOADEX",
                &module,
                "CONFIG",
                "configuration.i64",
                "500",
                "ARGS",
                "i64",
                "1001"
            ])
            .query::<()>(&mut con)
            .is_err()
    );
    assert!(!con.server_log()?.contains("configuration init:"));
    let configs: Vec<String> = redis::cmd("CONFIG")
        .arg(&["GET", "configuration.*"])
        .query(&mut con)?;
    assert!(configs.is_empty());

    redis::cmd("MODULE")
        .arg(&[
            "LOADEX",
            &module,
            "CONFIG",
            "configuration.i64",
            "500",
            "CONFIG",
            "configuration.bool",
            "yes",
            "CONFIG",
            "configuration.memory",
            "1kb",
            "ARGS",
            "i64",
            "25",
            "bool",
            "NO",
            "memory",
            "2kb",
            "string",
            "from-args",
        ])
        .query::<()>(&mut con)?;
    let configs: HashMap<String, String> = redis::cmd("CONFIG")
        .arg(&["GET", "configuration.*"])
        .query(&mut con)?;
    for (name, expected) in [
        ("i64", "500"),
        ("bool", "yes"),
        ("memory", "1024"),
        ("string", "from-args"),
        ("atomic_i64", "10"),
    ] {
        assert_eq!(
            configs[&format!("configuration.{name}")],
            expected,
            "{name}"
        );
    }
    assert!(con.server_log()?.contains("configuration init: i64 is 500"));
    Ok(())
}

#[test]
fn test_configuration_module_args_validate_numeric_range() -> Result<()> {
    for value in ["-1", "1001"] {
        assert_configuration_module_arg_rejected("i64", value, &["0", "1000"])?;
    }
    for value in ["0", "1000"] {
        assert_configuration_module_args(&["i64", value], &[("i64", value)])?;
    }
    Ok(())
}

#[test]
fn test_configuration_module_args_bool_is_case_insensitive() -> Result<()> {
    for (input, expected) in [
        ("YES", "yes"),
        ("Yes", "yes"),
        ("yes", "yes"),
        ("NO", "no"),
        ("No", "no"),
        ("no", "no"),
    ] {
        assert_configuration_module_args(&["bool", input], &[("bool", expected)])?;
    }
    Ok(())
}

#[test]
fn test_configuration_module_args_reject_invalid_bool() -> Result<()> {
    for value in ["true", "1", "nope", ""] {
        assert_configuration_module_arg_rejected("bool", value, &["yes", "no"])?;
    }
    Ok(())
}

#[test]
fn test_configuration_module_args_keep_names_and_values_paired() -> Result<()> {
    // A string value can equal a different configuration's name. It must not
    // override that configuration or hide a later explicit value for it.
    assert_configuration_module_args(
        &["string", "i64", "bool", "no"],
        &[("string", "i64"), ("i64", "10"), ("bool", "no")],
    )?;
    assert_configuration_module_args(
        &["string", "i64", "i64", "25"],
        &[("string", "i64"), ("i64", "25")],
    )?;
    // Lookup is first-match and does not validate the complete argument list.
    assert_configuration_module_args(&["i64", "25", "i64", "30"], &[("i64", "25")])?;
    assert_configuration_module_args(&["unknown", "ignored", "i64"], &[("i64", "10")])?;
    Ok(())
}

#[test]
fn test_configuration_module_args_validate_memory_range_after_units() -> Result<()> {
    // The example accepts 1024 through 8 MiB. Decimal 1k is below the
    // minimum, while binary 1kb is exactly the minimum.
    for input in ["1k", "9mb"] {
        assert_configuration_module_arg_rejected("memory", input, &["1024", "8388608"])?;
    }
    for (input, bytes) in [("1kb", "1024"), ("8mb", "8388608")] {
        assert_configuration_module_args(&["memory", input], &[("memory", bytes)])?;
    }
    Ok(())
}

#[test]
fn test_configuration_registration_errors_fail_the_load() -> Result<()> {
    let mut con = TestConnection::new("hello");
    let module = utils::module_path("configuration_registration")?;

    for kind in ["i64", "string", "bool", "enum"] {
        let error = redis::cmd("MODULE")
            .arg(&["LOAD", module.as_str(), kind])
            .query::<String>(&mut con)
            .expect_err("a duplicate configuration loaded");
        assert!(
            error.to_string().contains("Error loading the extension"),
            "duplicate {kind}: {error}"
        );
        let configs: Vec<String> = redis::cmd("CONFIG")
            .arg(&["GET", "configuration_registration.*"])
            .query(&mut con)?;
        assert!(configs.is_empty(), "{configs:?}");
        assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    }

    redis::cmd("MODULE")
        .arg(&["LOAD", module.as_str(), "valid"])
        .query::<()>(&mut con)
        .with_context(|| "failed to load configuration_registration")?;
    let configs: HashMap<String, String> = redis::cmd("CONFIG")
        .arg(&["GET", "configuration_registration.*"])
        .query(&mut con)?;
    let expected: HashMap<String, String> = [
        ("i64", "7"),
        ("string", "default"),
        ("bool", "yes"),
        ("enum", "One"),
    ]
    .into_iter()
    .map(|(name, value)| {
        (
            format!("configuration_registration.{name}"),
            value.to_owned(),
        )
    })
    .collect();
    assert_eq!(configs, expected);
    redis::cmd("MODULE")
        .arg(&["UNLOAD", "configuration_registration"])
        .query::<()>(&mut con)?;

    Ok(())
}

#[test]
fn test_configuration_observers_follow_client_and_worker_setter_threads() -> Result<()> {
    fn observed(con: &mut TestConnection) -> Result<Vec<(String, i64, bool)>> {
        let rows: Vec<Vec<Value>> = redis::cmd("config_observers.observed").query(con)?;
        rows.iter()
            .map(|row| {
                anyhow::ensure!(row.len() == 3, "observer name, count and thread flag");
                let name: String = redis::from_redis_value_ref(&row[0])?;
                // Redis 8.0 passes the module-prefixed name to enum setters.
                // This test identifies the setting; it does not constrain the
                // native callback's naming convention.
                let name = if name == "configuration_observers.choice" {
                    "choice".to_owned()
                } else {
                    name
                };
                Ok((
                    name,
                    redis::from_redis_value_ref(&row[1])?,
                    redis::from_redis_value_ref::<i64>(&row[2])? != 0,
                ))
            })
            .collect()
    }

    fn client_update(con: &mut TestConnection) -> Result<()> {
        redis::cmd("CONFIG")
            .arg(&[
                "SET",
                "configuration_observers.number",
                "7",
                "configuration_observers.text",
                "client",
                "configuration_observers.toggle",
                "yes",
                "configuration_observers.choice",
                "Two",
            ])
            .query::<()>(con)?;
        Ok(())
    }

    let mut con = TestConnection::new("configuration_observers");
    assert!(observed(&mut con)?.is_empty());
    client_update(&mut con)?;
    let first = observed(&mut con)?;
    assert_eq!(first.len(), 4);
    assert_eq!(
        redis::cmd("config_observers.shared_calls").query::<i64>(&mut con)?,
        4
    );
    let counts: HashMap<_, _> = first
        .into_iter()
        .map(|(name, count, main_thread)| {
            assert!(main_thread, "client update runs on the registration thread");
            (name, count)
        })
        .collect();
    assert_eq!(counts.len(), 4);
    for name in ["number", "text", "toggle", "choice"] {
        assert!(counts.contains_key(name), "missing {name} observer");
    }

    assert_eq!(
        redis::cmd("config_observers.worker").query::<String>(&mut con)?,
        "OK"
    );
    utils::wait_until(
        Duration::from_secs(5),
        "configuration worker to exit",
        || Ok(redis::cmd("config_observers.finish").query::<i64>(&mut con)? == 1),
    )?;
    let after_worker = observed(&mut con)?;
    assert_eq!(after_worker.len(), 8);
    assert_eq!(
        redis::cmd("config_observers.shared_calls").query::<i64>(&mut con)?,
        8
    );
    for (name, count, main_thread) in &after_worker[4..] {
        assert!(
            !main_thread,
            "worker CONFIG SET invokes {name} on the worker"
        );
        assert_eq!(
            *count,
            counts[name] + 1,
            "captured Cell persists for {name}"
        );
    }
    let configs: HashMap<String, String> = redis::cmd("CONFIG")
        .arg(&["GET", "configuration_observers.*"])
        .query(&mut con)?;
    for (name, value) in [
        ("number", "11"),
        ("text", "worker"),
        ("toggle", "no"),
        ("choice", "One"),
    ] {
        assert_eq!(configs[&format!("configuration_observers.{name}")], value);
    }
    assert!(
        redis::cmd("CONFIG")
            .arg(&["SET", "configuration_observers.number", "101"])
            .query::<()>(&mut con)
            .is_err()
    );
    assert_eq!(
        observed(&mut con)?,
        after_worker,
        "rejected setter emits no observer call"
    );

    client_update(&mut con)?;
    let after_client = observed(&mut con)?;
    assert_eq!(after_client.len(), 12);
    assert_eq!(
        redis::cmd("config_observers.shared_calls").query::<i64>(&mut con)?,
        12
    );
    for (name, count, main_thread) in &after_client[8..] {
        assert!(
            main_thread,
            "later client update returns {name} to the main thread"
        );
        assert_eq!(*count, counts[name] + 2);
    }
    redis::cmd("MODULE")
        .arg(&["UNLOAD", "configuration_observers"])
        .query::<()>(&mut con)?;
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    Ok(())
}

mod parsed_configuration_tests {
    use super::*;

    fn value(con: &mut TestConnection) -> Result<(String, i64)> {
        Ok(redis::cmd("parsed_config.value").query(con)?)
    }

    fn config(con: &mut TestConnection, name: &str) -> Result<String> {
        let values: HashMap<String, String> = redis::cmd("CONFIG")
            .arg(&["GET", &format!("parsed_configuration.{name}")])
            .query(con)?;
        Ok(values[&format!("parsed_configuration.{name}")].clone())
    }

    #[test]
    fn test_parsed_configuration_multi_set_rollback_is_visible_to_observers() -> Result<()> {
        let mut con = TestConnection::new("parsed_configuration");
        let observations = |con: &mut TestConnection| -> Result<Vec<Vec<Value>>> {
            Ok(redis::cmd("parsed_config.observed").query(con)?)
        };
        assert!(observations(&mut con)?.is_empty());
        assert!(
            redis::cmd("CONFIG")
                .arg(&[
                    "SET",
                    "parsed_configuration.workers",
                    "8",
                    "parsed_configuration.prefix",
                    "invalid"
                ])
                .query::<()>(&mut con)
                .is_err()
        );
        assert_eq!(value(&mut con)?, ("4".into(), 4));
        assert_eq!(config(&mut con, "workers")?, "4");
        assert_eq!(config(&mut con, "prefix")?, "items:");
        let seen = observations(&mut con)?;
        assert!(
            seen.contains(&vec![Value::BulkString(b"8".to_vec()), Value::Int(8)]),
            "an accepted intermediate value must be observable: {seen:?}"
        );
        assert_eq!(
            seen.last(),
            Some(&vec![Value::BulkString(b"4".to_vec()), Value::Int(4)]),
            "rollback must restore the old raw/parsed pair"
        );
        // The callback history is not rolled back with the stored values.
        assert!(seen.len() >= 2);
        Ok(())
    }

    #[test]
    fn test_parsed_configuration_updates_keep_raw_parsed_and_observers_consistent() -> Result<()> {
        let mut con = TestConnection::new("parsed_configuration");
        assert_eq!(value(&mut con)?, ("4".into(), 4));
        assert_eq!(config(&mut con, "workers")?, "4");
        assert_eq!(
            redis::cmd("parsed_config.prefix").query::<String>(&mut con)?,
            "items:"
        );
        redis::cmd("CONFIG")
            .arg(&["SET", "parsed_configuration.workers", " 008 "])
            .query::<()>(&mut con)?;
        assert_eq!(value(&mut con)?, (" 008 ".into(), 8));
        assert_eq!(config(&mut con, "workers")?, " 008 ");
        for bad in ["", "0", "65", "65536", "bad"] {
            let error = redis::cmd("CONFIG")
                .arg(&["SET", "parsed_configuration.workers", bad])
                .query::<()>(&mut con)
                .unwrap_err();
            assert!(
                error.to_string().contains("workers must be 1..=64"),
                "{error}"
            );
            assert_eq!(value(&mut con)?, (" 008 ".into(), 8));
            assert_eq!(config(&mut con, "workers")?, " 008 ");
        }
        assert!(
            redis::cmd("CONFIG")
                .arg(&["SET", "parsed_configuration.workers"])
                .arg(&b"\xff"[..])
                .query::<()>(&mut con)
                .is_err()
        );
        assert_eq!(value(&mut con)?, (" 008 ".into(), 8));
        let observed: Vec<Value> = redis::cmd("parsed_config.observed").query(&mut con)?;
        assert!(!observed.is_empty());
        for pair in observed {
            let pair: (String, i64) = redis::from_redis_value_ref(&pair)?;
            assert_eq!(pair, (" 008 ".into(), 8));
        }
        redis::cmd("CONFIG")
            .arg(&["SET", "parsed_configuration.prefix", "café:"])
            .query::<()>(&mut con)?;
        assert_eq!(config(&mut con, "prefix")?, "café:");
        assert_eq!(
            redis::cmd("parsed_config.prefix").query::<String>(&mut con)?,
            "café:"
        );
        for bad in ["", "missing-colon", "nul\0:"] {
            assert!(
                redis::cmd("CONFIG")
                    .arg(&["SET", "parsed_configuration.prefix", bad])
                    .query::<()>(&mut con)
                    .is_err()
            );
            assert_eq!(config(&mut con, "prefix")?, "café:");
        }
        Ok(())
    }

    #[test]
    fn test_parsed_configuration_module_arguments_are_validated_before_init() -> Result<()> {
        let mut con = TestConnection::with_args(
            "parsed_configuration",
            &["workers", " 012 ", "prefix", "custom:"],
        );
        assert_eq!(value(&mut con)?, (" 012 ".into(), 12));
        assert_eq!(config(&mut con, "workers")?, " 012 ");
        assert_eq!(config(&mut con, "prefix")?, "custom:");
        for args in [["workers", "0"], ["prefix", "bad"]] {
            let error = match TestConnection::try_with_args("parsed_configuration", &args) {
                Ok(_) => anyhow::bail!("invalid module argument loaded: {args:?}"),
                Err(error) => format!("{error:#}"),
            };
            assert!(
                error.contains("Redis rejected a module configuration value"),
                "{error}"
            );
        }
        Ok(())
    }

    #[test]
    fn test_parsed_configuration_failed_load_and_in_place_reload() -> Result<()> {
        let mut con = TestConnection::new("parsed_configuration");
        let module = utils::module_path("parsed_configuration")?;
        redis::cmd("CONFIG")
            .arg(&["SET", "parsed_configuration.workers", "9"])
            .query::<()>(&mut con)?;
        redis::cmd("MODULE")
            .arg(&["UNLOAD", "parsed_configuration"])
            .query::<()>(&mut con)?;
        assert!(
            redis::cmd("MODULE")
                .arg(&["LOAD", &module, "workers", "0"])
                .query::<()>(&mut con)
                .is_err()
        );
        assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
        redis::cmd("MODULE")
            .arg(&["LOAD", &module])
            .query::<()>(&mut con)?;
        assert_eq!(value(&mut con)?, ("4".into(), 4));
        assert_eq!(config(&mut con, "prefix")?, "items:");
        redis::cmd("MODULE")
            .arg(&["UNLOAD", "parsed_configuration"])
            .query::<()>(&mut con)?;
        redis::cmd("MODULE")
            .arg(&["LOAD", &module, "workers", "6"])
            .query::<()>(&mut con)?;
        assert_eq!(value(&mut con)?, ("6".into(), 6));
        Ok(())
    }
}

#[test]
fn test_configuration_load_errors_fail_the_load() -> Result<()> {
    let mut con = TestConnection::with_log_file("hello");
    let module = utils::module_path("configuration")?;

    // configuration.i64 accepts 0 to 1000.
    let error = redis::cmd("MODULE")
        .arg(&["LOADEX", &module, "CONFIG", "configuration.i64", "5000"])
        .query::<()>(&mut con)
        .expect_err("an out of range configuration value loaded");
    assert!(
        error.to_string().contains("Error loading the extension"),
        "{error}"
    );
    let log = con.server_log()?;
    assert!(
        !log.contains("configuration init:"),
        "init ran for a failed load: {log}"
    );
    assert!(
        log.contains("Redis rejected a module configuration value"),
        "{log}"
    );

    redis::cmd("MODULE")
        .arg(&["LOADEX", &module, "CONFIG", "configuration.i64", "500"])
        .query::<()>(&mut con)
        .with_context(|| "failed to load configuration")?;
    let config: Vec<String> = redis::cmd("CONFIG")
        .arg(&["GET", "configuration.i64"])
        .query(&mut con)?;
    assert_eq!(config, ["configuration.i64", "500"]);
    let log = con.server_log()?;
    assert!(log.contains("configuration init: i64 is 500"), "{log}");

    Ok(())
}

#[test]
fn test_response() -> Result<()> {
    let mut con = TestConnection::new("response");

    redis::cmd("hset")
        .arg(&["k", "a", "b", "c", "d", "e", "b", "f", "g"])
        .query::<()>(&mut con)
        .with_context(|| "failed to run string.set")?;

    let mut res: Vec<String> = redis::cmd("map.mget")
        .arg(&["k", "a", "c", "e"])
        .query(&mut con)
        .with_context(|| "failed to run string.set")?;

    res.sort();
    assert_eq!(&res, &["a", "b", "b", "c", "d", "e"]);

    let mut res: Vec<String> = redis::cmd("map.unique")
        .arg(&["k", "a", "c", "e"])
        .query(&mut con)
        .with_context(|| "failed to run string.set")?;

    res.sort();
    assert_eq!(&res, &["b", "d"]);

    // hash_get_by_string: present field, absent field, absent key.
    let res: String = redis::cmd("map.hget")
        .arg(&["k", "c"])
        .query(&mut con)
        .with_context(|| "failed to run map.hget")?;
    assert_eq!(res, "d");

    let res: Option<String> = redis::cmd("map.hget")
        .arg(&["k", "nosuchfield"])
        .query(&mut con)
        .with_context(|| "failed to run map.hget")?;
    assert_eq!(res, None);

    let res: Option<String> = redis::cmd("map.hget")
        .arg(&["nosuchkey", "c"])
        .query(&mut con)
        .with_context(|| "failed to run map.hget")?;
    assert_eq!(res, None);

    // A key of another type is an error, not a missing field.
    redis::cmd("set")
        .arg(&["s", "v"])
        .query::<()>(&mut con)
        .with_context(|| "failed to run set")?;
    for cmd in ["map.hget", "map.hget_writable", "map.mget"] {
        let err = redis::cmd(cmd)
            .arg(&["s", "c"])
            .query::<redis::Value>(&mut con)
            .expect_err("hash lookup on a string key must fail");
        assert_eq!(
            err.code(),
            Some("WRONGTYPE"),
            "{cmd}: unexpected error: {err}"
        );
    }

    // The writable handle resolves the same way.
    let res: String = redis::cmd("map.hget_writable")
        .arg(&["k", "c"])
        .query(&mut con)
        .with_context(|| "failed to run map.hget_writable")?;
    assert_eq!(res, "d");

    let res: Option<String> = redis::cmd("map.hget_writable")
        .arg(&["k", "nosuchfield"])
        .query(&mut con)
        .with_context(|| "failed to run map.hget_writable")?;
    assert_eq!(res, None);

    Ok(())
}

#[test]
fn test_command_proc_macro() -> Result<()> {
    let mut con = TestConnection::new("proc_macro_commands");

    let info: Vec<Value> = redis::cmd("COMMAND")
        .arg(&[
            "INFO",
            "redis_value_derive",
            "classic_keys",
            "keyword_keys",
            "num_keys",
        ])
        .query(&mut con)?;
    assert_eq!(info.len(), 4);
    for (entry, (name, arity)) in info.iter().zip([
        ("redis_value_derive", -1),
        ("classic_keys", -2),
        ("keyword_keys", -2),
        ("num_keys", -2),
    ]) {
        let fields = entry.as_sequence().expect("COMMAND INFO entry");
        assert_eq!(redis::from_redis_value_ref::<String>(&fields[0])?, name);
        assert_eq!(redis::from_redis_value_ref::<i64>(&fields[1])?, arity);
        let flags: Vec<String> = redis::from_redis_value_ref(&fields[2])?;
        assert!(flags.iter().any(|flag| flag == "readonly"));
    }
    let keyless = info[0].as_sequence().expect("Keyless command metadata");
    assert_eq!(
        &keyless[3..6],
        &[Value::Int(0), Value::Int(0), Value::Int(0)]
    );
    assert!(keyless[8].as_sequence().expect("Key specs").is_empty());

    let res: Vec<String> = redis::cmd("COMMAND")
        .arg(&["GETKEYS", "classic_keys", "x", "foo", "y", "bar"])
        .query(&mut con)
        .with_context(|| "failed key discovery for classic_keys")?;

    assert_eq!(&res, &["x", "y"]);

    let res: Vec<String> = redis::cmd("COMMAND")
        .arg(&["GETKEYS", "keyword_keys", "foo", "x", "1", "y", "2"])
        .query(&mut con)
        .with_context(|| "failed key discovery for keyword_keys")?;

    assert_eq!(&res, &["x", "y"]);

    let res: Vec<String> = redis::cmd("COMMAND")
        .arg(&["GETKEYS", "num_keys", "3", "x", "y", "z", "foo", "bar"])
        .query(&mut con)
        .with_context(|| "failed key discovery for num_keys")?;

    assert_eq!(&res, &["x", "y", "z"]);

    let res: Vec<String> = redis::cmd("COMMAND")
        .arg(&["GETKEYS", "num_keys", "0", "foo", "bar"])
        .query(&mut con)
        .with_context(|| "failed zero-key discovery for num_keys")?;

    assert!(res.is_empty());

    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");

    Ok(())
}

#[test]
fn test_redis_value_derive() -> Result<()> {
    let mut con = TestConnection::new("proc_macro_commands");

    let res: Value = redis::cmd("redis_value_derive")
        .query(&mut con)
        .with_context(|| "failed structured redis_value_derive reply")?;

    let fields: HashMap<String, Value> = redis::from_redis_value_ref(&res)?;
    assert_eq!(fields.len(), 11);
    let text = |s: &str| Value::BulkString(s.as_bytes().to_vec());
    let inner = |i| Value::Array(vec![text("i1"), Value::Int(i)]);
    for (name, expected) in [
        ("i", Value::Int(10)),
        ("i1", Value::Int(1)),
        ("u", Value::Int(20)),
        ("s", text("s")),
        (
            "v",
            Value::Array(vec![Value::Int(1), Value::Int(2), Value::Int(3)]),
        ),
        ("v2", Value::Array(vec![inner(1), inner(2)])),
        ("hash_map", Value::Array(vec![text("key"), text("val")])),
        ("hash_set", Value::Array(vec![text("key")])),
        ("ordered_map", Value::Array(vec![text("key"), inner(10)])),
        ("ordered_set", Value::Array(vec![text("key")])),
    ] {
        assert_eq!(fields.get(name), Some(&expected), "Reply field {name}");
    }
    assert_eq!(redis::from_redis_value_ref::<f64>(&fields["f"])?, 1.1);
    assert!(
        !fields.contains_key("inner"),
        "Flattened fields belong at the top level"
    );

    let res: String = redis::cmd("redis_value_derive")
        .arg(&["test"])
        .query(&mut con)
        .with_context(|| "failed string variant of redis_value_derive")?;

    assert_eq!(res, "OK");

    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");

    Ok(())
}

#[test]
#[cfg(any(
    feature = "min-redis-compatibility-version-8-0",
    feature = "min-redis-compatibility-version-7-4",
    feature = "min-redis-compatibility-version-7-2"
))]
fn test_call_blocking() -> Result<()> {
    let mut con = TestConnection::new("call");

    let res: Option<String> = redis::cmd("call.blocking")
        .query(&mut con)
        .with_context(|| "failed to run string.set")?;

    assert_eq!(res, None);

    let res: Option<String> = redis::cmd("call.blocking_from_detached_ctx")
        .query(&mut con)
        .with_context(|| "failed to run string.set")?;

    assert_eq!(res, None);

    Ok(())
}

#[test]
#[cfg(any(
    feature = "min-redis-compatibility-version-8-0",
    feature = "min-redis-compatibility-version-7-4",
    feature = "min-redis-compatibility-version-7-2"
))]
fn test_call_blocking_abort_releases_handlers_once() -> Result<()> {
    let mut con = TestConnection::new("call");

    // The empty handler is zero-sized, so its boxed pointer was never allocated.
    for (key, kind) in [("abort:empty", "empty"), ("abort:capture", "capture")] {
        let status: String = redis::cmd("call.blocking_abort")
            .arg(key)
            .arg(kind)
            .query(&mut con)
            .with_context(|| format!("failed to abort a {kind} handler"))?;
        assert_eq!(status, "aborted", "{kind}");
    }

    // Redis serves blocked calls after each command, so a still-pending BLPOP
    // would have run its handler and emptied the list before the next command.
    for key in ["abort:empty", "abort:capture"] {
        let _: i64 = redis::cmd("RPUSH").arg(key).arg("value").query(&mut con)?;
    }
    let stats: Vec<i64> = redis::cmd("call.blocking_abort_stats").query(&mut con)?;
    assert_eq!(stats, vec![0, 1, 0], "handler calls and capture drops");
    for key in ["abort:empty", "abort:capture"] {
        let len: i64 = redis::cmd("LLEN").arg(key).query(&mut con)?;
        assert_eq!(len, 1, "{key}");
    }

    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");

    Ok(())
}

#[test]
#[cfg(any(
    feature = "min-redis-compatibility-version-8-0",
    feature = "min-redis-compatibility-version-7-4",
    feature = "min-redis-compatibility-version-7-2"
))]
fn test_call_blocking_abort_after_unblock_keeps_handler_result() -> Result<()> {
    let mut con = TestConnection::new("call");

    let res: String = redis::cmd("call.blocking_keep")
        .arg("abort:kept")
        .query(&mut con)?;
    assert_eq!(res, "OK");

    // The push unblocks the kept call: its handler runs once, consumes the
    // push, and drops its capture when it returns.
    let _: i64 = redis::cmd("RPUSH")
        .arg("abort:kept")
        .arg("value")
        .query(&mut con)?;
    let stats: Vec<i64> = redis::cmd("call.blocking_abort_stats").query(&mut con)?;
    assert_eq!(stats, vec![0, 1, 1], "capture drops and kept handler calls");
    let len: i64 = redis::cmd("LLEN").arg("abort:kept").query(&mut con)?;
    assert_eq!(len, 0);

    // A later abort fails and releases nothing: releasing the handler again
    // would drop the capture twice.
    let status: String = redis::cmd("call.blocking_abort_kept").query(&mut con)?;
    assert_eq!(status, "not aborted");
    let stats: Vec<i64> = redis::cmd("call.blocking_abort_stats").query(&mut con)?;
    assert_eq!(stats, vec![0, 1, 1], "after the failed abort");

    // The handler was released once: a second release would refuse unload.
    redis::cmd("MODULE")
        .arg(&["UNLOAD", "call"])
        .query::<()>(&mut con)?;

    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");

    Ok(())
}

#[test]
#[cfg(any(
    feature = "min-redis-compatibility-version-8-0",
    feature = "min-redis-compatibility-version-7-4",
    feature = "min-redis-compatibility-version-7-2"
))]
fn test_call_blocking_with_auto_memory_disposes_before_return() -> Result<()> {
    let mut con = TestConnection::new("call");

    let res: String = redis::cmd("call.blocking_auto_memory")
        .arg("auto:memory")
        .query(&mut con)?;
    assert_eq!(res, "OK");

    // Disposing removed the promise reply from AutoMemory, so collection freed
    // nothing; the blocked client's reference keeps the promise, and the
    // handler runs when the push unblocks the call.
    let _: i64 = redis::cmd("RPUSH")
        .arg("auto:memory")
        .arg("value")
        .query(&mut con)?;
    let calls: i64 = redis::cmd("call.blocking_auto_memory_calls").query(&mut con)?;
    assert_eq!(calls, 1);
    let len: i64 = redis::cmd("LLEN").arg("auto:memory").query(&mut con)?;
    assert_eq!(len, 0);

    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");

    Ok(())
}

#[test]
#[cfg(any(
    feature = "min-redis-compatibility-version-8-0",
    feature = "min-redis-compatibility-version-7-4",
    feature = "min-redis-compatibility-version-7-2"
))]
fn test_module_unload_waits_for_pending_unblock_handlers() -> Result<()> {
    let mut con = TestConnection::new("call");

    let res: String = redis::cmd("call.blocking_fire")
        .arg("unload:pending")
        .arg("handler")
        .query(&mut con)?;
    assert_eq!(res, "pending");

    // Redis would call the handler, and read the module, after an unload.
    let refused = redis::cmd("MODULE")
        .arg(&["UNLOAD", "call"])
        .query::<()>(&mut con);
    assert!(
        refused.is_err(),
        "unload with a pending handler: {refused:?}"
    );

    // Once the push unblocks the call and its handler has run, unload works.
    let _: i64 = redis::cmd("RPUSH")
        .arg("unload:pending")
        .arg("value")
        .query(&mut con)?;
    let calls: i64 = redis::cmd("call.blocking_fire_calls").query(&mut con)?;
    assert_eq!(calls, 1);
    redis::cmd("MODULE")
        .arg(&["UNLOAD", "call"])
        .query::<()>(&mut con)?;
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");

    // A reloaded module starts with no pending handlers and guards again.
    redis::cmd("MODULE")
        .arg("LOAD")
        .arg(utils::module_path("call")?)
        .query::<()>(&mut con)?;
    let res: String = redis::cmd("call.blocking_fire")
        .arg("unload:reloaded")
        .arg("handler")
        .query(&mut con)?;
    assert_eq!(res, "pending");
    let refused = redis::cmd("MODULE")
        .arg(&["UNLOAD", "call"])
        .query::<()>(&mut con);
    assert!(
        refused.is_err(),
        "reloaded unload with a pending handler: {refused:?}"
    );
    let _: i64 = redis::cmd("RPUSH")
        .arg("unload:reloaded")
        .arg("value")
        .query(&mut con)?;
    redis::cmd("MODULE")
        .arg(&["UNLOAD", "call"])
        .query::<()>(&mut con)?;
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");

    Ok(())
}

#[test]
#[cfg(any(
    feature = "min-redis-compatibility-version-8-0",
    feature = "min-redis-compatibility-version-7-4",
    feature = "min-redis-compatibility-version-7-2"
))]
fn test_module_unload_allows_blocking_calls_without_pending_handlers() -> Result<()> {
    for (mode, reply) in [("none", "no handler"), ("abort", "aborted")] {
        let mut con = TestConnection::new("call");
        let res: String = redis::cmd("call.blocking_fire")
            .arg("unload:released")
            .arg(mode)
            .query(&mut con)?;
        assert_eq!(res, reply, "{mode}");
        redis::cmd("MODULE")
            .arg(&["UNLOAD", "call"])
            .query::<()>(&mut con)
            .with_context(|| format!("unload after {mode}"))?;
        assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    }

    Ok(())
}

#[test]
#[cfg(any(
    feature = "min-redis-compatibility-version-8-0",
    feature = "min-redis-compatibility-version-7-4",
    feature = "min-redis-compatibility-version-7-2"
))]
fn test_module_unload_after_aborting_a_kept_handle() -> Result<()> {
    let mut con = TestConnection::new("call");

    let res: String = redis::cmd("call.blocking_keep")
        .arg("unload:kept")
        .query(&mut con)?;
    assert_eq!(res, "OK");
    let refused = redis::cmd("MODULE")
        .arg(&["UNLOAD", "call"])
        .query::<()>(&mut con);
    assert!(refused.is_err(), "unload with a kept handle: {refused:?}");

    // Keeping the handle is what makes the pending call cancellable.
    let status: String = redis::cmd("call.blocking_abort_kept").query(&mut con)?;
    assert_eq!(status, "aborted");
    redis::cmd("MODULE")
        .arg(&["UNLOAD", "call"])
        .query::<()>(&mut con)?;
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");

    Ok(())
}

#[test]
#[cfg(any(
    feature = "min-redis-compatibility-version-8-0",
    feature = "min-redis-compatibility-version-7-4",
    feature = "min-redis-compatibility-version-7-2"
))]
fn test_module_unload_after_panicking_and_worker_handlers_run() -> Result<()> {
    for mode in ["panic", "worker"] {
        let mut con = TestConnection::new("call");
        let key = format!("unload:{mode}");
        let _: String = redis::cmd("call.blocking_fire")
            .arg(&key)
            .arg(mode)
            .query(&mut con)?;
        if mode == "worker" {
            utils::wait_until(
                Duration::from_secs(5),
                "worker installs its handler",
                || Ok(redis::cmd("call.blocking_worker_installed").query::<i64>(&mut con)? == 1),
            )?;
        }
        let refused = redis::cmd("MODULE")
            .arg(&["UNLOAD", "call"])
            .query::<()>(&mut con);
        assert!(
            refused.is_err(),
            "{mode} unload with a pending handler: {refused:?}"
        );

        let _: i64 = redis::cmd("RPUSH").arg(&key).arg("value").query(&mut con)?;
        let calls: i64 = redis::cmd("call.blocking_fire_calls").query(&mut con)?;
        assert_eq!(calls, 1, "{mode}");
        redis::cmd("MODULE")
            .arg(&["UNLOAD", "call"])
            .query::<()>(&mut con)
            .with_context(|| format!("unload after the {mode} handler ran"))?;
        assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    }

    Ok(())
}

#[test]
#[cfg(any(
    feature = "min-redis-compatibility-version-8-0",
    feature = "min-redis-compatibility-version-7-4",
    feature = "min-redis-compatibility-version-7-2"
))]
fn test_blocking_calls_fail_while_the_module_loads_or_unloads() -> Result<()> {
    let mut con = TestConnection::new("call");

    // init's blocking call was refused, so it left no handler pending.
    let started: i64 = redis::cmd("call.init_blocking").query(&mut con)?;
    assert_eq!(started, 0);
    let path = utils::module_path("call")?;
    redis::cmd("MODULE")
        .arg(&["UNLOAD", "call"])
        .query::<()>(&mut con)?;
    let log = con.server_log()?;
    assert!(
        log.contains("call deinit: blocking call refused"),
        "deinit's blocking call was not refused"
    );

    // LOADEX rejects an unconsumed configuration after OnLoad succeeded, and
    // Redis then forces an unload that must succeed. A handler that init left
    // pending would make it refuse, which Redis asserts against.
    redis::cmd("MODULE")
        .arg("LOADEX")
        .arg(&path)
        .arg(&["CONFIG", "call.unknown", "1"])
        .query::<()>(&mut con)
        .expect_err("an unconsumed LOADEX configuration was accepted");
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");

    Ok(())
}

#[test]
#[cfg(any(
    feature = "min-redis-compatibility-version-8-0",
    feature = "min-redis-compatibility-version-7-4",
    feature = "min-redis-compatibility-version-7-2"
))]
fn test_call_dump() -> Result<()> {
    let mut con = TestConnection::new("call");

    let res: String = redis::cmd("call.dump_test")
        .query(&mut con)
        .with_context(|| "failed to run call.dump_test")?;

    assert_eq!(&res, "pass");

    Ok(())
}

#[test]
#[cfg(any(
    feature = "min-redis-compatibility-version-8-0",
    feature = "min-redis-compatibility-version-7-4",
    feature = "min-redis-compatibility-version-7-2"
))]
fn test_call_binary_data() -> Result<()> {
    let mut con = TestConnection::new("call");

    let res: String = redis::cmd("call.binary_data_test")
        .query(&mut con)
        .with_context(|| "failed to run call.binary_data_test")?;

    assert_eq!(&res, "pass");

    Ok(())
}

/// Values whose bytes a forwarded call reply must keep: empty, ASCII, Unicode,
/// NUL, CR, LF, CRLF, invalid UTF-8, and a 4 KiB value ending in invalid UTF-8.
fn call_reply_inputs() -> Vec<Vec<u8>> {
    let mut large = vec![b'x'; 4096];
    large.push(0xff);
    vec![
        b"".to_vec(),
        b"ASCII".to_vec(),
        "h\u{e9}llo \u{65e5}\u{672c}".as_bytes().to_vec(),
        b"a\0b".to_vec(),
        b"\r".to_vec(),
        b"\n".to_vec(),
        b"\r\n".to_vec(),
        vec![0xff, 0, 0xfe],
        large,
    ]
}

#[test]
fn test_call_reply_strings_are_bulk() -> Result<()> {
    let mut con = TestConnection::new("call_reply");
    let inputs = call_reply_inputs();
    let keys: Vec<String> = (0..inputs.len())
        .map(|i| format!("call_reply:{i}"))
        .collect();

    for (key, input) in keys.iter().zip(&inputs) {
        let echoed: Value = redis::cmd("call_reply.echo")
            .arg(input.as_slice())
            .query(&mut con)
            .with_context(|| format!("failed to forward ECHO {input:?}"))?;
        assert_eq!(echoed, Value::BulkString(input.clone()), "ECHO {input:?}");

        redis::cmd("SET")
            .arg(key)
            .arg(input.as_slice())
            .query::<()>(&mut con)?;
        let value: Value = redis::cmd("call_reply.get").arg(key).query(&mut con)?;
        assert_eq!(value, Value::BulkString(input.clone()), "GET {input:?}");

        let direct: Value = redis::cmd("DUMP").arg(key).query(&mut con)?;
        let forwarded: Value = redis::cmd("call_reply.dump").arg(key).query(&mut con)?;
        assert_eq!(forwarded, direct, "DUMP of {input:?}");
    }

    let values: Value = redis::cmd("call_reply.mget").arg(&keys).query(&mut con)?;
    let expected = inputs.iter().cloned().map(Value::BulkString).collect();
    assert_eq!(values, Value::Array(expected));

    let missing: Value = redis::cmd("call_reply.get")
        .arg("call_reply:missing")
        .query(&mut con)?;
    assert_eq!(missing, Value::Nil);

    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    Ok(())
}

#[test]
fn test_call_reply_status_and_collections() -> Result<()> {
    let mut con = TestConnection::new("call_reply");
    let inputs = call_reply_inputs();

    // A converted call reply keeps a native status as a status, like a
    // module's own status reply.
    let forwarded: Value = redis::cmd("call_reply.status")
        .arg("call_reply:status")
        .arg("value")
        .query(&mut con)?;
    assert_eq!(forwarded, Value::Okay);
    let explicit: Value = redis::cmd("call_reply.explicit_status").query(&mut con)?;
    assert_eq!(explicit, Value::Okay);

    // Each field is a numbered prefix followed by the input bytes.
    let fields: Vec<Vec<u8>> = inputs
        .iter()
        .enumerate()
        .map(|(i, input)| [format!("field-{i}:").as_bytes(), input].concat())
        .collect();
    let mut hset = redis::cmd("HSET");
    hset.arg("call_reply:hash");
    for (field, input) in fields.iter().zip(&inputs) {
        hset.arg(field.as_slice()).arg(input.as_slice());
    }
    hset.query::<()>(&mut con)?;
    let pairs: Vec<Vec<u8>> = redis::cmd("call_reply.hgetall")
        .arg("call_reply:hash")
        .query(&mut con)?;
    let pairs: HashMap<Vec<u8>, Vec<u8>> = pairs
        .chunks(2)
        .map(|pair| (pair[0].clone(), pair[1].clone()))
        .collect();
    assert_eq!(pairs.len(), inputs.len());
    for (field, input) in fields.iter().zip(&inputs) {
        assert_eq!(pairs.get(field), Some(input), "HGETALL field {field:?}");
    }

    redis::cmd("SADD")
        .arg("call_reply:set")
        .arg(&inputs)
        .query::<()>(&mut con)?;
    let mut members: Vec<Vec<u8>> = redis::cmd("call_reply.smembers")
        .arg("call_reply:set")
        .query(&mut con)?;
    let mut expected = inputs.clone();
    members.sort();
    expected.sort();
    assert_eq!(members, expected);

    let server: String = redis::cmd("INFO").arg("server").query(&mut con)?;
    let version: Vec<i64> = server
        .lines()
        .find_map(|line| line.strip_prefix("redis_version:"))
        .expect("INFO server reports redis_version")
        .trim()
        .split('.')
        .map(|part| part.parse().expect("numeric version part"))
        .collect();
    let reported: Vec<i64> = redis::cmd("call_reply.version").query(&mut con)?;
    assert_eq!(reported[..3], version[..3]);
    assert_eq!(reported[3], 0, "not an enterprise server");

    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    Ok(())
}

/// Redis before 7.0 has no module API for the RESP3 reply types, so there
/// every client gets what Redis 7 sends a RESP2 client, and a verbatim reply
/// keeps its type only in the `txt` format.
#[test]
fn test_reply_types_fall_back_before_redis_7() -> Result<()> {
    let mut con = TestConnection::new("reply_types");
    // A reply that announces more elements than it sends fails the read
    // instead of hanging the test.
    con.set_read_timeout(Some(Duration::from_secs(5)))?;
    con.set_write_timeout(Some(Duration::from_secs(5)))?;
    let server: String = redis::cmd("INFO").arg("server").query(&mut con)?;
    let version: Vec<u32> = server
        .lines()
        .find_map(|line| line.strip_prefix("redis_version:"))
        .context("INFO server reports redis_version")?
        .trim()
        .split('.')
        .map(|part| part.parse().context("numeric version part"))
        .collect::<Result<_>>()?;
    let native = version[0] >= 7;
    let digits = "1234567890123456789012345678901234567890";
    let bulk = |s: &str| Value::BulkString(s.as_bytes().to_vec());

    for protocol in [2, 3, 2] {
        redis::cmd("HELLO").arg(protocol).query::<Value>(&mut con)?;
        let resp3 = protocol == 3;
        let typed = resp3 && native;

        let bools: Value = redis::cmd("reply.bool").query(&mut con)?;
        let expected = if typed {
            vec![Value::Boolean(true), Value::Boolean(false)]
        } else {
            vec![Value::Int(1), Value::Int(0)]
        };
        assert_eq!(bools, Value::Array(expected), "RESP{protocol}");

        let big: Value = redis::cmd("reply.big_number").query(&mut con)?;
        let expected = if typed {
            Value::BigNumber(digits.as_bytes().to_vec())
        } else {
            bulk(digits)
        };
        assert_eq!(big, expected, "RESP{protocol}");

        for (format, verbatim_format) in [
            ("txt", VerbatimFormat::Text),
            ("mkd", VerbatimFormat::Markdown),
        ] {
            let text: Value = redis::cmd("reply.verbatim")
                .arg(&[format, "some text"])
                .query(&mut con)?;
            let expected = if resp3 && (native || format == "txt") {
                Value::VerbatimString {
                    format: verbatim_format,
                    text: "some text".to_owned(),
                }
            } else {
                bulk("some text")
            };
            assert_eq!(text, expected, "RESP{protocol} {format}");
        }

        let set: Value = redis::cmd("reply.set").query(&mut con)?;
        let expected = if typed {
            Value::Set(vec![bulk("a"), bulk("b"), Value::Boolean(true)])
        } else {
            Value::Array(vec![bulk("a"), bulk("b"), Value::Int(1)])
        };
        assert_eq!(set, expected, "RESP{protocol}");

        // Redis 6.0 has no API that names the calling client's user.
        let user: Value = redis::cmd("reply.user").query(&mut con)?;
        let expected = if version[..2] == [6, 0] {
            Value::Nil
        } else {
            bulk("default")
        };
        assert_eq!(user, expected, "RESP{protocol}");

        assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    }
    Ok(())
}

#[test]
fn test_key_type_names_each_built_in_type() -> Result<()> {
    let mut con = TestConnection::new("key_type");
    let key_type = |con: &mut TestConnection, key: &str| -> Result<String> {
        Ok(redis::cmd("keytype.of").arg(key).query(con)?)
    };
    for args in [
        &["SET", "string", "v"][..],
        &["RPUSH", "list", "v"],
        &["HSET", "hash", "f", "v"],
        &["SADD", "set", "v"],
        &["ZADD", "zset", "1", "v"],
        &["XADD", "stream", "*", "f", "v"],
    ] {
        redis::cmd(args[0]).arg(&args[1..]).query::<()>(&mut con)?;
    }
    for (key, expected) in [
        ("missing", "Empty"),
        ("string", "String"),
        ("list", "List"),
        ("hash", "Hash"),
        ("set", "Set"),
        ("zset", "ZSet"),
        ("stream", "Stream"),
    ] {
        assert_eq!(key_type(&mut con, key)?, expected, "{key}");
    }
    // Redis 8.8 added arrays, which RedisModule_KeyType reports as 8.
    let arset: Vec<Value> = redis::cmd("COMMAND")
        .arg(&["INFO", "ARSET"])
        .query(&mut con)?;
    if arset != vec![Value::Nil] {
        redis::cmd("ARSET")
            .arg(&["array", "0", "v"])
            .query::<()>(&mut con)?;
        assert_eq!(key_type(&mut con, "array")?, "Array");
    }
    Ok(())
}

#[test]
fn test_open_key_with_flags() -> Result<()> {
    let mut con = TestConnection::new("open_key_with_flags");

    // Avoid active expriation
    redis::cmd("DEBUG")
        .arg(&["SET-ACTIVE-EXPIRE", "0"])
        .query::<()>(&mut con)
        .with_context(|| "failed to run DEBUG SET-ACTIVE-EXPIRE")?;

    for cmd in ["open_key_with_flags.write", "open_key_with_flags.read"].into_iter() {
        redis::cmd("set")
            .arg(&["x", "1"])
            .query::<()>(&mut con)
            .with_context(|| "failed to run string.set")?;

        // Set experition time to 1 second.
        redis::cmd("pexpire")
            .arg(&["x", "1"])
            .query::<()>(&mut con)
            .with_context(|| "failed to run expire")?;

        // Sleep for 2 seconds, ensure expiration time has passed.
        thread::sleep(Duration::from_millis(500));

        // Open key as read only or ReadWrite with NOEFFECTS flag.
        let res = redis::cmd(cmd).arg(&["x"]).query(&mut con);
        assert_eq!(res, Ok(()));

        // Get the number of expired keys.
        let stats: String = redis::cmd("info").arg(&["stats"]).query(&mut con)?;

        // Find the number of expired keys, x,  according to the substring "expired_keys:{x}"
        let expired_keys = stats
            .match_indices("expired_keys:")
            .next()
            .map(|(i, _)| &stats[i..i + "expired_keys:".len() + 1])
            .and_then(|s| s.split(':').nth(1))
            .and_then(|s| s.parse::<i32>().ok())
            .unwrap_or(-1);

        // Ensure that no keys were expired.
        assert_eq!(expired_keys, 0);

        // Delete key and reset stats
        redis::cmd("del").arg(&["x"]).query::<()>(&mut con)?;
        redis::cmd("config")
            .arg(&["RESETSTAT"])
            .query::<()>(&mut con)?;
    }

    Ok(())
}

#[test]
fn test_expire() -> Result<()> {
    let mut con = TestConnection::new("expire");

    // Create a key without TTL
    redis::cmd("set")
        .arg(&["key", "value"])
        .query::<()>(&mut con)
        .with_context(|| "failed to run set")?;

    let ttl: i64 = redis::cmd("ttl").arg(&["key"]).query(&mut con)?;
    assert_eq!(ttl, -1);

    // Set TTL on the key
    redis::cmd("expire.cmd")
        .arg(&["key", "100"])
        .query::<()>(&mut con)
        .with_context(|| "failed to run expire.cmd")?;

    let ttl: i64 = redis::cmd("ttl").arg(&["key"]).query(&mut con)?;
    assert!(ttl > 0);

    // Remove TTL on the key
    redis::cmd("expire.cmd")
        .arg(&["key", "-1"])
        .query::<()>(&mut con)
        .with_context(|| "failed to run expire.cmd")?;

    let ttl: i64 = redis::cmd("ttl").arg(&["key"]).query(&mut con)?;
    assert_eq!(ttl, -1);

    Ok(())
}

#[test]
fn test_defrag() -> Result<()> {
    let mut con = TestConnection::new("data_type");

    // Configure active defrag
    redis::cmd("config")
        .arg(&["set", "hz", "100"])
        .query::<()>(&mut con)
        .with_context(|| "failed to run 'config set hz 100'")?;

    redis::cmd("config")
        .arg(&["set", "active-defrag-ignore-bytes", "1"])
        .query::<()>(&mut con)
        .with_context(|| "failed to run 'config set active-defrag-ignore-bytes 1'")?;

    redis::cmd("config")
        .arg(&["set", "active-defrag-threshold-lower", "0"])
        .query::<()>(&mut con)
        .with_context(|| "failed to run 'config set active-defrag-threshold-lower 0'")?;

    redis::cmd("config")
        .arg(&["set", "active-defrag-cycle-min", "99"])
        .query::<()>(&mut con)
        .with_context(|| "failed to run 'config set active-defrag-cycle-min 99'")?;

    // enable active defrag
    if redis::cmd("config")
        .arg(&["set", "activedefrag", "yes"])
        .query::<String>(&mut con)
        .is_err()
    {
        // Server the does not support active defrag, avoid failing the test.
        return Ok(());
    }

    let start = SystemTime::now();
    loop {
        let res: HashMap<String, usize> = redis::cmd("alloc.defragstats")
            .query(&mut con)
            .with_context(|| "failed to run 'config set active-defrag-cycle-min 99'")?;
        let num_defrag_globals = res.get("num_defrag_globals").ok_or_else(|| {
            anyhow::Error::msg("Failed getting 'num_defrag_globals' value from result")
        })?;
        // Wait till we will get at least 2 defrag cycles.
        // We are looking at num_defrag_globals because this is supported by all Redis versions
        // that supports defrag.
        if *num_defrag_globals > 2 {
            break;
        }
        let duration = SystemTime::now().duration_since(start)?;
        if duration > Duration::from_secs(30) {
            return Err(anyhow::Error::msg("Failed waiting for defrag cycle"));
        }
    }

    Ok(())
}

/// Loading warns only about defrag code the server cannot run: defrag functions
/// need Redis 6.2 and defrag start and end functions Redis 8.0. `hello` has no
/// defrag code, and `data_type` has all three kinds.
#[test]
fn test_defrag_registration_warns_only_for_unsupported_code() -> Result<()> {
    for (module, has_defrag_code) in [("hello", false), ("data_type", true)] {
        let mut con = TestConnection::new(module);
        let server: String = redis::cmd("INFO").arg("server").query(&mut con)?;
        let version: Vec<u32> = server
            .lines()
            .find_map(|line| line.strip_prefix("redis_version:"))
            .context("INFO server reports redis_version")?
            .trim()
            .split('.')
            .map(|part| part.parse().context("numeric version part"))
            .collect::<Result<_>>()?;
        let version = (version[0], version[1]);
        let log = con.server_log()?;
        let warned = |what: &str| log.contains(&format!("Skip register defrag {what} as"));
        assert_eq!(
            warned("function"),
            has_defrag_code && version < (6, 2),
            "{module} on {version:?}: {log}"
        );
        assert_eq!(
            warned("callbacks"),
            has_defrag_code && version < (8, 0),
            "{module} on {version:?}: {log}"
        );
    }
    Ok(())
}

#[test]
fn test_module_type_wrongtype() -> Result<()> {
    let mut con = TestConnection::new("data_type");

    redis::cmd("set")
        .arg(&["s", "v"])
        .query::<()>(&mut con)
        .with_context(|| "failed to run set")?;
    for (cmd, args) in [("alloc.get", &["s"][..]), ("alloc.set", &["s", "1"][..])] {
        let err = redis::cmd(cmd)
            .arg(args)
            .query::<redis::Value>(&mut con)
            .expect_err("a module type lookup on a string key must fail");
        assert_eq!(
            err.code(),
            Some("WRONGTYPE"),
            "{cmd}: unexpected error: {err}"
        );
    }

    Ok(())
}

#[test]
fn test_alloc_alignment() -> Result<()> {
    let mut con = TestConnection::new("alloc_alignment");

    // Each entry is [align, size, misaligned count].
    let res: Vec<Vec<i64>> = redis::cmd("alloc_alignment.check")
        .query(&mut con)
        .with_context(|| "failed to run alloc_alignment.check")?;
    assert_eq!(res.len(), 7);

    let misaligned: Vec<_> = res.iter().filter(|entry| entry[2] != 0).collect();
    assert!(
        misaligned.is_empty(),
        "misaligned boxes as [align, size, count]: {misaligned:?}"
    );

    Ok(())
}
