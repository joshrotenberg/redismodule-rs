use anyhow::{bail, ensure, Context, Result};
use redis::{ConnectionLike, Parser, RedisResult, Value};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_millis(250);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const RETRY_INTERVAL: Duration = Duration::from_millis(25);
static RUN_ID: AtomicU64 = AtomicU64::new(0);

/// Start an isolated server and wait for readiness. Detailed failures include
/// the retained server log; the legacy static error type is kept for callers.
#[allow(dead_code)] // Retain the explicit-port utility alongside TestConnection.
pub fn start_redis(module_name: &str, port: u16) -> Result<Vec<ChildGuard>, &'static str> {
    start_redis_server_with_module(module_name, port)
        .map(|guard| vec![guard])
        .map_err(|error| {
            eprintln!("{error:#}");
            "failed to start redis server"
        })
}

pub struct TestConnection {
    // Close the client before terminating its server.
    connection: TestRedisConnection,
    _guard: ChildGuard,
}

impl TestConnection {
    pub fn new(module_name: &str) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .expect("Reserved an available local test port");
        let port = listener.local_addr().expect("Read local test port").port();
        drop(listener);
        let (guard, connection) = start_server(module_name, port)
            .unwrap_or_else(|error| panic!("Starting module {module_name}: {error:#}"));
        Self {
            connection,
            _guard: guard,
        }
    }
}

impl std::ops::Deref for TestConnection {
    type Target = TestRedisConnection;

    fn deref(&self) -> &Self::Target {
        &self.connection
    }
}

impl std::ops::DerefMut for TestConnection {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.connection
    }
}

/// Owns one process. Logs and data deliberately survive both success and failure.
#[derive(Debug)]
pub struct ChildGuard {
    child: Child,
    log_path: PathBuf,
}

impl ChildGuard {
    fn spawn(command: &mut Command, run_dir: &Path) -> Result<Self> {
        let log_path = run_dir.join("redis-server.log");
        let log = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&log_path)?;
        let stderr = log.try_clone()?;
        let child = command
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(stderr))
            .spawn()
            .with_context(|| format!("Starting server; log: {}", log_path.display()))?;
        eprintln!("Redis test artifacts: {}", run_dir.display());
        Ok(Self { child, log_path })
    }

    fn check_running(&mut self) -> Result<()> {
        if let Some(status) = self.child.try_wait()? {
            bail!("Server exited before readiness: {status}");
        }
        Ok(())
    }

    fn diagnostics(&self) -> String {
        let tail = log_tail(&self.log_path).unwrap_or_else(|error| error.to_string());
        format!("Server log: {}\n{tail}", self.log_path.display())
    }

    fn retain_info(&self, filename: &str, info: &str) -> Result<()> {
        let path = self
            .log_path
            .parent()
            .context("Server log has no run directory")?
            .join(filename);
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .and_then(|mut file| file.write_all(info.as_bytes()))
            .with_context(|| format!("Retaining server evidence: {}", path.display()))
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            if let Err(error) = self.child.kill() {
                eprintln!("Could not terminate test server: {error}");
            }
        }
        if let Err(error) = self.child.wait() {
            eprintln!("Could not reap test server: {error}");
        }
    }
}

fn log_tail(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    file.seek(SeekFrom::Start(length.saturating_sub(16 * 1024)))?;
    let mut bytes = Vec::new();
    file.take(16 * 1024).read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn module_directory(executable: &Path, override_dir: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(directory) = override_dir {
        return Ok(directory);
    }
    let deps = executable
        .parent()
        .context("Test executable has no directory")?;
    ensure!(
        deps.file_name().is_some_and(|name| name == "deps"),
        "Cannot infer example directory from {}; set REDIS_MODULE_TEST_MODULE_DIR",
        executable.display()
    );
    Ok(deps
        .parent()
        .context("Test executable has no profile directory")?
        .join("examples"))
}

fn unique_run_directory(root: &Path, module_name: &str) -> Result<PathBuf> {
    fs::create_dir_all(root)?;
    loop {
        let id = RUN_ID.fetch_add(1, Ordering::Relaxed);
        let directory = root.join(format!("{module_name}-{}-{id}", std::process::id()));
        match fs::create_dir(&directory) {
            Ok(()) => return Ok(directory),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error).context("Creating isolated test data directory"),
        }
    }
}

#[allow(dead_code)] // Retain the explicit-port utility alongside TestConnection.
pub fn start_redis_server_with_module(module_name: &str, port: u16) -> Result<ChildGuard> {
    start_server(module_name, port).map(|(guard, _connection)| guard)
}

