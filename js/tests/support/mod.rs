use std::fs::{self, OpenOptions};
use std::io::Write;
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::ops::{Deref, DerefMut};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use redis::{ConnectionLike, RedisResult, Value};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const PROBE_TIMEOUT: Duration = Duration::from_millis(200);
const POLL_INTERVAL: Duration = Duration::from_millis(10);
static INSTANCE_ID: AtomicUsize = AtomicUsize::new(0);

pub fn module_path(name: &str) -> Result<String> {
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
    let module = profile.join(format!("lib{name}.{extension}"));
    ensure!(
        module.is_file(),
        "module artifact is missing; build it with the matching profile and features: {}",
        module.display()
    );
    Ok(module.to_str().context("module path is UTF-8")?.to_owned())
}

pub struct Connection {
    socket: TcpStream,
    parser: redis::Parser,
    open: bool,
}

impl Connection {
    fn connect(port: u16) -> RedisResult<Self> {
        let address = SocketAddr::from(([127, 0, 0, 1], port));
        let socket = TcpStream::connect_timeout(&address, PROBE_TIMEOUT)?;
        socket.set_read_timeout(Some(PROBE_TIMEOUT))?;
        socket.set_write_timeout(Some(PROBE_TIMEOUT))?;
        Ok(Self {
            socket,
            parser: redis::Parser::new(),
            open: true,
        })
    }

    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        self.socket.set_read_timeout(timeout)
    }

    fn send(&mut self, command: &[u8]) -> RedisResult<()> {
        if let Err(error) = self.socket.write_all(command) {
            self.open = false;
            let _ = self.socket.shutdown(Shutdown::Both);
            return Err(error.into());
        }
        Ok(())
    }

    fn receive(&mut self) -> RedisResult<Value> {
        let reply = self.parser.parse_value(&mut self.socket);
        if reply.is_err() {
            self.open = false;
            let _ = self.socket.shutdown(Shutdown::Both);
        }
        reply.and_then(Value::extract_error)
    }
}

impl ConnectionLike for Connection {
    fn req_packed_command(&mut self, command: &[u8]) -> RedisResult<Value> {
        self.send(command)?;
        self.receive()
    }

    fn req_packed_commands(
        &mut self,
        command: &[u8],
        offset: usize,
        count: usize,
    ) -> RedisResult<Vec<Value>> {
        self.send(command)?;
        let mut replies = Vec::new();
        let mut first_error = None;
        for index in 0..(offset + count) {
            match self.receive() {
                Ok(reply) if index >= offset => replies.push(reply),
                Ok(_) => {}
                Err(error) if first_error.is_none() => first_error = Some(error),
                Err(_) => {}
            }
        }
        first_error.map_or(Ok(replies), Err)
    }

    fn get_db(&self) -> i64 {
        0
    }

    fn is_open(&self) -> bool {
        self.open
    }

    fn check_connection(&mut self) -> bool {
        redis::cmd("PING")
            .query::<String>(self)
            .is_ok_and(|reply| reply == "PONG")
    }
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
    child: ChildGuard,
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
        let child = command
            .arg("--loadmodule")
            .arg(&module)
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .spawn()?;
        // Own the process before any fallible setup so startup failures always
        // kill and reap it.
        let mut child = ChildGuard {
            child,
            log_file: log_file.clone(),
        };

        let mut connection = None;
        wait_for("Redis startup", || {
            if let Some(status) = child.child.try_wait()? {
                bail!(
                    "Redis exited with {status}. Server log:\n{}",
                    fs::read_to_string(&log_file).unwrap_or_default()
                );
            }
            connection = probe_connection(port, child.child.id())?;
            Ok(connection.is_some())
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
                if let Some(status) = server.child.child.try_wait()? {
                    bail!("Redis exited with {status} during replica synchronization");
                }
                let Some(mut probe) = probe_connection(port, server.child.child.id())? else {
                    return Ok(false);
                };
                let info: String = redis::cmd("INFO").arg("replication").query(&mut probe)?;
                Ok(info.lines().any(|line| line == "master_link_status:up")
                    && info.lines().any(|line| line == "master_sync_in_progress:0"))
            })?;
        }
        Ok(server)
    }

    pub fn connection(&self) -> Result<Connection> {
        get_connection(self.port, self.child.child.id())
    }

    pub fn replica(&self) -> Result<Self> {
        Self::start(self.module.clone(), Some(self.port))
    }

    pub fn server_log(&self) -> Result<String> {
        Ok(fs::read_to_string(&self.log_file)?)
    }
}

fn probe_connection(port: u16, expected_pid: u32) -> Result<Option<Connection>> {
    let mut connection = match Connection::connect(port) {
        Ok(connection) => connection,
        Err(error) if error.is_io_error() => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let probe = (|| -> RedisResult<bool> {
        let pong: String = redis::cmd("PING").query(&mut connection)?;
        let info: String = redis::cmd("INFO").arg("server").query(&mut connection)?;
        Ok(pong == "PONG"
            && info
                .lines()
                .any(|line| line == format!("process_id:{expected_pid}")))
    })();
    match probe {
        Ok(true) => {
            connection.socket.set_read_timeout(None)?;
            connection.socket.set_write_timeout(None)?;
            Ok(Some(connection))
        }
        Ok(false) => Ok(None),
        Err(error) if error.is_io_error() || error.code() == Some("LOADING") => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn get_connection(port: u16, expected_pid: u32) -> Result<Connection> {
    let mut connection = None;
    wait_for("authenticated Redis connection", || {
        connection = probe_connection(port, expected_pid)?;
        Ok(connection.is_some())
    })?;
    Ok(connection.expect("successful connection probe"))
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

struct ChildGuard {
    child: Child,
    log_file: PathBuf,
}

impl Drop for ChildGuard {
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
