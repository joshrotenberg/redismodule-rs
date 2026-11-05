//! Replication while Redis pauses writes (#220). Redis aborts at an assertion
//! when a module replicates during the pause; the SDK refuses instead, except
//! for a `write` or `may-replicate` command in the transaction that starts the
//! pause, which Redis replicates. Calls from post-notification jobs are refused
//! during a pause with or without the replicate flag (#231).
//!
//! Each test attaches a replica, so the primary has a replication backlog and
//! Redis 7.0 and later propagate (6.2 propagates without one), then pauses
//! writes on the primary. Redis 6.0 has no `CLIENT PAUSE WRITE` or
//! `CLIENT UNPAUSE`, and the tests skip there.
use super::utils::{TestConnection, wait_until};
use anyhow::{Context, Result, ensure};
use redis::Value;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);

fn server_version(con: &mut TestConnection) -> Result<(u32, u32)> {
    let info: String = redis::cmd("INFO").arg("server").query(con)?;
    let version = info
        .lines()
        .find_map(|line| line.strip_prefix("redis_version:"))
        .context("INFO server reports redis_version")?
        .trim();
    let mut parts = version.split('.').map(str::parse::<u32>);
    let major = parts.next().context("major version")??;
    let minor = parts.next().context("minor version")??;
    Ok((major, minor))
}

/// A primary running `module` that propagates to a replica. `None` on a
/// server without `CLIENT PAUSE WRITE`.
fn primary_with_replica(module: &str) -> Result<Option<(TestConnection, TestConnection)>> {
    let mut primary = TestConnection::new(module);
    let version = server_version(&mut primary)?;
    if version < (6, 2) {
        eprintln!("skipped: Redis {version:?} has no CLIENT PAUSE WRITE");
        return Ok(None);
    }
    let replica = primary.replica()?;
    Ok(Some((primary, replica)))
}

fn pause(primary: &mut TestConnection) -> Result<()> {
    Ok(redis::cmd("CLIENT")
        .arg(&["PAUSE", "20000", "WRITE"])
        .query::<()>(primary)?)
}

/// A primary running the `replicate` example with a replica, with writes
/// paused on the primary.
fn paused_primary() -> Result<Option<(TestConnection, TestConnection)>> {
    let Some((mut primary, replica)) = primary_with_replica("replicate")? else {
        return Ok(None);
    };
    assert_eq!(paused(&mut primary)?, 0);
    pause(&mut primary)?;
    assert_eq!(paused(&mut primary)?, 1);
    Ok(Some((primary, replica)))
}

fn paused(con: &mut TestConnection) -> Result<i64> {
    Ok(redis::cmd("replicate.paused").query(con)?)
}

/// Checks that the server survived the refusal, then ends the pause.
fn unpause(primary: &mut TestConnection) -> Result<()> {
    assert_eq!(redis::cmd("PING").query::<String>(primary)?, "PONG");
    let log = primary.server_log()?;
    ensure!(!log.contains("ASSERTION FAILED"), "server log: {log}");
    redis::cmd("CLIENT").arg("UNPAUSE").query::<()>(primary)?;
    Ok(())
}

/// Waits until the replica has everything the primary propagated so far.
/// `WAIT` waits for this client's own replication offset, which a timer, event
/// or worker does not advance, so the client first writes a marker of its own
/// behind whatever they propagated.
fn acknowledge(primary: &mut TestConnection, replica: &TestConnection) -> Result<()> {
    redis::cmd("INCR")
        .arg("replication-pause:ack")
        .query::<i64>(primary)?;
    let acknowledged: i64 = redis::cmd("WAIT").arg(&[1, 5000]).query(primary)?;
    assert_eq!(acknowledged, 1, "replica log: {}", replica.server_log()?);
    Ok(())
}

fn assert_refused_call(primary: &mut TestConnection, call: &mut redis::Cmd) -> Result<()> {
    let error = call
        .query::<Value>(primary)
        .expect_err("a replicated call ran during the pause");
    assert_eq!(error.code(), Some("ERR"), "{error}");
    assert!(error.to_string().contains("pauses replication"), "{error}");
    Ok(())
}

/// Waits until the replica has everything the primary propagated, then checks
/// that it has `resumed` and never received `refused`.
fn assert_replica_keys(
    primary: &mut TestConnection,
    replica: &mut TestConnection,
    refused: &str,
    resumed: &str,
) -> Result<()> {
    acknowledge(primary, replica)?;
    assert_eq!(
        redis::cmd("GET")
            .arg(resumed)
            .query::<Option<String>>(replica)?
            .as_deref(),
        Some("value")
    );
    assert_eq!(redis::cmd("EXISTS").arg(refused).query::<i64>(replica)?, 0);
    Ok(())
}