fn start_server(module_name: &str, port: u16) -> Result<(ChildGuard, TestRedisConnection)> {
    ensure!(
        !module_name.is_empty()
            && module_name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
        "Expected an example module name, got {module_name:?}"
    );
    ensure!(port != 0, "Explicit Redis test port must be nonzero");
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    // This catches common port conflicts. The subsequent process_id check also
    // handles a different process winning the race after this reservation drops.
    let reservation = TcpListener::bind(address)
        .with_context(|| format!("Test port {port} is already occupied"))?;
    let directory = module_directory(
        &std::env::current_exe()?,
        std::env::var_os("REDIS_MODULE_TEST_MODULE_DIR").map(PathBuf::from),
    )?;
    let extension = if cfg!(target_os = "macos") {
        "dylib"
    } else {
        "so"
    };
    let module = directory.join(format!("lib{module_name}.{extension}"));
    ensure!(
        module.is_file(),
        "Module artifact is missing: {}",
        module.display()
    );
    let module = fs::canonicalize(module)?;
    let log_root = std::env::var_os("REDIS_MODULE_TEST_LOG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            directory
                .parent()
                .unwrap_or(&directory)
                .join("integration-test")
        });
    let run_dir = fs::canonicalize(unique_run_directory(&log_root, module_name)?)?;
    let binary =
        std::env::var_os("REDIS_MODULE_TEST_REDIS_SERVER").unwrap_or_else(|| "redis-server".into());
    let mut command = Command::new(binary);
    command
        .arg("--bind")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(port.to_string())
        .arg("--daemonize")
        .arg("no")
        .arg("--loadmodule")
        .arg(module)
        .arg("--enable-debug-command")
        .arg("yes")
        .arg("--dir")
        .arg(&run_dir)
        .arg("--dbfilename")
        .arg("dump.rdb");
    drop(reservation);
    let mut guard = ChildGuard::spawn(&mut command, &run_dir)?;
    let expected_version = std::env::var("REDIS_MODULE_TEST_EXPECT_REDIS_VERSION").ok();
    let connection = wait_for_owned_server(
        &mut guard,
        address,
        STARTUP_TIMEOUT,
        expected_version.as_deref(),
    )
    .with_context(|| guard.diagnostics())?;
    Ok((guard, connection))
}

fn wait_for_owned_server(
    guard: &mut ChildGuard,
    address: SocketAddr,
    timeout: Duration,
    expected_version: Option<&str>,
) -> Result<TestRedisConnection> {
    let deadline = Instant::now() + timeout;
    loop {
        guard.check_running()?;
        let remaining = remaining_time(deadline).context("Server startup deadline elapsed")?;
        match TestRedisConnection::connect(address, CONNECT_TIMEOUT.min(remaining)) {
            Ok(mut connection) => {
                connection.timeout = COMMAND_TIMEOUT.min(remaining_time(deadline)?);
                let info: String = redis::cmd("INFO").arg("server").query(&mut connection)?;
                verify_server_identity(&info, guard.child.id(), expected_version)?;
                guard.check_running()?;
                guard.retain_info("server-info.txt", &info)?;
                // Only query further evidence once PID/version checks establish
                // that this is the server owned by this test.
                connection.timeout = COMMAND_TIMEOUT.min(remaining_time(deadline)?);
                let memory: String = redis::cmd("INFO").arg("memory").query(&mut connection)?;
                guard.retain_info("memory-info.txt", &memory)?;
                guard.check_running()?;
                ensure!(Instant::now() < deadline, "Server startup deadline elapsed");
                connection.timeout = COMMAND_TIMEOUT;
                return Ok(connection);
            }
            Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {
                std::thread::sleep(RETRY_INTERVAL.min(remaining));
            }
            Err(error) => return Err(error).context("Connecting to test server"),
        }
    }
}

fn verify_server_identity(
    info: &str,
    process_id: u32,
    expected_version: Option<&str>,
) -> Result<()> {
    let field = |name: &str| {
        info.lines().find_map(|line| {
            line.split_once(':')
                .filter(|(key, _)| *key == name)
                .map(|(_, value)| value)
        })
    };
    ensure!(
        field("process_id").and_then(|value| value.parse::<u32>().ok()) == Some(process_id),
        "Redis on the test port is not the owned server (expected PID {process_id})"
    );
    if let Some(expected) = expected_version {
        ensure!(
            field("redis_version") == Some(expected),
            "Expected Redis {expected}, got {:?}",
            field("redis_version")
        );
    }
    Ok(())
}

