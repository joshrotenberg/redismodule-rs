//! Replication while Redis pauses writes (#220). Redis aborts at an assertion
//! when a module replicates during the pause; the SDK refuses instead, except
//! for a `write` or `may-replicate` command in the transaction that starts the
//! pause, which Redis replicates. Calls from post-notification jobs are refused
//! during a pause with or without the replicate flag (#231). Outside a
//! transaction, calls that would start a pause are refused, because Redis
//! aborts when it propagates replication the command queued before (#235).
//!
//! Each test attaches a replica, so the primary has a replication backlog and
//! Redis 7.0 and later propagate (6.2 propagates without one), then pauses
//! writes on the primary or tries to. Redis 6.0 has no `CLIENT PAUSE WRITE` or
//! `CLIENT UNPAUSE`, and the tests skip there, except two #235 tests that need
//! neither.
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

#[cfg(feature = "redis-7-2-examples")]
const JOB_CALL_REFUSED: &str =
    "ERR post-notification jobs cannot call commands while Redis pauses replication";

/// A post-notification job runs after `EXEC` and before Redis propagates the
/// transaction. Any call from it, a read without the replicate flag too, ends
/// the exemption that lets a transaction which paused writes itself replicate,
/// and Redis then aborted when it propagated the transaction (#231). The SDK
/// refuses every call from a job during a pause, including the example's
/// replicated `XADD`, which it counts as an error.
#[test]
#[cfg(feature = "redis-7-2-examples")]
fn test_replication_pause_refuses_job_calls_after_the_transaction_that_pauses() -> Result<()> {
    let Some((mut primary, mut replica)) = primary_with_replica("keyspace_to_stream")? else {
        return Ok(());
    };
    let ((), (), (), probe): ((), (), (), Vec<i64>) = redis::pipe()
        .atomic()
        .cmd("CLIENT")
        .arg(&["PAUSE", "20000", "WRITE"])
        .cmd("SET")
        .arg(&["plain", "1"])
        .cmd("SET")
        .arg(&["watched:paused", "value"])
        .cmd("notify_stream.job_probe")
        .arg("unload")
        .query(&mut primary)?;
    assert_eq!(probe, [1, 0]);
    let counts: Vec<i64> = redis::cmd("notify_stream.probe_counts").query(&mut primary)?;
    assert_eq!(counts, [1, 1], "the job ran once");
    let stats: Vec<i64> = redis::cmd("notify_stream.stats").query(&mut primary)?;
    assert_eq!(stats, [1, 0, 1, 0], "queued, written, errors, skipped");
    unpause(&mut primary)?;
    let log = primary.server_log()?;
    for line in [
        format!("{JOB_PROBE_FAILED}{JOB_CALL_REFUSED}"),
        format!("notification stream append failed: {JOB_CALL_REFUSED}"),
    ] {
        assert!(log.contains(&line), "server log: {log}");
    }
    // The refused call never reached the module's unload hook.
    assert!(
        !log.contains("pending or executing post-notification jobs"),
        "server log: {log}"
    );
    acknowledge(&mut primary, &replica)?;
    for key in ["plain", "watched:paused"] {
        assert!(
            redis::cmd("EXISTS").arg(key).query::<bool>(&mut replica)?,
            "{key}"
        );
    }
    assert_eq!(
        redis::cmd("GET")
            .arg("plain")
            .query::<Option<String>>(&mut replica)?
            .as_deref(),
        Some("1")
    );
    for con in [&mut primary, &mut replica] {
        assert_eq!(
            redis::cmd("XLEN").arg("watched:events").query::<i64>(con)?,
            0
        );
    }
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

/// What a `PING` from another connection got while a job yielded.
#[cfg(feature = "redis-7-2-examples")]
#[derive(Debug, PartialEq)]
enum PingDuringYield {
    /// `-BUSY`: the yield served clients.
    Busy,
    /// `PONG` after waiting at least a second: the yield served no clients.
    Waited,
    /// Neither within the deadline.
    Neither,
}

/// Runs `job`, which queues the probe's `yield` job, while another connection
/// sends `PING` every 10 ms. The job yields with `YieldMode::Clients` for 2.5
/// seconds, past `busy-reply-threshold`, which this sets to 100 ms.
#[cfg(feature = "redis-7-2-examples")]
fn ping_during_yield(
    primary: &mut TestConnection,
    job: impl FnOnce(&mut TestConnection) -> Result<()>,
) -> Result<PingDuringYield> {
    redis::cmd("CONFIG")
        .arg(&["SET", "busy-reply-threshold", "100"])
        .query::<()>(primary)?;
    let mut pinger = primary.connection()?;
    // A PING postponed forever then fails the test instead of hanging it.
    pinger.set_read_timeout(Some(Duration::from_secs(10)))?;
    let pings = std::thread::spawn(move || -> Result<PingDuringYield> {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            let sent = std::time::Instant::now();
            match redis::cmd("PING").query::<String>(&mut pinger) {
                Ok(_) if sent.elapsed() >= Duration::from_secs(1) => {
                    return Ok(PingDuringYield::Waited);
                }
                Ok(_) => std::thread::sleep(Duration::from_millis(10)),
                Err(error) if error.code() == Some("BUSY") => return Ok(PingDuringYield::Busy),
                Err(error) => return Err(error.into()),
            }
        }
        Ok(PingDuringYield::Neither)
    });
    job(primary)?;
    pings.join().expect("the PING thread panicked")
}