/// Polls a timer or worker until it reports whether Redis replicated.
fn background_result(con: &mut TestConnection, source: &str) -> Result<i64> {
    let mut result = -1;
    wait_until(TIMEOUT, &format!("the {source} to replicate"), || {
        result = redis::cmd("replicate.result").arg(source).query(con)?;
        Ok(result != -1)
    })?;
    Ok(result)
}

fn command_calls(con: &mut TestConnection, command: &str) -> Result<u64> {
    let info: String = redis::cmd("INFO").arg("commandstats").query(con)?;
    let prefix = format!("cmdstat_{command}:calls=");
    info.lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .map_or(Ok(0), |stats| {
            Ok(stats
                .split(',')
                .next()
                .context("calls field")?
                .parse::<u64>()?)
        })
}

#[test]
fn test_replication_pause_refuses_replicate_from_a_read_only_command() -> Result<()> {
    let Some((mut primary, mut replica)) = paused_primary()? else {
        return Ok(());
    };
    let replicated: i64 = redis::cmd("replicate.command")
        .arg(&["SET", "refused", "value"])
        .query(&mut primary)?;
    assert_eq!(replicated, 0);
    unpause(&mut primary)?;
    let replicated: i64 = redis::cmd("replicate.command")
        .arg(&["SET", "resumed", "value"])
        .query(&mut primary)?;
    assert_eq!(replicated, 1);
    assert_replica_keys(&mut primary, &mut replica, "refused", "resumed")
}

#[test]
fn test_replication_pause_refuses_replicate_verbatim() -> Result<()> {
    let Some((mut primary, mut replica)) = paused_primary()? else {
        return Ok(());
    };
    let replicated: i64 = redis::cmd("replicate.verbatim")
        .arg("refused")
        .query(&mut primary)?;
    assert_eq!(replicated, 0);
    unpause(&mut primary)?;
    let replicated: i64 = redis::cmd("replicate.verbatim")
        .arg("resumed")
        .query(&mut primary)?;
    assert_eq!(replicated, 1);
    acknowledge(&mut primary, &replica)?;
    // The replica ran only the command replicated after the pause.
    assert_eq!(command_calls(&mut replica, "replicate.verbatim")?, 1);
    Ok(())
}

#[test]
fn test_replication_pause_refuses_replicated_calls_without_running_them() -> Result<()> {
    let Some((mut primary, mut replica)) = paused_primary()? else {
        return Ok(());
    };
    assert_refused_call(
        &mut primary,
        redis::cmd("replicate.call").arg(&["SET", "refused", "value"]),
    )?;
    assert_eq!(
        redis::cmd("EXISTS")
            .arg("refused")
            .query::<i64>(&mut primary)?,
        0
    );
    unpause(&mut primary)?;
    redis::cmd("replicate.call")
        .arg(&["SET", "resumed", "value"])
        .query::<()>(&mut primary)?;
    assert_replica_keys(&mut primary, &mut replica, "refused", "resumed")
}

#[test]
fn test_replication_pause_refuses_replicate_from_a_timer() -> Result<()> {
    let Some((mut primary, mut replica)) = paused_primary()? else {
        return Ok(());
    };
    redis::cmd("replicate.timer")
        .arg(&["SET", "refused", "value"])
        .query::<()>(&mut primary)?;
    assert_eq!(background_result(&mut primary, "timer")?, 0);
    unpause(&mut primary)?;
    redis::cmd("replicate.timer")
        .arg(&["SET", "resumed", "value"])
        .query::<()>(&mut primary)?;
    assert_eq!(background_result(&mut primary, "timer")?, 1);
    assert_replica_keys(&mut primary, &mut replica, "refused", "resumed")
}

#[test]
fn test_replication_pause_refuses_replicate_from_a_server_event() -> Result<()> {
    let Some((mut primary, mut replica)) = paused_primary()? else {
        return Ok(());
    };
    redis::cmd("replicate.event")
        .arg(&["SET", "refused", "value"])
        .query::<()>(&mut primary)?;
    assert_eq!(background_result(&mut primary, "event")?, 0);
    unpause(&mut primary)?;
    redis::cmd("replicate.event")
        .arg(&["SET", "resumed", "value"])
        .query::<()>(&mut primary)?;
    assert_eq!(background_result(&mut primary, "event")?, 1);
    assert_replica_keys(&mut primary, &mut replica, "refused", "resumed")
}

