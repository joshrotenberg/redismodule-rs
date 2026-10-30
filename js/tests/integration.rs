use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use redis::Value;

mod support;

use support::TestConnection;

fn module_path() -> Result<String> {
    support::module_path("redis_module_js")
}

fn server() -> Result<TestConnection> {
    TestConnection::new(module_path()?)
}

fn project(
    con: &mut impl redis::ConnectionLike,
    source: impl redis::ToRedisArgs,
    destination: impl redis::ToRedisArgs,
    function: impl redis::ToRedisArgs,
) -> redis::RedisResult<i64> {
    redis::cmd("JS.PROJECT")
        .arg(source)
        .arg(destination)
        .arg(function)
        .query(con)
}

fn watch(
    con: &mut impl redis::ConnectionLike,
    name: impl redis::ToRedisArgs,
    source: impl redis::ToRedisArgs,
    destination: impl redis::ToRedisArgs,
    function: impl redis::ToRedisArgs,
) -> redis::RedisResult<String> {
    redis::cmd("JS.WATCH")
        .arg(name)
        .arg(source)
        .arg(destination)
        .arg(function)
        .query(con)
}

fn info(
    con: &mut impl redis::ConnectionLike,
    name: &str,
) -> redis::RedisResult<HashMap<String, String>> {
    redis::cmd("JS.INFO").arg(name).query(con)
}

fn hash(con: &mut impl redis::ConnectionLike, key: &str) -> Result<HashMap<String, String>> {
    Ok(redis::cmd("HGETALL").arg(key).query(con)?)
}

fn assert_rejected_unchanged(
    con: &mut impl redis::ConnectionLike,
    source: impl redis::ToRedisArgs,
    function: impl redis::ToRedisArgs,
) -> Result<()> {
    let before = hash(con, "destination")?;
    project(con, source, "destination", function).expect_err("projection unexpectedly succeeded");
    assert_eq!(hash(con, "destination")?, before);
    Ok(())
}

#[test]
fn projects_real_json_and_preserves_unrelated_hash_fields() -> Result<()> {
    let _serial = support::serial_native_test();
    let mut con = server()?;
    redis::cmd("SET")
        .arg("order:1")
        .arg(
            r#"{"customer":"Ada","items":[{"quantity":2,"price_cents":1250},{"quantity":1,"price_cents":500}]}"#,
        )
        .query::<()>(&mut con)?;
    redis::cmd("HSET")
        .arg("order:1:summary")
        .arg("retained")
        .arg("yes")
        .query::<i64>(&mut con)?;

    let function = include_str!("../examples/order-summary.js");
    assert_eq!(
        project(&mut con, "order:1", "order:1:summary", function)?,
        3
    );
    assert_eq!(
        hash(&mut con, "order:1:summary")?,
        HashMap::from([
            ("customer".into(), "Ada".into()),
            ("item_count".into(), "3".into()),
            ("total_cents".into(), "3000".into()),
            ("retained".into(), "yes".into()),
        ])
    );
    assert_eq!(
        project(&mut con, "order:1", "order:1:summary", function)?,
        0
    );
    Ok(())
}

