use std::collections::HashMap;
use std::time::Duration;

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

    // {"a":"..."} contributes eight structural bytes. Each NUL uses the
    // canonical six-byte JSON escape while the remaining characters use one.
    let escaped = 16usize;
    let boundary_ascii = 1024 * 1024 - 8 - escaped * 6;
    let at_boundary =
        format!("() => ({{a: 'x'.repeat({boundary_ascii}) + '\\0'.repeat({escaped})}})");
    assert_eq!(project(&mut con, "source", "boundary", at_boundary)?, 1);
    let over_boundary = format!(
        "() => ({{a: 'x'.repeat({}) + '\\0'.repeat({escaped})}})",
        boundary_ascii + 1
    );
    assert_rejected_unchanged(&mut con, "source", over_boundary)?;
    Ok(())
}

#[test]
fn exception_timeout_and_heap_refusal_do_not_mutate_destination() -> Result<()> {
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
    con.set_read_timeout(None)?;

    assert_rejected_unchanged(
        &mut con,
        "source",
        "() => ({value: 'x'.repeat(16 * 1024 * 1024)})",
    )?;
    Ok(())
}

#[test]
fn nested_get_and_hset_obey_the_callers_acl() -> Result<()> {
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
    let acknowledged: usize = redis::cmd("WAIT").arg(&[1, 5000]).query(&mut primary)?;
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
fn command_info_reports_write_and_key_metadata() -> Result<()> {
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
    Ok(())
}
