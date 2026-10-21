use anyhow::{Context, Result, bail, ensure};
use redis::{ConnectionLike, RedisResult, Value};
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const PROBE_TIMEOUT: Duration = Duration::from_millis(200);
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const PORT_ATTEMPTS: usize = 5;
static INSTANCE_ID: AtomicUsize = AtomicUsize::new(0);

/// A test-local RESP connection with timeouts installed before any commands.
/// redis 0.23's connection constructor performs an unbounded CLIENT SETINFO
/// handshake before exposing its socket timeout setters.
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

    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        self.socket.set_write_timeout(timeout)
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
        if reply.as_ref().is_err_and(|error| error.is_io_error()) {
            // Partial or late replies cannot be reused after a timeout.
            self.open = false;
            let _ = self.socket.shutdown(Shutdown::Both);
        }
        reply
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
        // Drain all replies even if one command fails; otherwise the next
        // query could consume a response left behind by this pipeline.
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
            .is_ok_and(|pong| pong == "PONG")
    }
}

/// Polls until the condition succeeds, returning predicate errors immediately.
/// Conditions must bound their own blocking operations; the deadline is checked
/// between calls. A timeout reports what the test was waiting for.
pub fn wait_until(
    timeout: Duration,
    description: &str,
    mut condition: impl FnMut() -> Result<bool>,
) -> Result<()> {
    let start = Instant::now();
    loop {
        if condition()? {
            return Ok(());
        }
        let remaining = timeout.saturating_sub(start.elapsed());
        ensure!(
            !remaining.is_zero(),
            "Timed out after {timeout:?} waiting for {description}"
        );
        std::thread::sleep(POLL_INTERVAL.min(remaining));
    }
}

#[derive(Clone)]
struct LaunchSpec {
    binary: OsString,
    module: PathBuf,
    module_args: Vec<String>,
    port: u16,
    directory: PathBuf,
    log_file: PathBuf,
    replica_of: Option<u16>,
}

impl LaunchSpec {
    fn fresh(module: PathBuf, module_args: Vec<String>) -> Result<Self> {
        // Redis cannot inherit this listener. Readiness also verifies its PID,
        // so a process winning the bind race cannot be mistaken for our child.
        let port = TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port();
        let base = profile_dir()?.join("integration-test");
        fs::create_dir_all(&base)?;
        let directory = loop {
            let id = INSTANCE_ID.fetch_add(1, Ordering::Relaxed);
            let dir = base.join(format!("instance-{}-{id}-p{port}", std::process::id()));
            match fs::create_dir(&dir) {
                Ok(()) => break dir,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        };
        Ok(Self {
            binary: std::env::var_os("TEST_REDIS_SERVER_BIN")
                .unwrap_or_else(|| "redis-server".into()),
            module,
            module_args,
            port,
            log_file: directory.join("redis.log"),
            directory,
            replica_of: None,
        })
    }

    fn spawn(&self) -> Result<ChildGuard> {
        ensure!(
            self.module.is_file(),
            "Module not built: {}",
            self.module.display()
        );
        // Append across restarts and capture errors emitted before Redis opens
        // its own logfile. Files avoid the backpressure of undrained pipes.
        let output = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_file)?;
        let mut command = Command::new(&self.binary);
        command
            .args(["--bind", "127.0.0.1", "--port"])
            .arg(self.port.to_string())
            .args([
                "--save",
                "",
                "--appendonly",
                "no",
                "--enable-debug-command",
                "yes",
                "--enable-module-command",
                "local",
                "--repl-diskless-sync-delay",
                "0",
                "--dir",
            ])
            .arg(&self.directory)
            .args(["--dbfilename", "dump.rdb", "--logfile"])
            .arg(&self.log_file);
        if let Some(primary) = self.replica_of {
            command
                .args(["--replicaof", "127.0.0.1"])
                .arg(primary.to_string());
        }
        let child = command
            .arg("--loadmodule")
            .arg(&self.module)
            .args(&self.module_args)
            .stdout(Stdio::from(output.try_clone()?))
            .stderr(Stdio::from(output))
            .spawn()
            .with_context(|| format!("Starting Redis binary {:?}", self.binary))?;
        Ok(ChildGuard {
            child,
            log_file: self.log_file.clone(),
        })
    }

    fn diagnostics(&self, action: &str) -> String {
        let log = fs::read_to_string(&self.log_file)
            .unwrap_or_else(|error| format!("Could not read {}: {error}", self.log_file.display()));
        format!(
            "{action} for module {} on port {}. Server log ({}):\n{log}",
            self.module.display(),
            self.port,
            self.log_file.display()
        )
    }
}