#[test]
fn test_replication_pause_refuses_replicate_from_a_worker() -> Result<()> {
    let Some((mut primary, mut replica)) = paused_primary()? else {
        return Ok(());
    };
    redis::cmd("replicate.worker")
        .arg(&["SET", "refused", "value"])
        .query::<()>(&mut primary)?;
    assert_eq!(background_result(&mut primary, "worker")?, 0);
    unpause(&mut primary)?;
    redis::cmd("replicate.worker")
        .arg(&["SET", "resumed", "value"])
        .query::<()>(&mut primary)?;
    assert_eq!(background_result(&mut primary, "worker")?, 1);
    assert_replica_keys(&mut primary, &mut replica, "refused", "resumed")
}

/// A transaction runs during a write pause if it queues no write command.
/// Redis exempts a transaction only from a pause the transaction started
/// itself; the module API cannot tell the two apart, so the SDK refuses in
/// both.
#[test]
fn test_replication_pause_refuses_replicate_in_transactions() -> Result<()> {
    let Some((mut primary, mut replica)) = paused_primary()? else {
        return Ok(());
    };
    let replicated: Vec<i64> = redis::pipe()
        .atomic()
        .cmd("replicate.command")
        .arg(&["SET", "refused", "value"])
        .query(&mut primary)?;
    assert_eq!(replicated, [0]);
    unpause(&mut primary)?;

    let ((), replicated): ((), i64) = redis::pipe()
        .atomic()
        .cmd("CLIENT")
        .arg(&["PAUSE", "20000", "WRITE"])
        .cmd("replicate.command")
        .arg(&["SET", "refused-in-transaction", "value"])
        .query(&mut primary)?;
    assert_eq!(replicated, 0);
    unpause(&mut primary)?;

    let replicated: Vec<i64> = redis::pipe()
        .atomic()
        .cmd("replicate.command")
        .arg(&["SET", "resumed", "value"])
        .query(&mut primary)?;
    assert_eq!(replicated, [1]);
    assert_replica_keys(&mut primary, &mut replica, "refused", "resumed")?;
    assert_eq!(
        redis::cmd("EXISTS")
            .arg("refused-in-transaction")
            .query::<i64>(&mut replica)?,
        0
    );
    Ok(())
}

#[test]
fn test_replication_pause_refuses_calls_replicated_to_replicas_or_the_aof_only() -> Result<()> {
    let Some((mut primary, mut replica)) = paused_primary()? else {
        return Ok(());
    };
    for target in ["replicas", "aof"] {
        let key = format!("refused-{target}");
        assert_refused_call(
            &mut primary,
            redis::cmd("replicate.call_to").arg(&[target, "SET", &key, "value"]),
        )?;
        assert_eq!(
            redis::cmd("EXISTS").arg(&key).query::<i64>(&mut primary)?,
            0
        );
    }
    unpause(&mut primary)?;
    redis::cmd("replicate.call_to")
        .arg(&["replicas", "SET", "resumed", "value"])
        .query::<()>(&mut primary)?;
    redis::cmd("replicate.call_to")
        .arg(&["aof", "SET", "aof-only", "value"])
        .query::<()>(&mut primary)?;
    assert_replica_keys(&mut primary, &mut replica, "refused-replicas", "resumed")?;
    assert_eq!(
        redis::cmd("GET")
            .arg("aof-only")
            .query::<Option<String>>(&mut primary)?
            .as_deref(),
        Some("value")
    );
    for key in ["aof-only", "refused-aof"] {
        assert_eq!(redis::cmd("EXISTS").arg(key).query::<i64>(&mut replica)?, 0);
    }
    Ok(())
}

#[test]
#[cfg(any(
    feature = "min-redis-compatibility-version-8-0",
    feature = "min-redis-compatibility-version-7-4",
    feature = "min-redis-compatibility-version-7-2"
))]
fn test_replication_pause_refuses_replicated_blocking_calls() -> Result<()> {
    let Some((mut primary, mut replica)) = primary_with_replica("call")? else {
        return Ok(());
    };
    pause(&mut primary)?;
    assert_refused_call(
        &mut primary,
        redis::cmd("call.blocking_replicate").arg(&["SET", "refused", "value"]),
    )?;
    assert_eq!(
        redis::cmd("EXISTS")
            .arg("refused")
            .query::<i64>(&mut primary)?,
        0
    );
    unpause(&mut primary)?;
    redis::cmd("call.blocking_replicate")
        .arg(&["SET", "resumed", "value"])
        .query::<()>(&mut primary)?;
    assert_replica_keys(&mut primary, &mut replica, "refused", "resumed")
}

