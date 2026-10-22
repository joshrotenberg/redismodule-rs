use anyhow::Result;

use super::utils::TestConnection;

fn assert_reentrant_unload_refused(mode: &str, expected: [i64; 6]) -> Result<()> {
    let mut con = TestConnection::new("configuration_ownership");
    let observed: Vec<i64> = redis::cmd("config_ownership.probe")
        .arg(mode)
        .query(&mut con)?;
    assert_eq!(
        observed, expected,
        "{mode}: unload must be refused before user deinit"
    );
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    let victim: Vec<String> = redis::cmd("CONFIG")
        .arg(&["GET", "configuration_ownership.victim"])
        .query(&mut con)?;
    assert_eq!(victim, ["configuration_ownership.victim", "1"]);
    redis::cmd("MODULE")
        .arg(&["UNLOAD", "configuration_ownership"])
        .query::<()>(&mut con)?;
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    Ok(())
}

#[test]
fn configuration_direct_callback_refuses_reentrant_unload_before_deinit() -> Result<()> {
    assert_reentrant_unload_refused("direct", [1, 0, 0, 1, 0, 1])
}

#[test]
fn configuration_native_later_apply_refuses_reentrant_unload_before_deinit() -> Result<()> {
    assert_reentrant_unload_refused("later", [1, 1, 1, 1, 0, 1])
}