// A competing socket can win between port selection and either bind() or
// listen(). Require the TCP socket diagnostic and fatal line for this port.
fn tcp_address_in_use(log: &str, port: u16) -> bool {
    let socket = format!("Could not create server TCP listening socket 127.0.0.1:{port}: ");
    log.lines().any(|line| {
        line.contains(&socket)
            && (line.ends_with("bind: Address already in use")
                || line.ends_with("listen: Address already in use"))
    }) && log.contains(&format!("Failed listening on port {port} (tcp)"))
}

/// Owns a Redis server and a connection. Every server captures its startup log.
pub struct TestConnection {
    server: ChildGuard,
    connection: Connection,
    spec: LaunchSpec,
}

impl TestConnection {
    pub fn new(module_name: &str) -> Self {
        Self::with_args(module_name, &[])
    }

    /// Starts a module with arguments, panicking with diagnostics on failure.
    pub fn with_args(module_name: &str, args: &[&str]) -> Self {
        Self::try_with_args(module_name, args)
            .unwrap_or_else(|error| panic!("Redis instance failed to start: {error:#}"))
    }

    /// Returns startup failures with the server's exit status or timeout and log.
    pub fn try_with_args(module_name: &str, args: &[&str]) -> Result<Self> {
        let module: PathBuf = module_path(module_name)?.into();
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        Self::start_fresh(|| LaunchSpec::fresh(module.clone(), args.clone()))
    }

    /// Kept for existing tests; all constructors now capture server logs.
    pub fn with_log_file(module_name: &str) -> Self {
        Self::new(module_name)
    }

    fn start_fresh(mut create_spec: impl FnMut() -> Result<LaunchSpec>) -> Result<Self> {
        let mut collisions = Vec::new();
        for _ in 0..PORT_ATTEMPTS {
            let spec = create_spec()?;
            match Self::launch(spec.clone()) {
                Ok(connection) => return Ok(connection),
                Err(error) => {
                    let log = fs::read_to_string(&spec.log_file).unwrap_or_default();
                    // Port-zero selection cannot reserve a socket for Redis.
                    // Retry only a confirmed TCP bind collision, after launch
                    // has dropped and reaped the child. Other errors propagate.
                    if !tcp_address_in_use(&log, spec.port) {
                        return Err(error);
                    }
                    collisions.push(format!("{error:#}"));
                }
            }
        }
        bail!(
            "Redis could not bind a port after {PORT_ATTEMPTS} attempts:\n{}",
            collisions.join("\n")
        )
    }

    fn launch(spec: LaunchSpec) -> Result<Self> {
        // Install ownership before probing so every failure kills and reaps.
        let mut server = spec
            .spawn()
            .with_context(|| spec.diagnostics("Redis spawn failed"))?;
        let mut connection = None;
        wait_until(STARTUP_TIMEOUT, "Redis startup", || {
            if let Some(status) = server.child.try_wait()? {
                bail!("Redis exited with {status} before becoming ready");
            }
            connection = probe_connection(spec.port, Some(server.child.id()))?;
            Ok(connection.is_some())
        })
        .with_context(|| spec.diagnostics("Redis startup failed"))?;
        Ok(Self {
            server,
            connection: connection.expect("successful readiness probe"),
            spec,
        })
    }

    /// Opens another connection to the same server.
    pub fn connection(&self) -> Result<Connection> {
        get_redis_connection(self.spec.port, self.server.child.id())
            .with_context(|| self.spec.diagnostics("Redis connection failed"))
    }

    pub fn port(&self) -> u16 {
        self.spec.port
    }

    pub fn server_log(&self) -> std::io::Result<String> {
        fs::read_to_string(&self.spec.log_file)
    }

    /// Starts the same module and arguments in a separate replica and waits for
    /// initial synchronization. Later writes can be acknowledged with WAIT.
    pub fn replica(&self) -> Result<Self> {
        let mut replica = Self::start_fresh(|| {
            let mut spec =
                LaunchSpec::fresh(self.spec.module.clone(), self.spec.module_args.clone())?;
            spec.binary = self.spec.binary.clone();
            spec.replica_of = Some(self.spec.port);
            Ok(spec)
        })?;
        let sync = wait_until(STARTUP_TIMEOUT, "replica initial synchronization", || {
            if let Some(status) = replica.server.child.try_wait()? {
                bail!("Replica exited with {status} during synchronization");
            }
            let Some(mut probe) =
                probe_connection(replica.port(), Some(replica.server.child.id()))?
            else {
                return Ok(false);
            };
            probe.set_read_timeout(Some(PROBE_TIMEOUT))?;
            probe.set_write_timeout(Some(PROBE_TIMEOUT))?;
            // Use a fresh connection on each attempt: a timed-out INFO reply
            // must not be mistaken for the reply to the next polling request.
            match redis::cmd("INFO")
                .arg("replication")
                .query::<String>(&mut probe)
            {
                Ok(info) => Ok(info.lines().any(|line| line == "master_link_status:up")
                    && info.lines().any(|line| line == "master_sync_in_progress:0")),
                Err(error) if error.is_io_error() || error.code() == Some("LOADING") => Ok(false),
                Err(error) => Err(error.into()),
            }
        });
        sync.with_context(|| replica.spec.diagnostics("Replica synchronization failed"))?;
        replica.connection = replica.connection()?;
        Ok(replica)
    }

