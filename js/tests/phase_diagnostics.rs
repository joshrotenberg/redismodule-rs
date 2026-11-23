#![cfg(feature = "internal-phase-diagnostics")]

#[path = "../src/phase_diagnostics.rs"]
mod phase_diagnostics;
#[allow(dead_code)]
mod support;

use phase_diagnostics::format_record;
use support::phase_tail;

fn record(op: u32) -> String {
    let mut values = [0; 18];
    values[0] = 1;
    values[1] = op;
    values[2] = 495;
    values[7] = 123;
    format_record(&values).unwrap().as_str().to_owned()
}

#[test]
fn parser_accepts_only_canonical_numeric_schema() {
    let valid = record(1);
    assert_eq!(phase_tail::parse(valid.as_bytes(), 1), Some(valid.clone()));
    assert!(phase_tail::parse(valid.as_bytes(), 2).is_none());
    let invalid = [
        valid.replace("v=1", "v=2"),
        valid.replace("op=1", "op=3"),
        valid.replace("mask=495", "mask=512"),
        valid.replace("flags=0", "flags=8"),
        valid.replace("hit=0", "hit=2"),
        valid.replace("vm_err=0", "vm_err=2"),
        valid.replace("polls=0", "polls=4294967296"),
        valid.replace("polls=0", "polls=-1"),
        valid.replace("polls=0", "polls=+1"),
        valid.replace("polls=0", "polls=00"),
        valid.replace("polls=0", "polls="),
        valid.replace("polls=0", "polls=1.0"),
        valid.replace("polls=0 ", ""),
        valid.replace("polls=0", "polls=0 polls=0"),
        valid.replace("polls=0", "unknown=0"),
        valid.replace("v=1 op=1", "op=1 v=1"),
        format!("{valid} extra=1"),
        format!("{valid}\0"),
        format!("{valid}\r"),
        format!("{valid}\x1b[0m"),
        format!("{valid}{}", "x".repeat(384)),
        valid[..valid.len() - 1].to_owned(),
    ];
    for body in invalid {
        assert!(phase_tail::parse(body.as_bytes(), 1).is_none());
    }
    assert!(phase_tail::parse(&[255, 0], 1).is_none());
    // Mask distinguishes absent stamp 4 from zero-length run/result intervals.
    assert!(valid.contains("mask=495"));
    assert!(valid.contains("vm_us=123 run_us=0 result_us=0"));
}

#[test]
fn tail_parser_discards_cut_edges_noise_and_wrong_operations() {
    let valid = record(1);
    let other = record(2);
    let bytes = format!("prefix-secret {valid}\nnoise\nredis-prefix {valid}\n{other}\n{valid}");
    assert_eq!(
        phase_tail::records(bytes.as_bytes(), true, 1),
        vec![valid.clone()]
    );
    assert_eq!(
        phase_tail::records(bytes.as_bytes(), false, 1),
        vec![valid.clone(), valid]
    );
    assert!(phase_tail::records(b"\xff\0\x1bnoise\n", false, 1).is_empty());
    assert!(phase_tail::records(b"", false, 1).is_empty());
}

#[test]
fn file_reader_caps_bytes_and_never_echoes_raw_data() -> anyhow::Result<()> {
    let directory = std::env::temp_dir().join(format!("js-phase-reader-{}", std::process::id()));
    std::fs::create_dir(&directory)?;
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(directory.clone());
    let path = directory.join("owned.log");
    let valid = record(1);
    let mut bytes = vec![b'x'; phase_tail::WINDOW + 100];
    bytes.extend_from_slice(format!("\nsecret-prefix {valid}\nraw-secret\n{valid}").as_bytes());
    std::fs::write(&path, bytes)?;
    let (records, count) = phase_tail::read(&path, 1).unwrap();
    assert_eq!(count, phase_tail::WINDOW);
    assert_eq!(records, vec![valid.clone()]);
    assert_eq!(phase_tail::latest(&path, 1), valid);
    assert!(!records[0].contains("secret"));
    std::fs::write(&path, b"raw-secret\xff\0\n")?;
    assert_eq!(phase_tail::latest(&path, 1), phase_tail::UNAVAILABLE);
    std::fs::write(&path, b"")?;
    assert_eq!(phase_tail::latest(&path, 1), phase_tail::UNAVAILABLE);
    assert_eq!(
        phase_tail::latest(&directory.join("missing"), 1),
        phase_tail::UNAVAILABLE
    );
    assert_eq!(phase_tail::latest(&directory, 1), phase_tail::UNAVAILABLE);
    Ok(())
}

#[test]
fn native_timeout_preserves_error_destination_and_single_bounded_record() -> anyhow::Result<()> {
    let _serial = support::serial_native_test();
    let mut con = support::TestConnection::new(support::module_path("redis_module_js")?)?;
    redis::cmd("SET")
        .arg("source")
        .arg("{}")
        .query::<String>(&mut con)?;
    redis::cmd("HSET")
        .arg("destination")
        .arg("retained")
        .arg("yes")
        .query::<i64>(&mut con)?;
    let error = redis::cmd("JS.PROJECT")
        .arg("source")
        .arg("destination")
        .arg("() => { while (true) {} }")
        .query::<i64>(&mut con)
        .unwrap_err();
    assert_eq!(error.code(), Some("ERR"));
    assert_eq!(error.detail(), Some("JavaScript projection timed out"));
    let destination: std::collections::HashMap<String, String> =
        redis::cmd("HGETALL").arg("destination").query(&mut con)?;
    assert_eq!(
        destination,
        std::collections::HashMap::from([("retained".into(), "yes".into())])
    );
    let records = con.phase_records(2).map_err(anyhow::Error::msg)?;
    assert_eq!(records.len(), 1);
    assert!(records[0].len() <= phase_diagnostics::BODY_LIMIT);
    assert_eq!(con.latest_phase_record(2), records[0]);
    Ok(())
}
