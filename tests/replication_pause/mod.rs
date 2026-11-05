//! Replication while Redis pauses writes (#220). Redis aborts at an assertion
//! when a module replicates during the pause; the SDK refuses instead.
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

/// A primary that propagates to a replica, with writes paused on the primary.
/// `None` on a server without `CLIENT PAUSE WRITE`.
fn paused_primary() -> Result<Option<(TestConnection, TestConnection)>> {
    let mut primary = TestConnection::new("replicate");
    let version = server_version(&mut primary)?;
    if version < (6, 2) {
        eprintln!("skipped: Redis {version:?} has no CLIENT PAUSE WRITE");
        return Ok(None);
    }
    let replica = primary.replica()?;
    assert_eq!(paused(&mut primary)?, 0);
    redis::cmd("CLIENT")
        .arg(&["PAUSE", "20000", "WRITE"])
        .query::<()>(&mut primary)?;
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
    assert_eq!(paused(primary)?, 0);
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
    let acknowledged: i64 = redis::cmd("WAIT").arg(&[1, 5000]).query(primary)?;
    assert_eq!(acknowledged, 1, "replica log: {}", replica.server_log()?);
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
    let acknowledged: i64 = redis::cmd("WAIT").arg(&[1, 5000]).query(&mut primary)?;
    assert_eq!(acknowledged, 1, "replica log: {}", replica.server_log()?);
    // The replica ran only the command replicated after the pause.
    assert_eq!(command_calls(&mut replica, "replicate.verbatim")?, 1);
    Ok(())
}

#[test]
fn test_replication_pause_refuses_replicated_calls_without_running_them() -> Result<()> {
    let Some((mut primary, mut replica)) = paused_primary()? else {
        return Ok(());
    };
    let error = redis::cmd("replicate.call")
        .arg(&["SET", "refused", "value"])
        .query::<Value>(&mut primary)
        .expect_err("a replicated call ran during the pause");
    assert_eq!(error.code(), Some("ERR"), "{error}");
    assert!(error.to_string().contains("pauses replication"), "{error}");
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
