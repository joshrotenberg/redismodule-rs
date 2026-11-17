use super::utils::{self, TestConnection};
use anyhow::Result;
use std::time::Duration;

fn users(server: &mut TestConnection) -> Result<()> {
    redis::cmd("auth_async.users").query::<()>(server)?;
    Ok(())
}
fn stats(server: &mut TestConnection) -> Result<Vec<i64>> {
    Ok(redis::cmd("auth_async.stats").query(server)?)
}
fn drain(server: &mut TestConnection) -> Result<()> {
    redis::cmd("auth_async.drain").query::<()>(server)?;
    Ok(())
}
fn unload(server: &mut TestConnection) -> redis::RedisResult<()> {
    redis::cmd("MODULE")
        .arg(("UNLOAD", "auth_async"))
        .query(server)
}
fn positive() -> TestConnection {
    assert_eq!(
        std::env::var("TEST_REDIS_REQUIRE_ASYNC_AUTH").as_deref(),
        Ok("1"),
        "positive async fixture must be explicitly selected"
    );
    TestConnection::new("auth_async")
}
fn prepare(server: &mut TestConnection) -> Result<()> {
    users(server)?;
    let generation: i64 = redis::cmd("auth_async.prepare").query(server)?;
    assert!(generation > 0);
    Ok(())
}

#[test]
fn test_async_auth_unprepared_refuses_before_starter_and_retains_cleanup() -> Result<()> {
    let mut server = TestConnection::new("auth_async");
    users(&mut server)?;
    let mut client = server.connection()?;
    for mode in ["counted", "missing"] {
        redis::cmd("auth_async.probe")
            .arg(mode)
            .query::<()>(&mut server)?;
        redis::cmd("AUTH")
            .arg(("async", "test-password"))
            .query::<()>(&mut client)
            .expect_err("no hidden preparation");
        assert_eq!(
            redis::cmd("auth_async.probe")
                .arg("count")
                .query::<i64>(&mut server)?,
            0,
            "refusal precedes native block FFI"
        );
        redis::cmd("auth_async.probe")
            .arg("reset")
            .query::<()>(&mut server)?;
    }
    assert_eq!(stats(&mut server)?[0], 0);
    assert_eq!(stats(&mut server)?[2], 0, "abandoned starters are retained");
    unload(&mut server).expect_err("retained captures/users prevent unload");
    redis::cmd("auth_async.reap").query::<i64>(&mut server)?;
    assert_eq!(stats(&mut server)?[2], 2);
    drain(&mut server)?;
    unload(&mut server)?;
    Ok(())
}

#[test]
fn test_async_auth_runtime_preparation_obeys_explicit_fixture() -> Result<()> {
    let mut server = TestConnection::new("auth_async");
    let result = redis::cmd("auth_async.prepare").query::<i64>(&mut server);
    if std::env::var("TEST_REDIS_REQUIRE_ASYNC_AUTH").as_deref() == Ok("1") {
        assert!(result? > 0, "reviewed positive fixture must prepare");
    } else {
        result.expect_err("unsupported/unapproved fixture must refuse preparation");
    }
    assert_eq!(stats(&mut server)?[0], 0);
    unload(&mut server)?;
    Ok(())
}