    /// Saves and shuts down Redis, then relaunches on the same port with the
    /// same data directory, module arguments, role, binary, and appended log.
    /// Consuming self prevents callers from using a stale connection on error.
    pub fn restart(mut self) -> Result<Self> {
        self.connection.set_read_timeout(Some(STARTUP_TIMEOUT))?;
        let shutdown = redis::cmd("SHUTDOWN")
            .arg("SAVE")
            .query::<()>(&mut self.connection);
        if let Err(error) = shutdown {
            if !error.is_io_error() {
                return Err(error).with_context(|| self.spec.diagnostics("Redis shutdown failed"));
            }
            // Redis closes the connection on success. Confirm actual exit
            // below rather than mistaking a socket timeout for shutdown.
        }
        wait_until(STARTUP_TIMEOUT, "Redis shutdown", || {
            match self.server.child.try_wait()? {
                Some(status) => {
                    ensure!(status.success(), "Redis shutdown exited with {status}");
                    Ok(true)
                }
                None => Ok(false),
            }
        })
        .with_context(|| self.spec.diagnostics("Redis shutdown failed"))?;
        let spec = self.spec.clone();
        drop(self);
        Self::launch(spec)
    }
}

impl std::ops::Deref for TestConnection {
    type Target = Connection;
    fn deref(&self) -> &Self::Target {
        &self.connection
    }
}

impl std::ops::DerefMut for TestConnection {
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
        let status = self.child.try_wait();
        let failed = matches!(&status, Ok(Some(status)) if !status.success());
        if !matches!(status, Ok(Some(_))) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        if failed || std::thread::panicking() {
            match fs::read_to_string(&self.log_file) {
                Ok(log) => eprintln!("Redis server log ({}):\n{log}", self.log_file.display()),
                Err(error) => eprintln!("Could not read {}: {error}", self.log_file.display()),
            }
        }
    }
}

/// The profile directory beside the current integration test's deps directory.
fn profile_dir() -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let parent = exe.parent().context("Test executable has a parent")?;
    Ok(if parent.file_name().is_some_and(|name| name == "deps") {
        parent.parent().context("deps has a profile parent")?
    } else {
        parent
    }
    .to_path_buf())
}

/// Resolves built modules beside the running tests, including custom targets.
pub fn module_path(module_name: &str) -> Result<String> {
    let extension = if cfg!(target_os = "macos") {
        "dylib"
    } else {
        "so"
    };
    Ok(profile_dir()?
        .join("examples")
        .join(format!("lib{module_name}.{extension}"))
        .to_str()
        .context("Module path is UTF-8")?
        .to_owned())
}

