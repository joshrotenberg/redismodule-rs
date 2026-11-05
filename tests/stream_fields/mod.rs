use super::utils::TestConnection;
use anyhow::Result;

fn seed(con: &mut TestConnection) -> Result<()> {
    for id in ["1-0", "2-0"] {
        redis::cmd("XADD")
            .arg(&["s", id, "one", "1", "two", "2", "three", "3"])
            .query::<String>(con)?;
    }
    Ok(())
}

#[test]
fn stream_fields_native_output_request_baseline() -> Result<()> {
    let mut con = TestConnection::new("stream");
    seed(&mut con)?;
    for reverse in [0, 1] {
        for (mode, expected) in [
            ("all", vec![2, 6, 12]),
            ("skip", vec![2, 6, 0]),
            ("entry_zero", vec![2, 6, 0]),
            ("entry", vec![2, 6, 4]),
            ("drop", vec![2, 6, 4]),
            ("forget", vec![2, 6, 4]),
            ("alternate", vec![2, 6, 8]),
        ] {
            let counts: Vec<i64> = redis::cmd("STREAM_FIELD_COST")
                .arg("s")
                .arg(mode)
                .arg(reverse)
                .query(&mut con)?;
            eprintln!("{mode}, reverse={reverse}: {counts:?}");
            assert_eq!(counts, expected);
            assert_eq!(redis::cmd("XLEN").arg("s").query::<i64>(&mut con)?, 2);
        }
    }
    Ok(())
}

type EntryRow = (String, Vec<Vec<u8>>, usize);

#[expect(clippy::too_many_arguments)] // Mirrors the example command options.
fn entries(
    con: &mut TestConnection,
    reverse: i64,
    from: &str,
    to: &str,
    exclusive: i64,
    take: i64,
    mode: &str,
    am: i64,
) -> Result<Vec<EntryRow>> {
    Ok(redis::cmd("STREAM_ENTRIES")
        .arg("s")
        .arg(reverse)
        .arg(from)
        .arg(to)
        .arg(exclusive)
        .arg(take)
        .arg(mode)
        .arg(am)
        .query(con)?)
}

#[test]
fn stream_fields_partial_drop_skip_forget_and_legacy_interleave() -> Result<()> {
    let mut con = TestConnection::new("stream");
    seed(&mut con)?;
    for am in [0, 1] {
        for reverse in [0, 1] {
            for mode in ["skip", "drop", "forget", "alternate"] {
                for take in [0, 1, 10] {
                    let rows = entries(&mut con, reverse, "_", "_", 0, take, mode, am)?;
                    assert_eq!(rows.len(), 2);
                    let ids: Vec<_> = rows.iter().map(|row| row.0.as_str()).collect();
                    assert_eq!(
                        ids,
                        if reverse == 0 {
                            vec!["1-0", "2-0"]
                        } else {
                            vec!["2-0", "1-0"]
                        }
                    );
                    for (index, (_, fields, skipped)) in rows.iter().enumerate() {
                        let wanted = if mode == "alternate" && index % 2 == 1 {
                            3
                        } else {
                            usize::try_from(take)?.min(3)
                        };
                        let all: Vec<Vec<u8>> = [
                            b"one".to_vec(),
                            b"1".to_vec(),
                            b"two".to_vec(),
                            b"2".to_vec(),
                            b"three".to_vec(),
                            b"3".to_vec(),
                        ]
                        .into();
                        assert_eq!(fields, &all[..2 * wanted]);
                        assert_eq!(*skipped, 3 - wanted);
                    }
                    assert_eq!(redis::cmd("XLEN").arg("s").query::<i64>(&mut con)?, 2);
                }
            }
        }
    }
    Ok(())
}

#[test]
fn stream_fields_ranges_reverse_empty_and_binary_owners() -> Result<()> {
    let mut con = TestConnection::new("stream");
    seed(&mut con)?;
    redis::cmd("XADD")
        .arg(&["s", "3-0", "one", "3"])
        .query::<String>(&mut con)?;
    for reverse in [0, 1] {
        let rows = entries(&mut con, reverse, "1-0", "3-0", 1, 1, "drop", 0)?;
        assert_eq!(
            rows.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(),
            ["2-0"]
        );
        assert!(entries(&mut con, reverse, "10-0", "20-0", 0, 1, "skip", 0)?.is_empty());
    }
    redis::cmd("DEL").arg("s").query::<()>(&mut con)?;
    redis::cmd("XADD")
        .arg("s")
        .arg("1-0")
        .arg(b"field\0name".as_slice())
        .arg(b"value\xff".as_slice())
        .query::<String>(&mut con)?;
    for am in [0, 1] {
        let rows = entries(&mut con, 0, "_", "_", 0, 1, "drop", am)?;
        assert_eq!(rows[0].1, [b"field\0name".to_vec(), b"value\xff".to_vec()]);
    }
    redis::cmd("XDEL")
        .arg(&["s", "1-0"])
        .query::<i64>(&mut con)?;
    assert!(entries(&mut con, 0, "_", "_", 0, 1, "drop", 0)?.is_empty());
    Ok(())
}