/// A client command that Redis runs during a yield ends the exemption as a
/// call does: a replica's `REPLCONF ACK`, which is `allow-busy`, made Redis
/// abort when it propagated the transaction. During a pause the SDK yields a
/// job with `YieldMode::Events`, which postpones client commands.
#[test]
#[cfg(feature = "redis-7-2-examples")]
fn test_replication_pause_job_yields_serve_no_clients_after_the_transaction_that_pauses()
-> Result<()> {
    let Some((mut primary, mut replica)) = primary_with_replica("keyspace_to_stream")? else {
        return Ok(());
    };
    let ping = ping_during_yield(&mut primary, |primary| {
        let ((), (), probe): ((), (), Vec<i64>) = redis::pipe()
            .atomic()
            .cmd("CLIENT")
            .arg(&["PAUSE", "20000", "WRITE"])
            .cmd("SET")
            .arg(&["plain", "1"])
            .cmd("notify_stream.job_probe")
            .arg("yield")
            .query(primary)?;
        assert_eq!(probe, [1, 0]);
        Ok(())
    })?;
    assert_eq!(ping, PingDuringYield::Waited);
    let counts: Vec<i64> = redis::cmd("notify_stream.probe_counts").query(&mut primary)?;
    assert_eq!(counts, [1, 1], "the job ran once");
    unpause(&mut primary)?;
    assert!(
        !primary.server_log()?.contains("job probe yield failed"),
        "the job yielded"
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

/// Outside a pause a job's yield serves clients as asked.
#[test]
#[cfg(feature = "redis-7-2-examples")]
fn test_replication_pause_lets_job_yields_serve_clients_outside_a_pause() -> Result<()> {
    let Some((mut primary, _replica)) = primary_with_replica("keyspace_to_stream")? else {
        return Ok(());
    };
    let ping = ping_during_yield(&mut primary, |primary| {
        let probe: Vec<i64> = redis::cmd("notify_stream.job_probe")
            .arg("yield")
            .query(primary)?;
        assert_eq!(probe, [1, 0]);
        Ok(())
    })?;
    assert_eq!(ping, PingDuringYield::Busy);
    let counts: Vec<i64> = redis::cmd("notify_stream.probe_counts").query(&mut primary)?;
    assert_eq!(counts, [1, 1], "the job ran once");
    Ok(())
}

/// The error a call that would pause replication returns outside a
/// transaction (#235).
const PAUSE_REFUSED: &str = "ERR calls cannot pause replication outside a transaction";

/// Runs a call that would pause replication and checks that the SDK refused
/// it and that the server is still up. Without the refusal Redis aborts at the
/// end of the command and the query fails with a connection error.
fn assert_pause_refused(primary: &mut TestConnection, call: &mut redis::Cmd) -> Result<()> {
    let error = call
        .query::<Value>(primary)
        .expect_err("the pausing call ran");
    assert_eq!(error.code(), Some("ERR"), "{error}");
    assert!(
        error.to_string().contains(&PAUSE_REFUSED["ERR ".len()..]),
        "{error}"
    );
    survived(primary)
}

/// [`assert_pause_refused`] for the `replicate` example, which also reports
/// that Redis did not pause.
fn assert_replicate_pause_refused(
    primary: &mut TestConnection,
    call: &mut redis::Cmd,
) -> Result<()> {
    assert_pause_refused(primary, call)?;
    assert_eq!(paused(primary)?, 0);
    Ok(())
}

/// Checks that the server answers and logged no assertion.
fn survived(primary: &mut TestConnection) -> Result<()> {
    assert_eq!(redis::cmd("PING").query::<String>(primary)?, "PONG");
    let log = primary.server_log()?;
    ensure!(!log.contains("ASSERTION FAILED"), "server log: {log}");
    Ok(())
}

/// `CLIENT PAUSE` through `call` after `replicate_verbatim` queued the
/// command's own replication. Redis 6.2 and later abort when they propagate
/// it during the replica pause, in each mode; `ALL` is the default. Redis 6.0
/// has no modes and does not abort, and the SDK refuses there too.
#[test]
fn test_replication_pause_refuses_client_pause_after_replicate_verbatim() -> Result<()> {
    let mut primary = TestConnection::new("replicate");
    let modes: &[&[&str]] = if server_version(&mut primary)? >= (6, 2) {
        &[&["WRITE"], &["all"], &[]]
    } else {
        &[&[]]
    };
    let mut replica = primary.replica()?;
    for mode in modes {
        assert_replicate_pause_refused(
            &mut primary,
            redis::cmd("replicate.then_call")
                .arg(&["verbatim", "key", "client", "PAUSE", "20000"])
                .arg(*mode),
        )?;
    }
    acknowledge(&mut primary, &replica)?;
    // The replica ran each command that the primary refused to pause in,
    // and refused the pause too.
    assert_eq!(
        command_calls(&mut replica, "replicate.then_call")?,
        modes.len() as u64
    );
    assert_eq!(paused(&mut replica)?, 0);
    survived(&mut replica)
}

/// The same after a replicated `call_ext`.
#[test]
fn test_replication_pause_refuses_client_pause_after_a_replicated_call() -> Result<()> {
    let Some((mut primary, mut replica)) = primary_with_replica("replicate")? else {
        return Ok(());
    };
    assert_replicate_pause_refused(
        &mut primary,
        redis::cmd("replicate.then_call")
            .arg(&["call", "key", "CLIENT", "PAUSE", "20000", "WRITE"]),
    )?;
    acknowledge(&mut primary, &replica)?;
    assert_eq!(
        redis::cmd("GET")
            .arg("key")
            .query::<Option<String>>(&mut replica)?
            .as_deref(),
        Some("value")
    );
    Ok(())
}

/// Opening an expired key deletes it and queues a `DEL` for propagation, which
/// the module neither asked for nor can see. Redis 7.0 and later abort when
/// they propagate it during the pause; 6.2 propagates the deletion at once.
/// The SDK refuses on every version, so it refuses a pause with nothing
/// queued too.
#[test]
fn test_replication_pause_refuses_client_pause_after_a_lazy_expire() -> Result<()> {
    let Some((mut primary, mut replica)) = primary_with_replica("replicate")? else {
        return Ok(());
    };
    redis::cmd("DEBUG")
        .arg(&["SET-ACTIVE-EXPIRE", "0"])
        .query::<()>(&mut primary)?;
    redis::cmd("SET")
        .arg(&["expiring", "value", "PX", "1"])
        .query::<()>(&mut primary)?;
    acknowledge(&mut primary, &replica)?;
    // Replicas keep expired keys until the primary deletes them.
    assert_eq!(redis::cmd("DBSIZE").query::<i64>(&mut replica)?, 2);
    std::thread::sleep(Duration::from_millis(50));
    assert_replicate_pause_refused(
        &mut primary,
        redis::cmd("replicate.then_call")
            .arg(&["open", "expiring", "CLIENT", "PAUSE", "20000", "WRITE"]),
    )?;
    acknowledge(&mut primary, &replica)?;
    // The deletion reached the replica, which keeps only the marker key.
    assert_eq!(redis::cmd("DBSIZE").query::<i64>(&mut replica)?, 1);

    assert_replicate_pause_refused(
        &mut primary,
        redis::cmd("replicate.then_call")
            .arg(&["none", "key", "CLIENT", "PAUSE", "20000", "WRITE"]),
    )
}

/// `FAILOVER` pauses writes when it starts. `FAILOVER ABORT` does not, and
/// reaches Redis.
#[test]
fn test_replication_pause_refuses_failover_after_replicate_verbatim() -> Result<()> {
    let Some((mut primary, _replica)) = primary_with_replica("replicate")? else {
        return Ok(());
    };
    assert_replicate_pause_refused(
        &mut primary,
        redis::cmd("replicate.then_call").arg(&["verbatim", "key", "FAILOVER"]),
    )?;
    let error = redis::cmd("replicate.then_call")
        .arg(&["none", "key", "failover", "abort"])
        .query::<Value>(&mut primary)
        .expect_err("no failover is in progress");
    assert!(
        error.to_string().contains("No failover in progress"),
        "{error}"
    );
    let info: String = redis::cmd("INFO").arg("replication").query(&mut primary)?;
    assert!(info.lines().any(|line| line == "role:master"), "{info}");
    survived(&mut primary)
}

/// Other `CLIENT` subcommands reach Redis.
#[test]
fn test_replication_pause_lets_calls_run_other_client_subcommands() -> Result<()> {
    let mut primary = TestConnection::new("replicate");
    let reply: Value = redis::cmd("replicate.then_call")
        .arg(&["verbatim", "key", "CLIENT", "GETNAME"])
        .query(&mut primary)?;
    assert_eq!(reply, Value::Nil);
    if server_version(&mut primary)? >= (6, 2) {
        redis::cmd("replicate.then_call")
            .arg(&["verbatim", "key", "CLIENT", "UNPAUSE"])
            .query::<()>(&mut primary)?;
    }
    survived(&mut primary)
}

/// Inside `EXEC` Redis replicates the rest of the transaction during a pause
/// that the transaction started, so a call there may pause.
#[test]
fn test_replication_pause_lets_calls_pause_inside_a_transaction() -> Result<()> {
    let Some((mut primary, mut replica)) = primary_with_replica("replicate")? else {
        return Ok(());
    };
    let ((), ()): ((), ()) = redis::pipe()
        .atomic()
        .cmd("replicate.then_call")
        .arg(&["call", "key", "CLIENT", "PAUSE", "20000", "WRITE"])
        .cmd("SET")
        .arg(&["after", "value"])
        .query(&mut primary)?;
    assert_eq!(paused(&mut primary)?, 1);
    unpause(&mut primary)?;
    acknowledge(&mut primary, &replica)?;
    for key in ["key", "after"] {
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

/// `SHUTDOWN` without `NOW` pauses writes while it waits for lagging replicas,
/// and a blocking call can wait for it. `DEBUG SLEEP` keeps the replica from
/// acknowledging the write before it; Redis then aborts when it propagates
/// the replicated `SET` that the command queued. The SDK refuses whether or
/// not a replica lags. A plain call cannot wait, and Redis refuses `SHUTDOWN`
/// without `NOW` there itself.
#[test]
#[cfg(feature = "redis-7-2-examples")]
fn test_replication_pause_refuses_shutdown_through_blocking_calls() -> Result<()> {
    let Some((mut primary, replica)) = primary_with_replica("call")? else {
        return Ok(());
    };
    let mut sleeper = replica.connection()?;
    let sleep = std::thread::spawn(move || {
        redis::cmd("DEBUG")
            .arg(&["SLEEP", "2"])
            .query::<()>(&mut sleeper)
    });
    std::thread::sleep(Duration::from_millis(200));
    redis::cmd("SET")
        .arg(&["before", "value"])
        .query::<()>(&mut primary)?;
    assert_pause_refused(
        &mut primary,
        redis::cmd("call.blocking_after_set").arg(&["key", "SHUTDOWN", "NOSAVE"]),
    )?;
    sleep.join().expect("the DEBUG SLEEP thread panicked")?;
    let error = redis::cmd("call.blocking_after_set")
        .arg(&["key", "SHUTDOWN", "ABORT"])
        .query::<Value>(&mut primary)
        .expect_err("no shutdown is in progress");
    assert!(
        error.to_string().contains("No shutdown in progress"),
        "{error}"
    );
    acknowledge(&mut primary, &replica)?;
    survived(&mut primary)
}