#[test]
fn invalid_inputs_and_results_do_not_mutate_destination() -> Result<()> {
    let _serial = support::serial_native_test();
    let mut con = server()?;
    redis::cmd("HSET")
        .arg("destination")
        .arg("kept")
        .arg("value")
        .query::<i64>(&mut con)?;

    assert_rejected_unchanged(&mut con, "missing", "value => ({x: 'y'})")?;
    for (value, function) in [
        ("not json", "value => ({x: 'y'})"),
        ("[]", "value => ({x: 'y'})"),
        ("{}", "not valid javascript"),
        ("{}", "value => null"),
        ("{}", "value => []"),
        ("{}", "value => ({})"),
        ("{}", "value => ({nested: {}})"),
    ] {
        redis::cmd("SET")
            .arg("source")
            .arg(value)
            .query::<()>(&mut con)?;
        assert_rejected_unchanged(&mut con, "source", function)?;
    }

    redis::cmd("DEL").arg("source").query::<i64>(&mut con)?;
    redis::cmd("LPUSH")
        .arg("source")
        .arg("wrong type")
        .query::<i64>(&mut con)?;
    assert_rejected_unchanged(&mut con, "source", "value => ({x: 'y'})")?;

    redis::cmd("SET")
        .arg("source")
        .arg("{}")
        .query::<()>(&mut con)?;
    redis::cmd("SET")
        .arg("invalid-utf8")
        .arg([0xff_u8].as_slice())
        .query::<()>(&mut con)?;
    assert_rejected_unchanged(&mut con, "invalid-utf8", "value => ({x: 'y'})")?;
    assert_rejected_unchanged(&mut con, "source", [0xff_u8].as_slice())?;
    assert_rejected_unchanged(&mut con, "source", "x".repeat(64 * 1024 + 1))?;
    assert_rejected_unchanged(
        &mut con,
        "source",
        "() => { const o = {}; for (let i = 0; i < 1025; i++) o['f' + i] = 'v'; return o; }",
    )?;
    assert_rejected_unchanged(
        &mut con,
        "source",
        "() => ({value: 'x'.repeat(1024 * 1024)})",
    )?;

    redis::cmd("SET")
        .arg("large-source")
        .arg(format!(r#"{{"value":"{}"}}"#, "x".repeat(1024 * 1024)))
        .query::<()>(&mut con)?;
    assert_rejected_unchanged(&mut con, "large-source", "value => ({x: 'y'})")?;

    let before = hash(&mut con, "destination")?;
    project(
        &mut con,
        "destination",
        "destination",
        "value => ({x: 'y'})",
    )
    .expect_err("identical keys were accepted");
    assert_eq!(hash(&mut con, "destination")?, before);

    redis::cmd("SET")
        .arg("wrong-destination")
        .arg("string")
        .query::<()>(&mut con)?;
    let error = project(
        &mut con,
        "source",
        "wrong-destination",
        "value => ({x: 'y'})",
    )
    .expect_err("HSET accepted a string destination");
    assert_eq!(error.code(), Some("WRONGTYPE"));
    Ok(())
}

#[test]
fn validates_utf8_and_canonical_json_size_without_vm_hooks() -> Result<()> {
    let _serial = support::serial_native_test();
    let mut con = server()?;
    redis::cmd("SET")
        .arg("source")
        .arg("{}")
        .query::<()>(&mut con)?;
    redis::cmd("HSET")
        .arg("destination")
        .arg("kept")
        .arg("value")
        .query::<i64>(&mut con)?;

    assert_rejected_unchanged(
        &mut con,
        "source",
        r"() => { const output = {}; output['\uD800'] = 'value'; return output; }",
    )?;
    assert_rejected_unchanged(&mut con, "source", r"() => ({value: '\uD800'})")?;

    let function = r#"() => {
        Object.prototype.toJSON = () => "spoofed";
        Object.defineProperty(Object.prototype, "trap", {
            set() { throw new Error("host serialization invoked a setter"); }
        });
        const output = {};
        Object.defineProperty(output, "__proto__", {
            value: "prototype", enumerable: true
        });
        output.constructor = "constructor";
        output.toJSON = "field";
        Object.defineProperty(output, "trap", {value: "safe", enumerable: true});
        return output;
    }"#;
    assert_eq!(project(&mut con, "source", "magic", function)?, 4);
    assert_eq!(
        hash(&mut con, "magic")?,
        HashMap::from([
            ("__proto__".into(), "prototype".into()),
            ("constructor".into(), "constructor".into()),
            ("toJSON".into(), "field".into()),
            ("trap".into(), "safe".into()),
        ])
    );

    // This escaped output exceeds the JSON byte budget even when a script
    // attempts to hide it through a prototype hook. Exact-size success cases
    // are tested in host code, independently of the execution deadline.
    assert_rejected_unchanged(
        &mut con,
        "source",
        r#"() => { Object.prototype.toJSON = () => "x"; return {escaped: "\u0000".repeat(200000)}; }"#,
    )?;
    Ok(())
}

#[test]
fn exception_timeout_and_heap_refusal_do_not_mutate_destination() -> Result<()> {
    let _serial = support::serial_native_test();
    let mut con = server()?;
    redis::cmd("SET")
        .arg("source")
        .arg("{}")
        .query::<()>(&mut con)?;
    redis::cmd("HSET")
        .arg("destination")
        .arg("kept")
        .arg("value")
        .query::<i64>(&mut con)?;

    assert_rejected_unchanged(&mut con, "source", "() => { throw new Error('no'); }")?;

    con.set_read_timeout(Some(Duration::from_secs(2)))?;
    assert_rejected_unchanged(&mut con, "source", "() => { while (true) {} }")?;

    assert_rejected_unchanged(
        &mut con,
        "source",
        "() => ({value: 'x'.repeat(16 * 1024 * 1024)})",
    )?;
    Ok(())
}

#[test]
fn nested_get_and_hset_obey_the_callers_acl() -> Result<()> {
    let _serial = support::serial_native_test();
    let mut admin = server()?;
    redis::cmd("SET")
        .arg("source")
        .arg(r#"{"name":"Ada"}"#)
        .query::<()>(&mut admin)?;
    redis::cmd("ACL")
        .arg(&[
            "SETUSER",
            "projector",
            "reset",
            "on",
            ">secret",
            "~source",
            "~destination",
            "+js.project",
        ])
        .query::<()>(&mut admin)?;

    let mut caller = admin.connection()?;
    redis::cmd("AUTH")
        .arg(&["projector", "secret"])
        .query::<()>(&mut caller)?;
    project(
        &mut caller,
        "source",
        "destination",
        "value => ({name: value.name})",
    )
    .expect_err("projection bypassed the caller's GET permission");
    assert!(hash(&mut admin, "destination")?.is_empty());

    redis::cmd("ACL")
        .arg(&["SETUSER", "projector", "+get"])
        .query::<()>(&mut admin)?;
    project(
        &mut caller,
        "source",
        "destination",
        "value => ({name: value.name})",
    )
    .expect_err("projection bypassed the caller's HSET permission");
    assert!(hash(&mut admin, "destination")?.is_empty());

    redis::cmd("ACL")
        .arg(&["SETUSER", "projector", "+hset"])
        .query::<()>(&mut admin)?;
    assert_eq!(
        project(
            &mut caller,
            "source",
            "destination",
            "value => ({name: value.name})",
        )?,
        1
    );
    assert_eq!(
        hash(&mut admin, "destination")?,
        HashMap::from([("name".into(), "Ada".into())])
    );
    Ok(())
}

#[test]
fn declares_both_keys_and_survives_unload_reload() -> Result<()> {
    let _serial = support::serial_native_test();
    let mut con = server()?;
    let keys: Vec<String> = redis::cmd("COMMAND")
        .arg("GETKEYS")
        .arg("JS.PROJECT")
        .arg("source")
        .arg("destination")
        .arg("value => ({x: 'y'})")
        .query(&mut con)?;
    assert_eq!(keys, ["source", "destination"]);

    redis::cmd("MODULE")
        .arg(&["UNLOAD", "redis_module_js"])
        .query::<()>(&mut con)?;
    redis::cmd("MODULE")
        .arg("LOAD")
        .arg(module_path()?)
        .query::<()>(&mut con)?;
    redis::cmd("SET")
        .arg("source")
        .arg("{}")
        .query::<()>(&mut con)?;
    assert_eq!(
        project(
            &mut con,
            "source",
            "destination",
            "value => ({reloaded: 'yes'})"
        )?,
        1
    );
    Ok(())
}

#[test]
fn replication_replays_only_the_hset_effect() -> Result<()> {
    let _serial = support::serial_native_test();
    let mut primary = server()?;
    redis::cmd("SET")
        .arg("source")
        .arg("{}")
        .query::<()>(&mut primary)?;
    let mut replica = primary.replica()?;

    assert_eq!(
        project(
            &mut primary,
            "source",
            "destination",
            "() => ({token: String(Math.random())})"
        )?,
        1
    );
    // WAIT may block for five seconds. Keep a finite transport bound that
    // accommodates that explicit server-side timeout.
    primary.set_read_timeout(Some(Duration::from_secs(6)))?;
    let acknowledged: usize = redis::cmd("WAIT").arg(&[1, 5000]).query(&mut primary)?;
    primary.set_read_timeout(Some(Duration::from_secs(2)))?;
    ensure!(
        acknowledged == 1,
        "replica did not acknowledge the projection: {}",
        replica.server_log()?
    );
    let expected: String = redis::cmd("HGET")
        .arg(&["destination", "token"])
        .query(&mut primary)?;
    let actual: String = redis::cmd("HGET")
        .arg(&["destination", "token"])
        .query(&mut replica)?;
    assert_eq!(
        actual, expected,
        "JavaScript must not run again on the replica"
    );

    let command_stats: String = redis::cmd("INFO")
        .arg("commandstats")
        .query(&mut replica)
        .context("reading replica command statistics")?;
    assert!(
        command_stats
            .lines()
            .all(|line| !line.starts_with("cmdstat_js.project:")),
        "the module command was replayed instead of HSET:\n{command_stats}"
    );
    assert!(
        command_stats
            .lines()
            .any(|line| line.starts_with("cmdstat_hset:")),
        "replica did not replay an HSET:\n{command_stats}"
    );
    Ok(())
}

#[test]
fn reactive_replication_sends_hset_without_running_js_on_replica() -> Result<()> {
    let _serial = support::serial_native_test();
    let mut primary = server()?;
    assert_eq!(
        watch(
            &mut primary,
            "replicated",
            "source",
            "destination",
            "() => ({token: String(Math.random())})",
        )?,
        "OK"
    );
    let mut replica = primary.replica()?;
    redis::cmd("SET")
        .arg(&["source", "{}"])
        .query::<()>(&mut primary)?;
    primary.set_read_timeout(Some(Duration::from_secs(6)))?;
    let acknowledged: usize = redis::cmd("WAIT").arg(&[1, 5000]).query(&mut primary)?;
    primary.set_read_timeout(Some(Duration::from_secs(2)))?;
    ensure!(
        acknowledged == 1,
        "replica did not acknowledge reactive HSET"
    );
    let expected: String = redis::cmd("HGET")
        .arg(&["destination", "token"])
        .query(&mut primary)?;
    let actual: String = redis::cmd("HGET")
        .arg(&["destination", "token"])
        .query(&mut replica)?;
    assert_eq!(actual, expected);
    let stats: String = redis::cmd("INFO").arg("commandstats").query(&mut replica)?;
    assert!(!stats.lines().any(|line| line.starts_with("cmdstat_js.")));
    assert!(stats.lines().any(|line| line.starts_with("cmdstat_hset:")));
    Ok(())
}

#[test]
fn command_info_reports_write_and_key_metadata() -> Result<()> {
    let _serial = support::serial_native_test();
    let mut con = server()?;
    let info: Value = redis::cmd("COMMAND")
        .arg("INFO")
        .arg("JS.PROJECT")
        .query(&mut con)?;
    let debug = format!("{info:?}").to_ascii_lowercase();
    assert!(debug.contains("write"), "missing write flag: {debug}");
    assert!(debug.contains("denyoom"), "missing deny-oom flag: {debug}");
    assert!(
        debug.contains("begin_search") && debug.contains("lastkey"),
        "missing key specification: {debug}"
    );
    let watch_info: Value = redis::cmd("COMMAND")
        .arg("INFO")
        .arg("JS.WATCH")
        .query(&mut con)?;
    let watch_debug = format!("{watch_info:?}").to_ascii_lowercase();
    for expected in ["ro", "access", "rw", "update", "insert"] {
        assert!(
            watch_debug.contains(expected),
            "JS.WATCH metadata lacks {expected}: {watch_debug}"
        );
    }
    Ok(())
}

#[test]
fn watches_update_automatically_coalesce_and_keep_last_good_output() -> Result<()> {
    let _serial = support::serial_native_test();
    let mut con = server()?;
    redis::cmd("HSET")
        .arg(&["summary", "retained", "yes"])
        .query::<i64>(&mut con)?;
    let registration_started = Instant::now();
    assert_eq!(
        watch(
            &mut con,
            "orders",
            "order",
            "summary",
            "value => ({customer: value.customer, total: String(value.total)})"
        )?,
        "OK"
    );
    let registration_elapsed = registration_started.elapsed();
    assert!(!hash(&mut con, "summary")?.contains_key("customer"));

    redis::cmd("MULTI").query::<String>(&mut con)?;
    redis::cmd("SET")
        .arg(&["order", r#"{"customer":"first","total":1}"#])
        .query::<Value>(&mut con)?;
    redis::cmd("SET")
        .arg(&["order", r#"{"customer":"Ada","total":30}"#])
        .query::<Value>(&mut con)?;
    let update_started = Instant::now();
    redis::cmd("EXEC").query::<Value>(&mut con)?;
    let update_elapsed = update_started.elapsed();
    eprintln!(
        "reactive engine startup/registration: {registration_elapsed:?}; transaction-to-projection reply: {update_elapsed:?}"
    );
    assert_eq!(
        hash(&mut con, "summary")?,
        HashMap::from([
            ("customer".into(), "Ada".into()),
            ("total".into(), "30".into()),
            ("retained".into(), "yes".into()),
        ])
    );
    let status = info(&mut con, "orders")?;
    assert_eq!(status.get("runs").map(String::as_str), Some("1"));
    assert_eq!(status.get("successes").map(String::as_str), Some("1"));
    assert_eq!(status.get("coalesced").map(String::as_str), Some("1"));
    assert_eq!(status.get("pending").map(String::as_str), Some("0"));

    redis::cmd("SET")
        .arg(&["order", "not-json"])
        .query::<()>(&mut con)?;
    assert_eq!(
        hash(&mut con, "summary")?
            .get("customer")
            .map(String::as_str),
        Some("Ada")
    );
    let failed = info(&mut con, "orders")?;
    assert_eq!(failed.get("failures").map(String::as_str), Some("1"));
    assert!(!failed.get("last_error").unwrap().is_empty());

    redis::cmd("DEL").arg("order").query::<i64>(&mut con)?;
    assert_eq!(
        hash(&mut con, "summary")?
            .get("customer")
            .map(String::as_str),
        Some("Ada")
    );
    assert_eq!(
        info(&mut con, "orders")?
            .get("failures")
            .map(String::as_str),
        Some("2")
    );
    Ok(())
}

#[test]
fn watch_count_limit_rejects_without_touching_keys() -> Result<()> {
    let _serial = support::serial_native_test();
    let mut con = server()?;
    for index in 0..64 {
        assert_eq!(
            watch(
                &mut con,
                format!("watch-{index}"),
                format!("source-{index}"),
                format!("destination-{index}"),
                "() => ({value: 'ok'})",
            )?,
            "OK"
        );
    }
    watch(
        &mut con,
        "overflow",
        "overflow-source",
        "overflow-destination",
        "() => ({value: 'bad'})",
    )
    .expect_err("the 65th watch was accepted");
    assert_eq!(
        redis::cmd("EXISTS")
            .arg(&["overflow-source", "overflow-destination"])
            .query::<i64>(&mut con)?,
        0
    );
    Ok(())
}

#[test]
fn watch_names_databases_collisions_cancellation_and_reload_are_isolated() -> Result<()> {
    let _serial = support::serial_native_test();
    let mut con = server()?;
    assert_eq!(
        watch(
            &mut con,
            "same",
            "source",
            "destination",
            "v => ({v: String(v.v)})"
        )?,
        "OK"
    );
    watch(
        &mut con,
        "same",
        "other",
        "other-destination",
        "v => ({v: 'x'})",
    )
    .expect_err("duplicate name was replaced");
    watch(
        &mut con,
        "overlap",
        "destination",
        "third",
        "v => ({v: 'x'})",
    )
    .expect_err("a projection chain was accepted");
    watch(&mut con, "equal", "equal", "equal", "v => ({v: 'x'})")
        .expect_err("equal keys were accepted");
    watch(&mut con, "", "a", "b", "v => ({v: 'x'})").expect_err("empty name accepted");
    watch(&mut con, "x".repeat(129), "a", "b", "v => ({v: 'x'})").expect_err("long name accepted");

    redis::cmd("SELECT").arg(1).query::<()>(&mut con)?;
    assert_eq!(
        watch(
            &mut con,
            "same",
            "source",
            "destination",
            "v => ({v: 'db1'})"
        )?,
        "OK"
    );
    redis::cmd("SET")
        .arg(&["source", "{}"])
        .query::<()>(&mut con)?;
    assert_eq!(
        hash(&mut con, "destination")?.get("v").map(String::as_str),
        Some("db1")
    );
    redis::cmd("SELECT").arg(0).query::<()>(&mut con)?;
    assert!(hash(&mut con, "destination")?.is_empty());

    redis::cmd("MULTI").query::<String>(&mut con)?;
    redis::cmd("SET")
        .arg(&["source", "{}"])
        .query::<Value>(&mut con)?;
    redis::cmd("JS.UNWATCH")
        .arg("same")
        .query::<Value>(&mut con)?;
    redis::cmd("JS.WATCH")
        .arg(&["same", "source", "destination", "v => ({v: 'new'})"])
        .query::<Value>(&mut con)?;
    redis::cmd("EXEC").query::<Value>(&mut con)?;
    assert!(hash(&mut con, "destination")?.is_empty());
    redis::cmd("SET")
        .arg(&["source", "{}"])
        .query::<()>(&mut con)?;
    assert_eq!(
        hash(&mut con, "destination")?.get("v").map(String::as_str),
        Some("new")
    );

    redis::cmd("MODULE")
        .arg(&["UNLOAD", "redis_module_js"])
        .query::<()>(&mut con)?;
    redis::cmd("MODULE")
        .arg("LOAD")
        .arg(module_path()?)
        .query::<()>(&mut con)?;
    redis::cmd("JS.INFO")
        .arg("same")
        .query::<Value>(&mut con)
        .expect_err("watch survived reload");
    Ok(())
}

#[test]
fn watch_uses_owner_acl_and_declares_keys() -> Result<()> {
    let _serial = support::serial_native_test();
    let mut admin = server()?;
    redis::cmd("ACL")
        .arg(&[
            "SETUSER",
            "owner",
            "reset",
            "on",
            ">secret",
            "%R~source",
            "%W~destination",
            "+js.watch",
            "+js.unwatch",
            "+js.info",
            "+get",
            "+hset",
        ])
        .query::<()>(&mut admin)?;
    let mut caller = admin.connection()?;
    redis::cmd("AUTH")
        .arg(&["owner", "secret"])
        .query::<()>(&mut caller)?;
    assert_eq!(
        watch(
            &mut caller,
            "owned",
            "source",
            "destination",
            "v => ({v: String(v.v)})"
        )?,
        "OK"
    );
    redis::cmd("SET")
        .arg(&["source", r#"{"v":1}"#])
        .query::<()>(&mut admin)?;
    assert_eq!(
        hash(&mut admin, "destination")?
            .get("v")
            .map(String::as_str),
        Some("1")
    );

    redis::cmd("JS.INFO")
        .arg("owned")
        .query::<Value>(&mut admin)
        .expect_err("another user inspected owner diagnostics");
    redis::cmd("ACL")
        .arg(&["SETUSER", "owner", "-get", "-hset"])
        .query::<()>(&mut admin)?;
    redis::cmd("SET")
        .arg(&["source", r#"{"v":2}"#])
        .query::<()>(&mut admin)?;
    assert_eq!(
        hash(&mut admin, "destination")?
            .get("v")
            .map(String::as_str),
        Some("1")
    );
    assert_eq!(
        redis::cmd("JS.UNWATCH")
            .arg("owned")
            .query::<String>(&mut caller)?,
        "OK"
    );

    redis::cmd("ACL")
        .arg(&["SETUSER", "owner", "+get", "+hset"])
        .query::<()>(&mut admin)?;
    assert_eq!(
        watch(
            &mut caller,
            "disabled",
            "source",
            "destination",
            "v => ({v: String(v.v)})"
        )?,
        "OK"
    );
    redis::cmd("ACL")
        .arg(&["SETUSER", "owner", "off"])
        .query::<()>(&mut admin)?;
    redis::cmd("SET")
        .arg(&["source", r#"{"v":3}"#])
        .query::<()>(&mut admin)?;
    assert_eq!(
        hash(&mut admin, "destination")?
            .get("v")
            .map(String::as_str),
        Some("1")
    );
    redis::cmd("ACL")
        .arg(&["SETUSER", "owner", "on"])
        .query::<()>(&mut admin)?;
    assert_eq!(
        info(&mut caller, "disabled")?
            .get("failures")
            .map(String::as_str),
        Some("1")
    );
    redis::cmd("ACL")
        .arg(&["DELUSER", "owner"])
        .query::<i64>(&mut admin)?;
    redis::cmd("SET")
        .arg(&["source", r#"{"v":4}"#])
        .query::<()>(&mut admin)?;
    assert_eq!(
        hash(&mut admin, "destination")?
            .get("v")
            .map(String::as_str),
        Some("1")
    );

    let keys: Vec<String> = redis::cmd("COMMAND")
        .arg("GETKEYS")
        .arg("JS.WATCH")
        .arg("name")
        .arg("source")
        .arg("destination")
        .arg("v => ({v: 'x'})")
        .query(&mut admin)?;
    assert_eq!(keys, ["source", "destination"]);
    Ok(())
}
