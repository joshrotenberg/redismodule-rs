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
fn positive(label: &str) -> Result<TestConnection> {
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        anyhow::bail!("positive fixture requires native Linux x86_64");
    }
    assert_eq!(
        std::env::var("TEST_REDIS_REQUIRE_ASYNC_AUTH").as_deref(),
        Ok("1"),
        "positive async fixture must be explicitly selected"
    );
    let mut server = TestConnection::new("auth_async");
    server.set_read_timeout(Some(Duration::from_secs(3)))?;
    server.set_write_timeout(Some(Duration::from_secs(3)))?;
    record_identity(&mut server, label)?;
    Ok(server)
}
fn record_identity(server: &mut TestConnection, label: &str) -> Result<()> {
    use sha2::{Digest, Sha256};
    let dir = std::path::PathBuf::from(std::env::var("TEST_ASYNC_AUTH_EVIDENCE_DIR")?);
    std::fs::create_dir_all(&dir)?;
    let info: String = redis::cmd("INFO").arg("server").query(server)?;
    let pid: u32 = info
        .lines()
        .find_map(|line| line.strip_prefix("process_id:"))
        .expect("native process ID")
        .trim()
        .parse()?;
    let actual = std::fs::read(format!("/proc/{pid}/exe"))?;
    let binary_sha = format!("{:x}", Sha256::digest(&actual));
    assert_eq!(
        binary_sha,
        "63f265f3b62da523a01e2dfed5f2f8f28b7f833d22dd98836360dd8a5203020d"
    );
    let module = std::path::PathBuf::from(utils::module_path("auth_async")?);
    let module_bytes = std::fs::read(&module)?;
    let module_sha = format!("{:x}", Sha256::digest(&module_bytes));
    let modules: redis::Value = redis::cmd("MODULE").arg("LIST").query(server)?;
    let redis::Value::Array(rows) = &modules else {
        anyhow::bail!("expected MODULE LIST array");
    };
    let loaded_path = rows
        .iter()
        .find_map(|row| {
            let redis::Value::Array(fields) = row else {
                return None;
            };
            let field = |name: &str| {
                fields.chunks_exact(2).find_map(|pair| {
                    (redis::from_redis_value::<String>(pair[0].clone())
                        .ok()
                        .as_deref()
                        == Some(name))
                    .then(|| redis::from_redis_value::<String>(pair[1].clone()).ok())
                    .flatten()
                })
            };
            (field("name").as_deref() == Some("auth_async"))
                .then(|| field("path"))
                .flatten()
        })
        .expect("loaded auth_async path");
    assert_eq!(
        std::fs::canonicalize(loaded_path)?,
        std::fs::canonicalize(&module)?
    );
    let module_archive = format!("module-{module_sha}.so");
    let retained = dir.join(&module_archive);
    if retained.exists() {
        assert_eq!(std::fs::read(&retained)?, module_bytes);
    } else {
        use std::io::Write;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&retained)?
            .write_all(&module_bytes)?;
    }
    std::fs::write(
        dir.join(format!("{label}-{}.txt", server.port())),
        format!(
            "port:{}\npid:{pid}\nactual_process_image_sha256:{binary_sha}\nmodule:{}\nmodule_sha256:{module_sha}\nmodule_archive:{module_archive}\nmodule_list:{modules:?}\n{info}",
            server.port(),
            module.display()
        ),
    )?;
    Ok(())
}
fn record_cleanup(mut server: TestConnection, label: &str) -> Result<()> {
    let info: String = redis::cmd("INFO").arg("server").query(&mut server)?;
    let pid: u32 = info
        .lines()
        .find_map(|line| line.strip_prefix("process_id:"))
        .expect("native process ID")
        .trim()
        .parse()?;
    let port = server.port();
    drop(server);
    assert!(
        !std::path::Path::new(&format!("/proc/{pid}")).exists(),
        "owned child must be killed and reaped"
    );
    let dir = std::path::PathBuf::from(std::env::var("TEST_ASYNC_AUTH_EVIDENCE_DIR")?);
    std::fs::write(
        dir.join(format!("{label}-{port}-cleanup.txt")),
        format!("pid:{pid}\nport:{port}\nowned_child_reaped:true\n"),
    )?;
    println!("ASYNC_POSITIVE_EXECUTED:{label}");
    Ok(())
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
        users(&mut server)?;
        redis::cmd("auth_async.probe")
            .arg("counted")
            .query::<()>(&mut server)?;
        let mut client = server.connection()?;
        redis::cmd("AUTH")
            .arg(("async", "test-password"))
            .query::<()>(&mut client)
            .expect_err("failed preparation cannot authorize blocking");
        assert_eq!(
            redis::cmd("auth_async.probe")
                .arg("count")
                .query::<i64>(&mut server)?,
            0
        );
        redis::cmd("auth_async.probe")
            .arg("reset")
            .query::<()>(&mut server)?;
        drain(&mut server)?;
    }
    assert_eq!(stats(&mut server)?[0], 0);
    unload(&mut server)?;
    Ok(())
}