fn probe_connection(port: u16, expected_pid: Option<u32>) -> Result<Option<Connection>> {
    let mut con = match Connection::connect(port) {
        Ok(con) => con,
        Err(error) if error.is_io_error() => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    con.set_read_timeout(Some(PROBE_TIMEOUT))?;
    con.set_write_timeout(Some(PROBE_TIMEOUT))?;
    let probe = (|| -> redis::RedisResult<bool> {
        let pong: String = redis::cmd("PING").query(&mut con)?;
        if let Some(pid) = expected_pid {
            let info: String = redis::cmd("INFO").arg("server").query(&mut con)?;
            let expected = format!("process_id:{pid}");
            return Ok(pong == "PONG" && info.lines().any(|line| line == expected));
        }
        Ok(pong == "PONG")
    })();
    match probe {
        Ok(true) => {
            con.set_read_timeout(None)?;
            con.set_write_timeout(None)?;
            Ok(Some(con))
        }
        Ok(false) => Ok(None),
        Err(error) if error.is_io_error() || error.code() == Some("LOADING") => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Connects with bounded attempts instead of retrying connection refusal forever.
fn get_redis_connection(port: u16, expected_pid: u32) -> Result<Connection> {
    let mut connection = None;
    wait_until(
        STARTUP_TIMEOUT,
        &format!("Redis connection on port {port}"),
        || {
            connection = probe_connection(port, Some(expected_pid))?;
            Ok(connection.is_some())
        },
    )?;
    Ok(connection.expect("successful readiness probe"))
}

#[cfg(test)]
mod connection_tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn tcp_collisions_include_bind_and_listen_failures() {
        for operation in ["bind", "listen"] {
            let log = format!(
                "Warning: Could not create server TCP listening socket 127.0.0.1:1234: \
                 {operation}: Address already in use\n\
                 Failed listening on port 1234 (tcp), aborting.\n"
            );
            assert!(tcp_address_in_use(&log, 1234));
            assert!(!tcp_address_in_use(&log, 1235));
            assert!(!tcp_address_in_use(
                &log.replace("Address already in use", "Permission denied"),
                1234
            ));
            assert!(!tcp_address_in_use(log.lines().next().unwrap(), 1234));
        }
        assert!(!tcp_address_in_use(
            "Module load failed: bind: Address already in use\n\
             Failed listening on port 1234 (tcp)",
            1234
        ));
    }

    #[test]
    fn test_harness_retries_a_confirmed_port_collision() -> Result<()> {
        let occupied = TcpListener::bind(("127.0.0.1", 0))?;
        let occupied_port = occupied.local_addr()?.port();
        let mut attempts = 0;
        let mut failed_log = PathBuf::new();
        let mut con = TestConnection::start_fresh(|| {
            attempts += 1;
            let mut spec = LaunchSpec::fresh(module_path("hello")?.into(), Vec::new())?;
            if attempts == 1 {
                spec.port = occupied_port;
                failed_log = spec.log_file.clone();
            }
            Ok(spec)
        })?;
        assert!((2..=PORT_ATTEMPTS).contains(&attempts));
        assert_ne!(con.port(), occupied_port);
        assert!(
            tcp_address_in_use(&fs::read_to_string(failed_log)?, occupied_port),
            "the failed attempt's diagnostics were lost"
        );
        assert_eq!(
            redis::cmd("hello.mul")
                .arg(&[2, 3])
                .query::<Vec<i64>>(&mut con)?,
            [2, 3, 6]
        );
        Ok(())
    }

    #[test]
    fn test_harness_does_not_retry_module_initialization_errors() -> Result<()> {
        let mut attempts = 0;
        let result = TestConnection::start_fresh(|| {
            attempts += 1;
            LaunchSpec::fresh(
                module_path("configuration")?.into(),
                vec!["bool".into(), "invalid".into()],
            )
        });
        assert_eq!(attempts, 1);
        let error = match result {
            Ok(_) => bail!("an invalid module configuration loaded"),
            Err(error) => error,
        };
        assert!(format!("{error:#}").contains("Boolean configuration 'bool'"));
        Ok(())
    }

    #[test]
    fn test_harness_rejects_a_different_process_on_the_port() -> Result<()> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        let peer = std::thread::spawn(move || -> Result<()> {
            let (mut socket, _) = listener.accept()?;
            socket.set_read_timeout(Some(Duration::from_secs(3)))?;
            socket.set_write_timeout(Some(Duration::from_secs(3)))?;
            let mut parser = redis::Parser::new();
            parser.parse_value(&mut socket)?;
            socket.write_all(b"+PONG\r\n")?;
            parser.parse_value(&mut socket)?;
            let info = b"# Server\r\nprocess_id:0\r\n";
            write!(socket, "${}\r\n", info.len())?;
            socket.write_all(info)?;
            socket.write_all(b"\r\n")?;
            Ok(())
        });
        let result = probe_connection(port, Some(u32::MAX));
        peer.join().expect("fake listener thread panicked")?;
        assert!(result?.is_none(), "a different process passed readiness");
        Ok(())
    }

    #[test]
    fn test_harness_unresponsive_listener_is_bounded() -> Result<()> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        let peer = std::thread::spawn(move || -> std::io::Result<()> {
            let (mut socket, _) = listener.accept()?;
            // Close eventually even if the client's timeout regresses, so the
            // regression fails an elapsed-time assertion instead of hanging CI.
            socket.set_read_timeout(Some(Duration::from_secs(3)))?;
            let mut bytes = [0; 1024];
            loop {
                match socket.read(&mut bytes) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
            Ok(())
        });
        let start = Instant::now();
        let result = probe_connection(port, None);
        let elapsed = start.elapsed();
        peer.join().expect("fake listener thread panicked")?;
        assert!(
            result?.is_none(),
            "an unresponsive listener passed readiness"
        );
        assert!(elapsed < Duration::from_secs(2), "probe waited {elapsed:?}");
        Ok(())
    }
}