fn read_drop_markers(path: &std::path::Path) -> Result<Vec<u8>> {
    match std::fs::read(path) {
        Ok(mut markers) => {
            markers.sort_unstable();
            Ok(markers)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}

#[test]
fn configuration_observer_captures_drop_once_only_after_successful_unload() -> Result<()> {
    let mut con = TestConnection::new("configuration_ownership");
    let module = super::utils::module_path("configuration_ownership")?;
    redis::cmd("MODULE")
        .arg(&["UNLOAD", "configuration_ownership"])
        .query::<()>(&mut con)?;
    for mode in [
        "owned",
        "deinit-refuse",
        "deinit-panic",
        "deinit-read",
        "drop-panic",
        "drop-payload-panic",
    ] {
        let path = std::env::temp_dir().join(format!(
            "sdk-config-owned-{}-{}-{mode}",
            std::process::id(),
            con.port()
        ));
        let _ = std::fs::remove_file(&path);
        redis::cmd("MODULE")
            .arg("LOAD")
            .arg(&module)
            .arg(mode)
            .arg(path.to_string_lossy().as_ref())
            .query::<()>(&mut con)?;
        assert!(
            read_drop_markers(&path)?.is_empty(),
            "{mode}: captures dropped while registered"
        );
        if mode == "deinit-refuse" || mode == "deinit-panic" {
            redis::cmd("MODULE")
                .arg(&["UNLOAD", "configuration_ownership"])
                .query::<()>(&mut con)
                .expect_err("refusing or panicking deinit allowed unload");
            assert!(
                read_drop_markers(&path)?.is_empty(),
                "{mode}: refused unload released captures"
            );
            redis::cmd("CONFIG")
                .arg(&["SET", "configuration_ownership.victim", "2"])
                .query::<()>(&mut con)?;
            let value: Vec<String> = redis::cmd("CONFIG")
                .arg(&["GET", "configuration_ownership.victim"])
                .query(&mut con)?;
            assert_eq!(value, ["configuration_ownership.victim", "2"]);
        }
        redis::cmd("MODULE")
            .arg(&["UNLOAD", "configuration_ownership"])
            .query::<()>(&mut con)?;
        assert_eq!(
            read_drop_markers(&path)?,
            b"AB",
            "{mode}: each capture must drop once, even if another drop panics"
        );
        let names: Vec<String> = redis::cmd("CONFIG")
            .arg(&["GET", "configuration_ownership.*"])
            .query(&mut con)?;
        assert!(
            names.is_empty(),
            "{mode}: native registrations survived unload"
        );
        assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
        std::fs::remove_file(&path)?;
    }
    Ok(())
}

#[test]
fn configuration_failed_initialization_releases_observer_captures_once() -> Result<()> {
    let mut con = TestConnection::new("configuration_ownership");
    let module = super::utils::module_path("configuration_ownership")?;
    redis::cmd("MODULE")
        .arg(&["UNLOAD", "configuration_ownership"])
        .query::<()>(&mut con)?;
    for mode in [
        "init-error",
        "init-panic",
        "registration-error",
        "init-panic-drop-panic",
    ] {
        let path = std::env::temp_dir().join(format!(
            "sdk-config-init-{}-{}-{mode}",
            std::process::id(),
            con.port()
        ));
        let _ = std::fs::remove_file(&path);
        redis::cmd("MODULE")
            .arg("LOAD")
            .arg(&module)
            .arg(mode)
            .arg(path.to_string_lossy().as_ref())
            .query::<()>(&mut con)
            .expect_err("failed SDK initialization allowed module load");
        assert_eq!(
            read_drop_markers(&path)?,
            b"AB",
            "{mode}: failed initialization leaked or repeated capture drops"
        );
        let names: Vec<String> = redis::cmd("CONFIG")
            .arg(&["GET", "configuration_ownership.*"])
            .query(&mut con)?;
        assert!(
            names.is_empty(),
            "{mode}: failed load left native registrations"
        );
        assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
        redis::cmd("MODULE")
            .arg("LOAD")
            .arg(&module)
            .query::<()>(&mut con)?;
        redis::cmd("CONFIG")
            .arg(&["SET", "configuration_ownership.victim", "3"])
            .query::<()>(&mut con)?;
        redis::cmd("MODULE")
            .arg(&["UNLOAD", "configuration_ownership"])
            .query::<()>(&mut con)?;
        assert_eq!(
            read_drop_markers(&path)?,
            b"AB",
            "{mode}: reload repeated old capture cleanup"
        );
        std::fs::remove_file(&path)?;
    }
    Ok(())
}

fn load_owned_fixture(con: &mut TestConnection, label: &str) -> Result<std::path::PathBuf> {
    let module = super::utils::module_path("configuration_ownership")?;
    redis::cmd("MODULE")
        .arg(&["UNLOAD", "configuration_ownership"])
        .query::<()>(con)?;
    let path = std::env::temp_dir().join(format!(
        "sdk-config-{label}-{}-{}",
        std::process::id(),
        con.port()
    ));
    let _ = std::fs::remove_file(&path);
    redis::cmd("MODULE")
        .arg("LOAD")
        .arg(&module)
        .arg("owned")
        .arg(path.to_string_lossy().as_ref())
        .query::<()>(con)?;
    Ok(path)
}

fn finish_worker(con: &mut TestConnection) -> Result<()> {
    super::utils::wait_until(
        std::time::Duration::from_secs(5),
        "configuration worker join",
        || Ok(redis::cmd("config_ownership.finish").query::<i64>(con)? == 1),
    )
}

#[test]
fn configuration_worker_native_completion_allows_unload_and_cleanup() -> Result<()> {
    let mut con = TestConnection::new("configuration_ownership");
    let path = load_owned_fixture(&mut con, "worker")?;
    let native_completion: bool =
        redis::cmd("config_ownership.post_job_available").query(&mut con)?;
    redis::cmd("config_ownership.worker").query::<()>(&mut con)?;
    finish_worker(&mut con)?;
    assert_eq!(
        redis::cmd("config_ownership.deinit_count").query::<i64>(&mut con)?,
        0
    );
    assert!(read_drop_markers(&path)?.is_empty());
    if !native_completion {
        redis::cmd("MODULE")
            .arg(&["UNLOAD", "configuration_ownership"])
            .query::<()>(&mut con)
            .expect_err("a worker without native completion proof allowed unload");
        assert_eq!(
            redis::cmd("config_ownership.deinit_count").query::<i64>(&mut con)?,
            0
        );
        assert!(read_drop_markers(&path)?.is_empty());
        assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
        return Ok(());
    }
    redis::cmd("MODULE")
        .arg(&["UNLOAD", "configuration_ownership"])
        .query::<()>(&mut con)?;
    assert_eq!(read_drop_markers(&path)?, b"AB");
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    std::fs::remove_file(&path)?;
    Ok(())
}

#[test]
fn configuration_worker_without_native_completion_keeps_unload_disabled() -> Result<()> {
    let mut con = TestConnection::new("configuration_ownership");
    let path = load_owned_fixture(&mut con, "worker-no-job")?;
    redis::cmd("config_ownership.worker")
        .arg("no-job")
        .query::<()>(&mut con)?;
    finish_worker(&mut con)?;
    // Returning to the main loop and finishing the worker are insufficient to
    // prove that every possible native worker CONFIG frame has finished.
    for _ in 0..3 {
        redis::cmd("MODULE")
            .arg(&["UNLOAD", "configuration_ownership"])
            .query::<()>(&mut con)
            .expect_err("worker fallback allowed unsafe unload");
        assert_eq!(
            redis::cmd("config_ownership.deinit_count").query::<i64>(&mut con)?,
            0
        );
        assert!(read_drop_markers(&path)?.is_empty());
        assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    }
    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn configuration_main_thread_timer_fallback_protects_direct_and_later_native_frames() -> Result<()>
{
    for (mode, expected) in [
        ("direct-no-job", [1, 0, 0, 1, 0, 1]),
        ("later-no-job", [1, 1, 1, 1, 0, 1]),
    ] {
        let mut con = TestConnection::new("configuration_ownership");
        let path = load_owned_fixture(&mut con, mode)?;
        let observed: Vec<i64> = redis::cmd("config_ownership.probe")
            .arg(mode)
            .query(&mut con)?;
        assert_eq!(
            observed, expected,
            "{mode}: native frame must remain protected until loop return"
        );
        super::utils::wait_until(
            std::time::Duration::from_secs(5),
            "configuration main-thread fallback completion",
            || {
                Ok(redis::cmd("MODULE")
                    .arg(&["UNLOAD", "configuration_ownership"])
                    .query::<()>(&mut con)
                    .is_ok())
            },
        )?;
        assert_eq!(
            read_drop_markers(&path)?,
            b"AB",
            "{mode}: loop completion must permit exact cleanup"
        );
        assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
        std::fs::remove_file(&path)?;
    }
    Ok(())
}

#[test]
fn configuration_worker_later_callback_refuses_unload_without_native_precheck_masking() -> Result<()>
{
    let mut con = TestConnection::new("configuration_ownership");
    let path = load_owned_fixture(&mut con, "worker-later")?;
    let native_completion: bool =
        redis::cmd("config_ownership.post_job_available").query(&mut con)?;
    redis::cmd("config_ownership.worker")
        .arg("later")
        .query::<()>(&mut con)?;
    finish_worker(&mut con)?;
    let snapshot: Vec<i64> = redis::cmd("config_ownership.worker_result").query(&mut con)?;
    assert_eq!(
        snapshot,
        [1, 1, 1, 1, 0, 1],
        "later native apply must remain protected after the SDK observer returns, without a victim timer or blocked client"
    );
    if native_completion {
        redis::cmd("MODULE")
            .arg(&["UNLOAD", "configuration_ownership"])
            .query::<()>(&mut con)?;
        assert_eq!(read_drop_markers(&path)?, b"AB");
        std::fs::remove_file(&path)?;
    } else {
        redis::cmd("MODULE")
            .arg(&["UNLOAD", "configuration_ownership"])
            .query::<()>(&mut con)
            .expect_err("worker fallback must remain fail-closed after native apply returns");
        assert_eq!(
            redis::cmd("config_ownership.deinit_count").query::<i64>(&mut con)?,
            0
        );
        assert!(read_drop_markers(&path)?.is_empty());
    }
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    Ok(())
}

#[test]
fn configuration_load_and_native_post_load_failures_release_captures_once() -> Result<()> {
    let mut con = TestConnection::new("configuration_ownership");
    let module = super::utils::module_path("configuration_ownership")?;
    redis::cmd("MODULE")
        .arg(&["UNLOAD", "configuration_ownership"])
        .query::<()>(&mut con)?;
    for mode in ["config-load-error", "skip-load"] {
        let path = std::env::temp_dir().join(format!(
            "sdk-config-load-{}-{}-{mode}",
            std::process::id(),
            con.port()
        ));
        let _ = std::fs::remove_file(&path);
        if mode == "config-load-error" {
            redis::cmd("MODULE")
                .arg("LOADEX")
                .arg(&module)
                .arg(&[
                    "CONFIG",
                    "configuration_ownership.victim",
                    "200",
                    "ARGS",
                    "owned",
                ])
                .arg(path.to_string_lossy().as_ref())
                .query::<()>(&mut con)
                .expect_err("out-of-range native load configuration was accepted");
        } else {
            redis::cmd("MODULE")
                .arg("LOAD")
                .arg(&module)
                .arg(mode)
                .arg(path.to_string_lossy().as_ref())
                .query::<()>(&mut con)
                .expect_err("uninitialized native configuration registrations were accepted");
        }
        assert_eq!(
            read_drop_markers(&path)?,
            b"AB",
            "{mode}: every accepted registration must be released once"
        );
        let names: Vec<String> = redis::cmd("CONFIG")
            .arg(&["GET", "configuration_ownership.*"])
            .query(&mut con)?;
        assert!(
            names.is_empty(),
            "{mode}: rejected load left native configurations"
        );
        assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
        redis::cmd("MODULE")
            .arg("LOAD")
            .arg(&module)
            .query::<()>(&mut con)?;
        redis::cmd("CONFIG")
            .arg(&["SET", "configuration_ownership.victim", "4"])
            .query::<()>(&mut con)?;
        redis::cmd("MODULE")
            .arg(&["UNLOAD", "configuration_ownership"])
            .query::<()>(&mut con)?;
        assert_eq!(
            read_drop_markers(&path)?,
            b"AB",
            "{mode}: reload repeated previous-generation cleanup"
        );
        std::fs::remove_file(&path)?;
    }
    Ok(())
}

#[test]
fn configuration_cleanup_reentry_denies_access_after_typed_owners_are_destroyed() -> Result<()> {
    let mut con = TestConnection::new("configuration_ownership");
    let module = super::utils::module_path("configuration_ownership")?;
    redis::cmd("MODULE")
        .arg(&["UNLOAD", "configuration_ownership"])
        .query::<()>(&mut con)?;
    let path = std::env::temp_dir().join(format!(
        "sdk-config-reentry-{}-{}",
        std::process::id(),
        con.port()
    ));
    let _ = std::fs::remove_file(&path);
    redis::cmd("MODULE")
        .arg("LOAD")
        .arg(&module)
        .arg("drop-reenter")
        .arg(path.to_string_lossy().as_ref())
        .query::<()>(&mut con)?;
    let text: Vec<String> = redis::cmd("CONFIG")
        .arg(&["GET", "configuration_ownership.text"])
        .query(&mut con)?;
    assert_eq!(text, ["configuration_ownership.text", "live"]);
    redis::cmd("CONFIG")
        .arg(&["SET", "configuration_ownership.victim", "9"])
        .query::<()>(&mut con)?;
    redis::cmd("MODULE")
        .arg(&["UNLOAD", "configuration_ownership"])
        .query::<()>(&mut con)?;
    assert_eq!(
        read_drop_markers(&path)?,
        b"ABR",
        "late destructor must safely reenter getters/setters after string and numeric metadata were freed"
    );
    assert_eq!(redis::cmd("PING").query::<String>(&mut con)?, "PONG");
    std::fs::remove_file(&path)?;
    Ok(())
}