#[test]
#[ignore = "requires the reviewed exact native Linux amd64 positive fixture"]
fn test_async_auth_positive_success_denial_stale_and_ordinary_cleanup() -> Result<()> {
    let mut server = positive("success")?;
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
    assert_eq!(
        stats(&mut server)?[1],
        0,
        "reauth consumer waits for ordinary reaping"
    );
    redis::cmd("auth_async.reap").query::<i64>(&mut server)?;
    assert_eq!(
        stats(&mut server)?[1],
        1,
        "prior async installation notified once"
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
    redis::cmd("MODULE")
        .arg("LOAD")
        .arg(utils::module_path("auth_async")?)
        .query::<()>(&mut server)?;
    record_identity(&mut server, "reload")?;
    users(&mut server)?;
    let started = stats(&mut server)?[0];
    let mut fresh = server.connection()?;
    redis::cmd("AUTH")
        .arg(("async", "test-password"))
        .query::<()>(&mut fresh)
        .expect_err("new load cannot reuse the old preparation receipt");
    assert_eq!(stats(&mut server)?[0], started);
    drop(fresh);
    utils::wait_until(Duration::from_secs(3), "reload explicit drain", || {
        drain(&mut server)?;
        Ok(unload(&mut server).is_ok())
    })?;
    record_cleanup(server, "success")
}

#[test]
#[ignore = "requires the reviewed exact native Linux amd64 positive fixture"]
fn test_async_auth_positive_timeout_and_late_completion_are_once_only() -> Result<()> {
    let mut server = positive("timeout")?;
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
    record_cleanup(server, "timeout")
}

#[test]
#[ignore = "requires the reviewed exact native Linux amd64 positive fixture"]
fn test_async_auth_positive_disconnect_retains_producer_until_release() -> Result<()> {
    use std::io::Write;
    let mut server = positive("disconnect")?;
    prepare(&mut server)?;
    let address = std::net::SocketAddr::from(([127, 0, 0, 1], server.port()));
    let mut socket = std::net::TcpStream::connect_timeout(&address, Duration::from_secs(3))?;
    socket.set_write_timeout(Some(Duration::from_secs(3)))?;
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
    record_cleanup(server, "disconnect")
}

#[test]
#[ignore = "requires the reviewed exact native Linux amd64 positive fixture"]
fn test_async_auth_positive_preparation_and_prepared_admission_observations() -> Result<()> {
    let mut server = positive("cost")?;
    server.set_read_timeout(Some(Duration::from_secs(30)))?;
    users(&mut server)?;
    let mut preparation_ns = Vec::new();
    for _ in 0..10 {
        let measured: Vec<i64> = redis::cmd("auth_async.observe")
            .arg("prepare")
            .query(&mut server)?;
        assert_eq!(measured.len(), 2);
        assert!(measured[0] > 0 && measured[1] >= 0);
        preparation_ns.push(measured[1]);
    }
    redis::cmd("auth_async.observe")
        .arg("start")
        .query::<()>(&mut server)?;
    let baseline_ns: Vec<i64> = redis::cmd("auth_async.observe")
        .arg("baseline")
        .query(&mut server)?;
    assert_eq!(baseline_ns.len(), 200);
    assert_eq!(
        redis::cmd("auth_async.observe")
            .arg("reset")
            .query::<i64>(&mut server)?,
        200
    );
    let mut client = server.connection()?;
    client.set_read_timeout(Some(Duration::from_secs(3)))?;
    fn reap_when_quiescent(server: &mut TestConnection) -> Result<()> {
        utils::wait_until(
            Duration::from_secs(3),
            "observation ordinary reaper",
            || match redis::cmd("auth_async.reap").query::<i64>(server) {
                Ok(_) => Ok(true),
                Err(error)
                    if error
                        .to_string()
                        .contains("asynchronous authentication is unavailable") =>
                {
                    Ok(false)
                }
                Err(error) => Err(error.into()),
            },
        )
    }
    for _ in 0..20 {
        redis::cmd("AUTH")
            .arg(("observe", "test-password"))
            .query::<()>(&mut client)?;
        reap_when_quiescent(&mut server)?;
    }
    assert_eq!(
        redis::cmd("auth_async.observe")
            .arg("reset")
            .query::<i64>(&mut server)?,
        20
    );
    for _ in 0..200 {
        redis::cmd("AUTH")
            .arg(("observe", "test-password"))
            .query::<()>(&mut client)?;
        reap_when_quiescent(&mut server)?;
    }
    let admission_ns: Vec<i64> = redis::cmd("auth_async.observe")
        .arg("samples")
        .query(&mut server)?;
    assert_eq!(admission_ns.len(), 200);
    assert!(admission_ns.iter().chain(&baseline_ns).all(|n| *n >= 0));
    assert_eq!(
        stats(&mut server)?[0],
        220,
        "each observed AUTH ran its real starter"
    );
    assert_eq!(
        redis::cmd("ACL")
            .arg("WHOAMI")
            .query::<String>(&mut client)?,
        "async-active"
    );
    let dir = std::path::PathBuf::from(std::env::var("TEST_ASYNC_AUTH_EVIDENCE_DIR")?);
    std::fs::write(
        dir.join("cost-observations.json"),
        format!(
            "{{\n\"schema\":1,\n\"preparation_scope\":\"Context::prepare_async_auth_runtime in ordinary command; no network time\",\n\"admission_scope\":\"begin_with initial clock return to next clock return after prepared admission and initial role checks; before job allocation/block/starter\",\n\"caveats\":\"Instrumented observations include clock/TLS hook overhead, not pure receipt cost or total AUTH. No timing threshold, baseline subtraction or latency guarantee. Executable loading and identity hashing precede preparation; no cache flush or cold-cache claim. Scheduling and CPU frequency uncontrolled.\",\n\"warmup_auths\":20,\n\"debug_assertions\":{},\n\"preparation_ns\":{preparation_ns:?},\n\"clock_bracket_baseline_ns\":{baseline_ns:?},\n\"prepared_admission_ns\":{admission_ns:?}\n}}\n",
            cfg!(debug_assertions)
        ),
    )?;
    redis::cmd("auth_async.observe")
        .arg("stop")
        .query::<()>(&mut server)?;
    assert_eq!(
        redis::cmd("auth_async.observe")
            .arg("status")
            .query::<Vec<i64>>(&mut server)?,
        vec![0, 1, 0]
    );
    drop(client);
    utils::wait_until(Duration::from_secs(3), "observation finalization", || {
        drain(&mut server)?;
        Ok(unload(&mut server).is_ok())
    })?;
    redis::cmd("MODULE")
        .arg("LOAD")
        .arg(utils::module_path("auth_async")?)
        .query::<()>(&mut server)?;
    record_identity(&mut server, "cost-reload")?;
    assert_eq!(
        redis::cmd("auth_async.observe")
            .arg("status")
            .query::<Vec<i64>>(&mut server)?,
        vec![0, 0, 0],
        "empty unload restores the slot; reload starts without observer storage"
    );
    unload(&mut server)?;
    record_cleanup(server, "cost")
}