#[test]
fn stream_fields_pointer_borrows_close_exactly_one_owner() -> Result<()> {
    let mut con = TestConnection::new("stream");
    redis::cmd("SET")
        .arg("plain")
        .arg(b"value\0\xff".as_slice())
        .query::<()>(&mut con)?;
    for mode in ["read", "write"] {
        let (kind, value, closes): (i64, Vec<u8>, i64) = redis::cmd("STREAM_KEY_POINTER")
            .arg(&["plain", mode])
            .query(&mut con)?;
        assert_eq!(kind, redis_module::raw::REDISMODULE_KEYTYPE_STRING as i64);
        assert_eq!(value, b"value\0\xff");
        assert_eq!(closes, 2, "initial owner and reopened owner, {mode}");
        assert_eq!(
            redis::cmd("GET").arg("plain").query::<Vec<u8>>(&mut con)?,
            b"value\0\xff"
        );
    }
    Ok(())
}

#[test]
fn stream_fields_native_id_parser_numeric_and_command_markers() -> Result<()> {
    use redis::Value;
    let mut con = TestConnection::new("stream");
    seed(&mut con)?;
    redis::cmd("XGROUP")
        .arg(&["CREATE", "s", "g", "0"])
        .query::<()>(&mut con)?;
    for (input, expected) in [
        ("0", "0-0"),
        ("1", "1-0"),
        ("1-2", "1-2"),
        (
            "18446744073709551615-18446744073709551615",
            "18446744073709551615-18446744073709551615",
        ),
    ] {
        assert_eq!(
            redis::cmd("STREAM_PARSE_ID")
                .arg(input)
                .query::<String>(&mut con)?,
            expected
        );
        redis::cmd("XREADGROUP")
            .arg(&["GROUP", "g", "c", "STREAMS", "s", input])
            .query::<Value>(&mut con)?;
    }
    for input in [
        "",
        "not-an-id",
        "1--1",
        "1-2-3",
        "18446744073709551616-0",
        "1-*",
        "$",
        "(1-0",
    ] {
        let error = redis::cmd("STREAM_PARSE_ID")
            .arg(input)
            .query::<Value>(&mut con)
            .unwrap_err();
        assert_eq!(error.code(), Some("ERR"));
        assert!(
            redis::cmd("XREADGROUP")
                .arg(&["GROUP", "g", "c", "STREAMS", "s", input])
                .query::<Value>(&mut con)
                .is_err(),
            "{input}"
        );
    }
    for (input, expected) in [
        ("-", "0-0"),
        ("+", "18446744073709551615-18446744073709551615"),
    ] {
        assert_eq!(
            redis::cmd("STREAM_PARSE_ID")
                .arg(input)
                .query::<String>(&mut con)?,
            expected
        );
        assert!(
            redis::cmd("XREADGROUP")
                .arg(&["GROUP", "g", "c", "STREAMS", "s", input])
                .query::<Value>(&mut con)
                .is_err()
        );
    }
    assert!(
        redis::cmd("STREAM_PARSE_ID")
            .arg(">")
            .query::<Value>(&mut con)
            .is_err()
    );
    redis::cmd("XREADGROUP")
        .arg(&["GROUP", "g", "c", "STREAMS", "s", ">"])
        .query::<Value>(&mut con)?;
    Ok(())
}

#[test]
fn stream_fields_parser_missing_api_recovers_without_fallback() -> Result<()> {
    use redis::Value;
    let mut con = TestConnection::new("stream");
    let error = redis::cmd("STREAM_PARSE_ID")
        .arg(&["1-2", "missing"])
        .query::<Value>(&mut con)
        .unwrap_err();
    assert_eq!(error.code(), Some("ERR"));
    assert!(error.to_string().contains("RedisModule_StringToStreamID"));
    assert_eq!(
        redis::cmd("STREAM_PARSE_ID")
            .arg("1-2")
            .query::<String>(&mut con)?,
        "1-2"
    );
    Ok(())
}
