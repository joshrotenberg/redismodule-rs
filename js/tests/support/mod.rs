use std::fs::{self, OpenOptions};
use std::net::TcpListener;
use std::ops::{Deref, DerefMut};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use redis::Connection;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const POLL_INTERVAL: Duration = Duration::from_millis(10);
static INSTANCE_ID: AtomicUsize = AtomicUsize::new(0);
static MODULE_BUILD: OnceLock<Result<(), String>> = OnceLock::new();

fn build_module() -> Result<()> {
    let result = MODULE_BUILD.get_or_init(|| {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let workspace = manifest_dir
            .parent()
            .expect("js package has a workspace parent");
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let mut command = Command::new(cargo);
        command
            .current_dir(workspace)
            .args(["build", "--package", "redis-module-js"]);
        if cfg!(feature = "min-redis-compatibility-version-7-2") {
            command.args([
                "--no-default-features",
                "--features",
                "min-redis-compatibility-version-7-2",
            ]);
        }
        let output = command.output().map_err(|error| error.to_string())?;
        if output.status.success() {
            Ok(())
        } else {
            Err(format!(
                "module build failed with {}:\n{}\n{}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ))
        }
    });
    result
        .as_ref()
        .map_err(|message| anyhow::anyhow!(message.clone()))
        .copied()
}

pub fn module_path(name: &str) -> Result<String> {
    // Cargo does not link a loadable cdylib for `cargo test`; build the module
    // once in the foreground before any server tries to load it.
    build_module()?;
    let executable = std::env::current_exe()?;
    let deps = executable
        .parent()
        .context("integration test executable has a parent")?;
    let profile = deps.parent().context("deps has a profile parent")?;
    let extension = if cfg!(target_os = "macos") {
        "dylib"
    } else {
        "so"
    };
    Ok(profile
        .join(format!("lib{name}.{extension}"))
        .to_str()
        .context("module path is UTF-8")?
        .to_owned())
}

fn wait_for(description: &str, mut ready: impl FnMut() -> Result<bool>) -> Result<()> {
    let start = Instant::now();
    loop {
        if ready()? {
            return Ok(());
        }
        ensure!(
            start.elapsed() < STARTUP_TIMEOUT,
            "timed out waiting for {description}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

pub struct TestConnection {
    child: Child,
    connection: Connection,
    module: String,
    port: u16,
    log_file: PathBuf,
}

impl TestConnection {
    pub fn new(module: String) -> Result<Self> {
        Self::start(module, None)
    }

    fn start(module: String, replica_of: Option<u16>) -> Result<Self> {
        let id = INSTANCE_ID.fetch_add(1, Ordering::Relaxed);
        // Redis cannot inherit this listener. The small race after releasing it
        // is preferable to fixed ports colliding across parallel test processes.
        let port = TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port();
        let profile = std::env::current_exe()?
            .parent()
            .and_then(|path| path.parent())
            .context("integration test has a profile directory")?
            .to_path_buf();
        let directory = profile
            .join("redis-module-js-integration")
            .join(format!("{}-{id}-p{port}", std::process::id()));
        fs::create_dir_all(&directory)?;
        let log_file = directory.join("redis.log");
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_file)?;
        let binary =
            std::env::var_os("TEST_REDIS_SERVER_BIN").unwrap_or_else(|| "redis-server".into());
        let mut command = Command::new(binary);
        command
            .args(["--bind", "127.0.0.1", "--port"])
            .arg(port.to_string())
            .args([
                "--save",
                "",
                "--appendonly",
                "no",
                "--repl-diskless-sync-delay",
                "0",
                "--enable-debug-command",
                "yes",
                "--enable-module-command",
                "local",
                "--dir",
            ])
            .arg(&directory)
            .args(["--logfile"])
            .arg(&log_file);
        if let Some(primary) = replica_of {
            command
                .args(["--replicaof", "127.0.0.1"])
                .arg(primary.to_string());
        }
        let mut child = command
            .arg("--loadmodule")
            .arg(&module)
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .spawn()?;

        let client = redis::Client::open(format!("redis://127.0.0.1:{port}/"))?;
        let mut connection = None;
        wait_for("Redis startup", || {
            if let Some(status) = child.try_wait()? {
                bail!(
                    "Redis exited with {status}. Server log:\n{}",
                    fs::read_to_string(&log_file).unwrap_or_default()
                );
            }
            match client.get_connection() {
                Ok(mut candidate) => {
                    if redis::cmd("PING")
                        .query::<String>(&mut candidate)
                        .is_ok_and(|reply| reply == "PONG")
                    {
                        connection = Some(candidate);
                        Ok(true)
                    } else {
                        Ok(false)
                    }
                }
                Err(error) if error.is_io_error() => Ok(false),
                Err(error) => Err(error.into()),
            }
        })?;
        let mut server = Self {
            child,
            connection: connection.expect("successful startup probe"),
            module,
            port,
            log_file,
        };
        if replica_of.is_some() {
            wait_for("replica synchronization", || {
                let info: String = redis::cmd("INFO")
                    .arg("replication")
                    .query(&mut server.connection)?;
                Ok(info.lines().any(|line| line == "master_link_status:up")
                    && info.lines().any(|line| line == "master_sync_in_progress:0"))
            })?;
        }
        Ok(server)
    }

    pub fn connection(&self) -> Result<Connection> {
        Ok(redis::Client::open(format!("redis://127.0.0.1:{}/", self.port))?.get_connection()?)
    }

    pub fn replica(&self) -> Result<Self> {
        Self::start(self.module.clone(), Some(self.port))
    }

    pub fn server_log(&self) -> Result<String> {
        Ok(fs::read_to_string(&self.log_file)?)
    }
}

impl Deref for TestConnection {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        &self.connection
    }
}

impl DerefMut for TestConnection {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.connection
    }
}

impl Drop for TestConnection {
    fn drop(&mut self) {
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        if std::thread::panicking() {
            if let Ok(log) = fs::read_to_string(&self.log_file) {
                eprintln!("Redis server log ({}):\n{log}", self.log_file.display());
            }
        }
    }
}