/// Redis postpones a transaction that queued a `write` command during an
/// earlier write pause, so a write command that runs in a transaction during a
/// pause is in the transaction that started it, and Redis replicates it. The
/// `lists` example's `write` command replicates verbatim.
#[test]
fn test_replication_pause_lets_write_commands_replicate_in_the_transaction_that_pauses()
-> Result<()> {
    let Some((mut primary, mut replica)) = primary_with_replica("lists")? else {
        return Ok(());
    };
    let ((), pushed): ((), i64) = redis::pipe()
        .atomic()
        .cmd("CLIENT")
        .arg(&["PAUSE", "20000", "WRITE"])
        .cmd("LISTS.PUSHHEAD")
        .arg(&["list", "element"])
        .query(&mut primary)?;
    assert_eq!(pushed, 1);
    unpause(&mut primary)?;
    acknowledge(&mut primary, &replica)?;
    assert_eq!(
        redis::cmd("LLEN").arg("list").query::<i64>(&mut primary)?,
        1
    );
    assert_eq!(
        redis::cmd("LLEN").arg("list").query::<i64>(&mut replica)?,
        1
    );
    Ok(())
}

#[test]
fn test_replication_pause_lets_write_commands_replicate_each_way_in_the_transaction_that_pauses()
-> Result<()> {
    let Some((mut primary, mut replica)) = primary_with_replica("replicate")? else {
        return Ok(());
    };
    let ((), verbatim, command, call): ((), i64, i64, i64) = redis::pipe()
        .atomic()
        .cmd("CLIENT")
        .arg(&["PAUSE", "20000", "WRITE"])
        .cmd("replicate.write")
        .arg(&["verbatim", "by-verbatim", "value"])
        .cmd("replicate.write")
        .arg(&["command", "by-command", "value"])
        .cmd("replicate.write")
        .arg(&["call", "by-call", "value"])
        .query(&mut primary)?;
    assert_eq!((verbatim, command, call), (1, 1, 1));
    unpause(&mut primary)?;
    acknowledge(&mut primary, &replica)?;
    for key in ["by-verbatim", "by-command", "by-call"] {
        assert_eq!(
            redis::cmd("GET")
                .arg(key)
                .query::<Option<String>>(&mut replica)?
                .as_deref(),
            Some("value"),
            "{key}"
        );
    }
    Ok(())
}

/// A `write` command that a read-only command calls runs on a temporary
/// client, not in a transaction Redis postponed for it, so the SDK refuses its
/// replication during a pause, in a transaction too. Redis would drop it
/// anyway, since the call has no replicate flag; called with the flag from
/// code the SDK does not check, it would abort.
///
/// Below Redis 7.0 the transaction holds only the replicated call, which the
/// SDK refuses before it runs: Redis 6.2 propagates an `EXEC` that changed
/// data, so the other case's local write would abort it whatever the SDK does.
#[test]
fn test_replication_pause_refuses_write_commands_that_another_command_calls() -> Result<()> {
    let Some((mut primary, mut replica)) = paused_primary()? else {
        return Ok(());
    };
    let replicated: i64 = redis::cmd("replicate.forward")
        .arg(&["replicate.write", "command", "called", "value"])
        .query(&mut primary)?;
    assert_eq!(replicated, 0);
    let mut transaction = redis::pipe();
    transaction.atomic();
    let with_local_write = server_version(&mut primary)? >= (7, 0);
    if with_local_write {
        transaction.cmd("replicate.forward").arg(&[
            "replicate.write",
            "command",
            "called-in-transaction",
            "value",
        ]);
    }
    transaction.cmd("replicate.forward").arg(&[
        "replicate.write",
        "call",
        "called-in-transaction",
        "value",
    ]);
    let replicated: Vec<i64> = transaction.query(&mut primary)?;
    let expected = if with_local_write {
        vec![0, 0]
    } else {
        vec![0]
    };
    assert_eq!(replicated, expected);
    unpause(&mut primary)?;
    let replicated: i64 = redis::cmd("replicate.write")
        .arg(&["command", "resumed", "value"])
        .query(&mut primary)?;
    assert_eq!(replicated, 1);
    assert_replica_keys(&mut primary, &mut replica, "called", "resumed")?;
    assert_eq!(
        redis::cmd("EXISTS")
            .arg("called-in-transaction")
            .query::<i64>(&mut replica)?,
        0
    );
    Ok(())
}

