//! Native regressions for issue166, kept separate from other integration lanes.
use super::utils::{self, TestConnection};
use anyhow::Result;
use redis::Value;
use std::collections::HashMap;

#[test]
fn correctness_next_i64_replies_with_an_err_code() -> Result<()> {
    let mut con = TestConnection::new("expire");
    let error = redis::cmd("expire.cmd")
        .arg(&["key", "not-an-integer"])
        .query::<Value>(&mut con)
        .unwrap_err();
    assert_eq!(error.code(), Some("ERR"), "{error}");
    assert!(error.to_string().contains("parse as integer"), "{error}");
    Ok(())
}

#[test]
fn correctness_expiry_overflow_leaves_existing_value_and_ttl_untouched() -> Result<()> {
    let mut con = TestConnection::new("expire");
    redis::cmd("SET")
        .arg(&["key", "value", "PX", "60000"])
        .query::<()>(&mut con)?;
    let overflow_seconds = i64::MAX / 1000;
    let result = redis::cmd("expire.cmd")
        .arg("key")
        .arg(overflow_seconds)
        .query::<Value>(&mut con);
    let value: Option<String> = redis::cmd("GET").arg("key").query(&mut con)?;
    assert!(
        result.is_err(),
        "overflow accepted: {result:?}, remaining value={value:?}"
    );
    assert_eq!(result.unwrap_err().code(), Some("ERR"));
    assert_eq!(value.as_deref(), Some("value"));
    assert!((1..=60000).contains(&redis::cmd("PTTL").arg("key").query::<i64>(&mut con)?));
    let conversion = redis::cmd("expire.cmd")
        .arg("key")
        .arg(i64::MAX)
        .query::<Value>(&mut con)
        .unwrap_err();
    assert_eq!(conversion.code(), Some("ERR"));
    assert!((1..=60000).contains(&redis::cmd("PTTL").arg("key").query::<i64>(&mut con)?));
    redis::cmd("expire.cmd")
        .arg(&["key", "60"])
        .query::<()>(&mut con)?;
    assert!((1..=60000).contains(&redis::cmd("PTTL").arg("key").query::<i64>(&mut con)?));
    redis::cmd("expire.cmd")
        .arg(&["key", "-1"])
        .query::<()>(&mut con)?;
    assert_eq!(redis::cmd("PTTL").arg("key").query::<i64>(&mut con)?, -1);
    redis::cmd("expire.cmd")
        .arg(&["key", "0"])
        .query::<()>(&mut con)?;
    // Native expiration is lazy and compares millisecond snapshots. Observe
    // the recorded deadline rather than waiting for a scheduler/clock tick.
    let info: String = redis::cmd("INFO").arg("server").query(&mut con)?;
    if info
        .lines()
        .any(|line| line.starts_with("redis_version:6."))
    {
        let ttl: i64 = redis::cmd("PTTL").arg("key").query(&mut con)?;
        assert!(matches!(ttl, -2 | 0), "zero relative expiry: {ttl}");
    } else {
        let deadline: i64 = redis::cmd("PEXPIRETIME").arg("key").query(&mut con)?;
        let (seconds, micros): (i64, i64) = redis::cmd("TIME").query(&mut con)?;
        assert!(
            deadline == -2 || deadline <= seconds * 1000 + micros / 1000,
            "zero expiry deadline: {deadline}"
        );
    }
    Ok(())
}

#[test]
fn correctness_array_display_has_each_separator() -> Result<()> {
    let mut con = TestConnection::new("call_reply");
    for (script, expected) in [
        ("return {}", "[]"),
        ("return {1}", "[1]"),
        ("return {1,2}", "[1, 2]"),
        ("return {1,2,3}", "[1, 2, 3]"),
    ] {
        let result: String = redis::cmd("call_reply.display")
            .arg(&["EVAL", script, "0"])
            .query(&mut con)?;
        assert_eq!(result, expected);
    }
    Ok(())
}

fn assert_setlike_display(script: &str, expected: &[&str]) -> Result<()> {
    let mut con = TestConnection::new("call_reply");
    let result: String = redis::cmd("call_reply.display")
        .arg(&["EVAL", script, "0"])
        .query(&mut con)?;
    assert!(result.starts_with('{') && result.ends_with('}'), "{result}");
    let mut items: Vec<_> = result[1..result.len() - 1].split(", ").collect();
    items.sort_unstable();
    assert_eq!(items, expected, "{result}");
    Ok(())
}

#[test]
fn correctness_map_display_has_each_separator() -> Result<()> {
    assert_setlike_display(
        "return {map={[1]=10,[2]=20,[3]=30}}",
        &["1: 10", "2: 20", "3: 30"],
    )
}

#[test]
fn correctness_set_display_has_each_separator() -> Result<()> {
    assert_setlike_display(
        "return {set={[1]=true,[2]=true,[3]=true}}",
        &["1", "2", "3"],
    )
}

#[test]
fn correctness_enum_module_arguments_ignore_ascii_case() -> Result<()> {
    for input in ["VAL2", "vAl2", "val2"] {
        let mut con = TestConnection::new("hello");
        redis::cmd("MODULE")
            .arg("LOAD")
            .arg(utils::module_path("configuration")?)
            .arg(&["enum", input])
            .query::<()>(&mut con)?;
        let actual: HashMap<String, String> = redis::cmd("CONFIG")
            .arg(&["GET", "configuration.enum"])
            .query(&mut con)?;
        assert_eq!(actual["configuration.enum"], "Val2");
    }
    Ok(())
}

#[test]
fn correctness_enum_bitflags_are_refused_before_registration() -> Result<()> {
    let mut con = TestConnection::new("hello");
    let result = redis::cmd("MODULE")
        .arg("LOAD")
        .arg(utils::module_path("configuration_registration")?)
        .arg("bitflags")
        .query::<Value>(&mut con);
    assert!(
        result.is_err(),
        "unsupported BITFLAGS enum loaded: {result:?}"
    );
    assert!(
        con.server_log()?
            .contains("ERR enum configurations do not support BITFLAGS")
    );
    let configs: HashMap<String, String> = redis::cmd("CONFIG")
        .arg(&["GET", "configuration_registration.*"])
        .query(&mut con)?;
    assert!(configs.is_empty());
    Ok(())
}
