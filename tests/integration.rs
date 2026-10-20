use std::collections::HashMap;
use std::thread;
use std::time::Duration;
use std::time::SystemTime;

use crate::utils::TestConnection;
use anyhow::Context;
use anyhow::Result;
use redis::{RedisError, RedisResult, Value};
use redis_module::RedisValue;

mod utils;

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
        let Value::Bulk(top) = response else {
            panic!("scan array")
        };
        assert_eq!(top[0], Value::Int(2));
        let Value::Bulk(pairs) = &top[1] else {
            panic!("pair array")
        };
        for row in pairs {
            let Value::Bulk(pair) = row else {
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
            let Value::Bulk(top) = response else {
                panic!("scan array")
            };
            assert_eq!(top[0], Value::Int(1));
            assert_eq!(
                top[1],
                Value::Bulk(vec![Value::Bulk(vec![
                    Value::Data(b"f".to_vec()),
                    Value::Data(b"value".to_vec())
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
            assert!(redis::cmd("owner.read")
                .query::<Vec<Vec<u8>>>(&mut con)?
                .is_empty());
        }
    }
    assert!(redis::cmd("owner.lifecycle")
        .arg(2)
        .query::<Value>(&mut con)
        .is_err());
    assert!(redis::cmd("owner.lifecycle")
        .query::<Value>(&mut con)
        .is_err());
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
            let Value::Bulk(top) = response else {
                panic!("scan response must be an array")
            };
            let count: usize = redis::from_redis_value(&top[0])?;
            let Value::Bulk(rows) = &top[1] else {
                panic!("scan pairs must be an array")
            };
            let mut pairs = Vec::new();
            for row in rows {
                let Value::Bulk(pair) = row else {
                    panic!("pair must be an array")
                };
                assert_eq!(pair.len(), 2);
                let field: Vec<u8> = redis::from_redis_value(&pair[0])?;
                let value: Option<Vec<u8>> = if pair[1] == Value::Nil {
                    None
                } else {
                    Some(redis::from_redis_value(&pair[1])?)
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
            let Value::Bulk(top) = response else {
                panic!("scan response must be an array")
            };
            let again: usize = redis::from_redis_value(&top[0])?;
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
    assert!(error
        .detail()
        .unwrap_or_default()
        .contains("parse as UTF-8 string"));
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
    assert!(error
        .detail()
        .unwrap_or_default()
        .contains("parse as UTF-8 string"));
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
    assert!(error
        .detail()
        .unwrap_or_default()
        .contains("parse as UTF-8 string"));
    assert_eq!(
        redis::cmd("get_static_data").query::<String>(&mut con)?,
        "a\0b"
    );
    assert!(redis::cmd("set_static_data")
        .query::<Value>(&mut con)
        .is_err());
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
    assert!(error
        .detail()
        .unwrap_or_default()
        .contains("parse as UTF-8 string"));
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
    assert_eq!(res, res2);

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
            assert!(res
                .err()
                .unwrap()
                .to_string()
                .contains("RedisModule_GetCurrentCommandName is not available"));
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

#[test]
fn test_insertion_ordered_map() -> Result<()> {
    let mut con = TestConnection::new("response");

    // RESP2 receives the map as a flat array of keys and values, in order.
    let res: Vec<redis::Value> = redis::cmd("map.ordered").query(&mut con)?;
    assert_eq!(
        res,
        [
            redis::Value::Data(b"zebra".to_vec()),
            redis::Value::Int(1),
            redis::Value::Data(b"apple".to_vec()),
            redis::Value::Int(2),
            redis::Value::Data(b"mango".to_vec()),
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
        assert_eq!(res, Value::Data(b"v".to_vec()), "{command} GET");
        let res: Value = redis::cmd(command).arg(&["PING"]).query(&mut con)?;
        assert_eq!(res, Value::Status("PONG".to_owned()), "{command} PING");
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
    let text = |field: &str| redis::from_redis_value::<String>(&entry[field]).unwrap();
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
fn test_cron_event_hz() -> Result<()> {
    let mut con = TestConnection::new("server_events");

    redis::cmd("CONFIG")
        .arg(&["SET", "hz", "50"])
        .query::<()>(&mut con)?;
    // At 50 Hz a cron event follows within 20 ms; allow a second.
    let mut hz = 0;
    for _ in 0..100 {
        hz = redis::cmd("last_cron_hz").query(&mut con)?;
        if hz == 50 {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(hz, 50);

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

    assert_eq!(
        config_get(&mut con, "configuration.redis_string")?,
        "default"
    );
    config_set(&mut con, "configuration.redis_string", "new")?;
    assert_eq!(config_get(&mut con, "configuration.redis_string")?, "new");

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

/// The path of an example module built by `cargo test`.
fn example_module_path(module_name: &str) -> String {
    let extension = if cfg!(target_os = "macos") {
        "dylib"
    } else {
        "so"
    };
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    std::env::current_dir()
        .expect("current directory")
        .join(format!(
            "target/{profile}/examples/lib{module_name}.{extension}"
        ))
        .display()
        .to_string()
}

#[test]
fn test_configuration_registration_errors_fail_the_load() -> Result<()> {
    let mut con = TestConnection::new("hello");
    let module = example_module_path("configuration_registration");

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
fn test_configuration_load_errors_fail_the_load() -> Result<()> {
    let mut con = TestConnection::with_log_file("hello");
    let module = example_module_path("configuration");

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
        assert_eq!(redis::from_redis_value::<String>(&fields[0])?, name);
        assert_eq!(redis::from_redis_value::<i64>(&fields[1])?, arity);
        let flags: Vec<String> = redis::from_redis_value(&fields[2])?;
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

    let fields: HashMap<String, Value> = redis::from_redis_value(&res)?;
    assert_eq!(fields.len(), 11);
    let text = |s: &str| Value::Data(s.as_bytes().to_vec());
    let inner = |i| Value::Bulk(vec![text("i1"), Value::Int(i)]);
    for (name, expected) in [
        ("i", Value::Int(10)),
        ("i1", Value::Int(1)),
        ("u", Value::Int(20)),
        ("s", text("s")),
        (
            "v",
            Value::Bulk(vec![Value::Int(1), Value::Int(2), Value::Int(3)]),
        ),
        ("v2", Value::Bulk(vec![inner(1), inner(2)])),
        ("hash_map", Value::Bulk(vec![text("key"), text("val")])),
        ("hash_set", Value::Bulk(vec![text("key")])),
        ("ordered_map", Value::Bulk(vec![text("key"), inner(10)])),
        ("ordered_set", Value::Bulk(vec![text("key")])),
    ] {
        assert_eq!(fields.get(name), Some(&expected), "Reply field {name}");
    }
    assert_eq!(redis::from_redis_value::<f64>(&fields["f"])?, 1.1);
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
        assert_eq!(echoed, Value::Data(input.clone()), "ECHO {input:?}");

        redis::cmd("SET")
            .arg(key)
            .arg(input.as_slice())
            .query::<()>(&mut con)?;
        let value: Value = redis::cmd("call_reply.get").arg(key).query(&mut con)?;
        assert_eq!(value, Value::Data(input.clone()), "GET {input:?}");

        let direct: Value = redis::cmd("DUMP").arg(key).query(&mut con)?;
        let forwarded: Value = redis::cmd("call_reply.dump").arg(key).query(&mut con)?;
        assert_eq!(forwarded, direct, "DUMP of {input:?}");
    }

    let values: Value = redis::cmd("call_reply.mget").arg(&keys).query(&mut con)?;
    let expected = inputs.iter().cloned().map(Value::Data).collect();
    assert_eq!(values, Value::Bulk(expected));

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