/// The `keyspace_to_stream` probe's job calls `MODULE UNLOAD` without the
/// replicate flag and logs the error.
#[cfg(feature = "redis-7-2-examples")]
const JOB_PROBE_FAILED: &str = "job probe unload failed: ";

/// A post-notification job runs after `EXEC` and before Redis propagates the
/// transaction. Any call from it, a read without the replicate flag too, ends
/// the exemption that lets a transaction which paused writes itself replicate,
/// and Redis then aborted when it propagated the transaction (#231). The SDK
/// refuses every call from a job during a pause.
#[test]
#[cfg(feature = "redis-7-2-examples")]
fn test_replication_pause_refuses_job_calls_after_the_transaction_that_pauses() -> Result<()> {
    let Some((mut primary, mut replica)) = primary_with_replica("keyspace_to_stream")? else {
        return Ok(());
    };
    let ((), (), probe): ((), (), Vec<i64>) = redis::pipe()
        .atomic()
        .cmd("CLIENT")
        .arg(&["PAUSE", "20000", "WRITE"])
        .cmd("SET")
        .arg(&["plain", "1"])
        .cmd("notify_stream.job_probe")
        .arg("unload")
        .query(&mut primary)?;
    assert_eq!(probe, [1, 0]);
    let counts: Vec<i64> = redis::cmd("notify_stream.probe_counts").query(&mut primary)?;
    assert_eq!(counts, [1, 1], "the job ran once");
    unpause(&mut primary)?;
    let log = primary.server_log()?;
    assert!(
        log.contains(&format!(
            "{JOB_PROBE_FAILED}ERR post-notification jobs cannot call commands while Redis \
             pauses replication"
        )),
        "server log: {log}"
    );
    // The refused call never reached the module's unload hook.
    assert!(
        !log.contains("pending or executing post-notification jobs"),
        "server log: {log}"
    );
    acknowledge(&mut primary, &replica)?;
    assert_eq!(
        redis::cmd("GET")
            .arg("plain")
            .query::<Option<String>>(&mut replica)?
            .as_deref(),
        Some("1")
    );
    Ok(())
}

/// Outside a pause a job's calls run: the probe's `MODULE UNLOAD` reaches the
/// module's unload hook, which refuses it while the job runs, and the
/// example's job appends with a replicated `XADD` that reaches the replica.
#[test]
#[cfg(feature = "redis-7-2-examples")]
fn test_replication_pause_lets_jobs_call_outside_a_pause() -> Result<()> {
    let Some((mut primary, mut replica)) = primary_with_replica("keyspace_to_stream")? else {
        return Ok(());
    };
    let ((), probe): ((), Vec<i64>) = redis::pipe()
        .atomic()
        .cmd("SET")
        .arg(&["plain", "1"])
        .cmd("notify_stream.job_probe")
        .arg("unload")
        .query(&mut primary)?;
    assert_eq!(probe, [1, 0]);
    let counts: Vec<i64> = redis::cmd("notify_stream.probe_counts").query(&mut primary)?;
    assert_eq!(counts, [1, 1], "the job ran once");
    let log = primary.server_log()?;
    assert!(
        log.contains(&format!("{JOB_PROBE_FAILED}ERR Error unloading module")),
        "server log: {log}"
    );
    assert!(
        log.contains("pending or executing post-notification jobs"),
        "server log: {log}"
    );
    assert!(!log.contains("pauses replication"), "server log: {log}");

    redis::cmd("SET")
        .arg(&["watched:key", "value"])
        .query::<()>(&mut primary)?;
    let stats: Vec<i64> = redis::cmd("notify_stream.stats").query(&mut primary)?;
    assert_eq!(stats, [1, 1, 0, 0], "queued, written, errors, skipped");
    acknowledge(&mut primary, &replica)?;
    assert_eq!(
        redis::cmd("GET")
            .arg("plain")
            .query::<Option<String>>(&mut replica)?
            .as_deref(),
        Some("1")
    );
    assert_eq!(
        redis::cmd("XLEN")
            .arg("watched:events")
            .query::<i64>(&mut replica)?,
        1
    );
    Ok(())
}