#[test]
#[ignore = "requires the reviewed exact native Linux amd64 positive fixture"]
fn test_async_auth_positive_success_denial_stale_and_ordinary_cleanup() -> Result<()> {
    let mut server = positive();
    prepare(&mut server)?;
    let mut client = server.connection()?;
    client.set_read_timeout(Some(Duration::from_secs(3)))?;
    redis::cmd("AUTH")
        .arg(("async", "test-password"))
        .query::<()>(&mut client)?;
    assert_eq!(
        redis::cmd("ACL")
            .arg("WHOAMI")
            .query::<String>(&mut client)?,
        "async-active"
    );
    assert!(stats(&mut server)?[0] > 0, "real starter must execute");
    let hello: redis::Value = redis::cmd("HELLO")
        .arg(3)
        .arg(("AUTH", "async", "test-password"))
        .query(&mut client)?;
    assert!(
        matches!(hello, redis::Value::Map(_)),
        "Redis owns RESP3 HELLO AUTH"
    );
    redis::cmd("AUTH")
        .arg(("async", "wrong-password"))
        .query::<()>(&mut client)
        .expect_err("owned credential mismatch");

    for name in ["deny", "stale", "disabled", "drop", "panic"] {
        redis::cmd("AUTH")
            .arg((name, "test-password"))
            .query::<()>(&mut client)
            .expect_err("handled denial");
        assert_eq!(
            redis::cmd("ACL")
                .arg("WHOAMI")
                .query::<String>(&mut client)?,
            "async-active"
        );
    }
    redis::cmd("ACL")
        .arg(("SETUSER", "fallback", "on", "+@all", "~*", ">test-password"))
        .query::<()>(&mut server)?;
    redis::cmd("AUTH")
        .arg(("fallback", "test-password"))
        .query::<()>(&mut client)?;
    assert_eq!(
        redis::cmd("ACL")
            .arg("WHOAMI")
            .query::<String>(&mut client)?,
        "fallback"
    );
    unload(&mut server).expect_err("live identities/jobs/users prevent unload");
    drop(client);
    utils::wait_until(Duration::from_secs(3), "ordinary async drain", || {
        drain(&mut server)?;
        Ok(unload(&mut server).is_ok())
    })?;
    Ok(())
}

#[test]
#[ignore = "requires the reviewed exact native Linux amd64 positive fixture"]
fn test_async_auth_positive_timeout_and_late_completion_are_once_only() -> Result<()> {
    let mut server = positive();
    prepare(&mut server)?;
    let mut client = server.connection()?;
    client.set_read_timeout(Some(Duration::from_secs(3)))?;
    redis::cmd("AUTH")
        .arg(("hold", "test-password"))
        .query::<()>(&mut client)
        .expect_err("monotonic deadline denial");
    assert_eq!(stats(&mut server)?[0], 1);
    assert_eq!(stats(&mut server)?[3], 1);
    assert_eq!(redis::cmd("PING").query::<String>(&mut client)?, "PONG");
    redis::cmd("auth_async.release").query::<()>(&mut server)?;
    redis::cmd("auth_async.reap").query::<i64>(&mut server)?;
    assert_eq!(stats(&mut server)?[1], 0, "late target was never installed");
    assert_eq!(stats(&mut server)?[2], 1, "losing cleanup once");
    drop(client);
    utils::wait_until(Duration::from_secs(3), "timeout finalization", || {
        drain(&mut server)?;
        Ok(unload(&mut server).is_ok())
    })?;
    Ok(())
}

#[test]
#[ignore = "requires the reviewed exact native Linux amd64 positive fixture"]
fn test_async_auth_positive_disconnect_retains_producer_until_release() -> Result<()> {
    use std::io::Write;
    let mut server = positive();
    prepare(&mut server)?;
    let mut socket = std::net::TcpStream::connect(("127.0.0.1", server.port()))?;
    socket.write_all(b"*3\r\n$4\r\nAUTH\r\n$4\r\nhold\r\n$13\r\ntest-password\r\n")?;
    utils::wait_until(Duration::from_secs(2), "real native blocked AUTH", || {
        let list: String = redis::cmd("CLIENT").arg("LIST").query(&mut server)?;
        Ok(stats(&mut server)?[0] == 1
            && list
                .lines()
                .any(|l| l.contains("flags=b") && l.contains("cmd=auth")))
    })?;
    drop(socket);
    unload(&mut server).expect_err("native completion/producer ownership");
    redis::cmd("auth_async.release").query::<()>(&mut server)?;
    utils::wait_until(
        Duration::from_secs(3),
        "disconnect once-only cleanup",
        || {
            drain(&mut server)?;
            Ok(unload(&mut server).is_ok())
        },
    )?;
    Ok(())
}