/// Connect to a caller-owned local server with a finite retry deadline. This
/// helper alone does not establish ownership; TestConnection verifies the PID.
#[allow(dead_code)] // Retain the standalone connection utility for explicit-port callers.
pub fn get_redis_connection(port: u16) -> Result<TestRedisConnection> {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    loop {
        let remaining = remaining_time(deadline).context("Connection deadline elapsed")?;
        match TestRedisConnection::connect(address, CONNECT_TIMEOUT.min(remaining)) {
            Ok(connection) => return Ok(connection),
            Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {
                std::thread::sleep(RETRY_INTERVAL.min(remaining));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn remaining_time(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "Test I/O deadline elapsed"))
}

/// A test-only RESP2 connection. The public redis-rs parser and command API are
/// reused, but connection setup does no I/O: redis-rs 0.23 otherwise sends an
/// unbounded CLIENT SETINFO pipeline before callers can set socket timeouts.
/// No authentication, database switching, PubSub, or reconnect behavior is added.
pub struct TestRedisConnection {
    stream: TcpStream,
    parser: Parser,
    timeout: Duration,
    open: bool,
}

impl TestRedisConnection {
    fn connect(address: SocketAddr, timeout: Duration) -> io::Result<Self> {
        let stream = TcpStream::connect_timeout(&address, timeout)?;
        Ok(Self {
            stream,
            parser: Parser::new(),
            timeout: COMMAND_TIMEOUT,
            open: true,
        })
    }

    fn exchange(&mut self, command: &[u8], offset: usize, count: usize) -> RedisResult<Vec<Value>> {
        let result = self.exchange_inner(command, offset, count);
        if result.as_ref().is_err_and(|error| error.is_io_error()) {
            self.open = false;
            let _ = self.stream.shutdown(Shutdown::Both);
        }
        result
    }

    fn exchange_inner(
        &mut self,
        command: &[u8],
        offset: usize,
        count: usize,
    ) -> RedisResult<Vec<Value>> {
        let total = offset.checked_add(count).ok_or_else(|| {
            redis::RedisError::from((redis::ErrorKind::ClientError, "Response count overflow"))
        })?;
        let mut stream = DeadlineStream {
            stream: &mut self.stream,
            deadline: Instant::now() + self.timeout,
        };
        stream.write_all(command)?;
        let mut responses = Vec::with_capacity(count);
        let mut first_error = None;
        for index in 0..total {
            match self.parser.parse_value(&mut stream) {
                Ok(value) if index >= offset => responses.push(value),
                Ok(_) => {}
                Err(error) if error.is_io_error() => return Err(error),
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
        first_error.map_or(Ok(responses), Err)
    }
}

impl ConnectionLike for TestRedisConnection {
    fn req_packed_command(&mut self, command: &[u8]) -> RedisResult<Value> {
        self.exchange(command, 0, 1)
            .map(|mut values| values.remove(0))
    }

    fn req_packed_commands(
        &mut self,
        command: &[u8],
        offset: usize,
        count: usize,
    ) -> RedisResult<Vec<Value>> {
        self.exchange(command, offset, count)
    }

    fn get_db(&self) -> i64 {
        0
    }
    fn is_open(&self) -> bool {
        self.open
    }
    fn check_connection(&mut self) -> bool {
        redis::cmd("PING").query::<String>(self).is_ok()
    }
}

struct DeadlineStream<'a> {
    stream: &'a mut TcpStream,
    deadline: Instant,
}

impl Read for DeadlineStream<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.stream
            .set_read_timeout(Some(remaining_time(self.deadline)?))?;
        self.stream.read(buffer)
    }
}

impl Write for DeadlineStream<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.stream
            .set_write_timeout(Some(remaining_time(self.deadline)?))?;
        self.stream.write(buffer)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

#[cfg(test)]
mod harness_tests {
    use super::*;

    #[test]
    fn artifacts_follow_executable_profile_and_explicit_override() {
        for executable in [
            "/repo/target/debug/deps/integration-abc",
            "/tmp/build/release/deps/integration-abc",
            "/tmp/build/aarch64-unknown-linux-gnu/debug/deps/integration-abc",
        ] {
            let executable = Path::new(executable);
            assert_eq!(
                module_directory(executable, None).unwrap(),
                executable
                    .parent()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .join("examples")
            );
        }
        let override_dir = PathBuf::from("/explicit/modules");
        assert_eq!(
            module_directory(Path::new("/unusual/test"), Some(override_dir.clone())).unwrap(),
            override_dir
        );
        assert!(module_directory(Path::new("/unusual/test"), None).is_err());
    }

    #[test]
    fn data_directories_are_unique_and_preserve_existing_files() {
        let root =
            std::env::temp_dir().join(format!("redis-harness-directories-{}", std::process::id()));
        let first = unique_run_directory(&root, "example").unwrap();
        let sentinel = first.join("keep.rdb");
        fs::write(&sentinel, b"existing data").unwrap();
        let second = unique_run_directory(&root, "example").unwrap();
        assert_ne!(first, second);
        assert_eq!(fs::read(sentinel).unwrap(), b"existing data");
        fs::remove_dir_all(first).unwrap();
        fs::remove_dir_all(second).unwrap();
    }

    #[test]
    fn readiness_requires_owned_pid_and_expected_server_version() {
        let info = "# Server\r\nredis_version:7.2.8\r\nprocess_id:123\r\n";
        assert!(verify_server_identity(info, 123, Some("7.2.8")).is_ok());
        assert!(verify_server_identity(info, 124, None).is_err());
        assert!(verify_server_identity(info, 123, Some("8.10.2")).is_err());
        assert!(verify_server_identity("redis_version:7.2.8\r\n", 123, None).is_err());
    }

    #[test]
    fn command_errors_and_pipeline_errors_leave_response_alignment_intact() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let peer = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = [0; 3];
            socket.read_exact(&mut request).unwrap();
            socket.write_all(b"-ERR ordinary test error\r\n").unwrap();
            socket.read_exact(&mut request).unwrap();
            socket
                .write_all(b"+ignored\r\n-ERR ordinary pipeline error\r\n+last\r\n")
                .unwrap();
            socket.read_exact(&mut request).unwrap();
            socket.write_all(b"+aligned\r\n").unwrap();
        });
        let mut connection = TestRedisConnection::connect(address, Duration::from_secs(1)).unwrap();
        assert!(connection.req_packed_command(b"one").is_err());
        assert!(connection.req_packed_commands(b"two", 1, 2).is_err());
        assert_eq!(
            connection.req_packed_command(b"end").unwrap(),
            Value::Status("aligned".into())
        );
        peer.join().unwrap();
    }

    #[test]
    fn silent_peer_has_bounded_setup_and_read() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let started = Instant::now();
        let mut connection = TestRedisConnection::connect(address, Duration::from_secs(1)).unwrap();
        let (_silent_peer, _) = listener.accept().unwrap();
        connection.timeout = Duration::from_millis(50);
        let error = connection.req_packed_command(b"PING\r\n").unwrap_err();
        assert!(error.is_io_error());
        assert!(!connection.is_open());
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn peer_that_does_not_read_has_bounded_writes() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let mut connection = TestRedisConnection::connect(address, Duration::from_secs(1)).unwrap();
        let (_silent_peer, _) = listener.accept().unwrap();
        connection.timeout = Duration::from_millis(50);
        // Exceed normal loopback socket buffers without involving a Redis server.
        let request = vec![b'x'; 16 * 1024 * 1024];
        let started = Instant::now();
        let error = connection.req_packed_command(&request).unwrap_err();
        assert!(error.is_io_error());
        assert!(!connection.is_open());
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn expired_io_deadline_fails_before_read_or_write() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (_peer, _) = listener.accept().unwrap();
        let mut stream = DeadlineStream {
            stream: &mut client,
            deadline: Instant::now() - Duration::from_millis(1),
        };
        assert_eq!(
            stream.read(&mut [0]).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert_eq!(
            stream.write(b"hello").unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
    }

    #[cfg(unix)]
    #[test]
    fn early_exit_reports_status_and_retains_server_log() {
        let directory = unique_run_directory(
            &std::env::temp_dir().join("redis-harness-processes"),
            "exit",
        )
        .unwrap();
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "echo ordinary-startup-failure; exit 7"]);
        let mut guard = ChildGuard::spawn(&mut command, &directory).unwrap();
        let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = port.local_addr().unwrap();
        drop(port);
        let error = wait_for_owned_server(&mut guard, address, Duration::from_secs(2), None)
            .err()
            .expect("Fake server should exit");
        assert!(
            error.to_string().contains("Server exited before readiness"),
            "{error:#}"
        );
        drop(guard);
        assert!(fs::read_to_string(directory.join("redis-server.log"))
            .unwrap()
            .contains("ordinary-startup-failure"));
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn startup_deadline_and_guard_drop_terminate_owned_process() {
        let directory = unique_run_directory(
            &std::env::temp_dir().join("redis-harness-processes"),
            "timeout",
        )
        .unwrap();
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "exec sleep 30"]);
        let mut guard = ChildGuard::spawn(&mut command, &directory).unwrap();
        let pid = guard.child.id();
        let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = port.local_addr().unwrap();
        drop(port);
        let started = Instant::now();
        let error = wait_for_owned_server(&mut guard, address, Duration::from_millis(50), None)
            .err()
            .expect("Fake server should miss startup deadline");
        assert!(error.to_string().contains("deadline elapsed"), "{error:#}");
        drop(guard);
        assert!(started.elapsed() < Duration::from_secs(2));
        // `kill -0` checks process existence and sends no signal.
        assert!(!Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success());
        assert!(directory.join("redis-server.log").is_file());
        fs::remove_dir_all(directory).unwrap();
    }
}
