use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use ureq::unversioned::resolver::{ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{DefaultConnector, NextTimeout};

const REQUEST_LIMIT: usize = 2048;
const RESPONSE_LIMIT: u64 = 1024;
const HEADER_LIMIT: usize = 4096;
/// Maximum time allowed for one HTTP transaction, including its response body.
pub const HTTP_DEADLINE: Duration = Duration::from_millis(500);

/// A configured, numeric loopback listener. No hostnames or custom paths.
#[derive(Clone, Copy, Debug)]
pub struct Endpoint(SocketAddr);
impl Endpoint {
    /// Accepts only a numeric loopback address with a nonzero port.
    pub fn parse(value: &str) -> Option<Self> {
        let address: SocketAddr = value.parse().ok()?;
        (address.ip().is_loopback() && address.port() != 0).then_some(Self(address))
    }
}

/// Fixed ACL profiles selected by the administrator, never by provider rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// GET and PING; keys restricted to `http:*`.
    Reader,
    /// GET, SET and PING; keys restricted to `http:*`.
    Writer,
}
impl Role {
    /// Parses the two administrator-selectable profiles.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "reader" => Some(Self::Reader),
            "writer" => Some(Self::Writer),
            _ => None,
        }
    }
}

/// Usernames are 1..=64 UTF-8 bytes without control characters.
pub fn valid_username(username: &[u8]) -> bool {
    !username.is_empty()
        && username.len() <= 64
        && std::str::from_utf8(username).is_ok_and(|text| !text.chars().any(char::is_control))
}
/// Passwords are 1..=256 UTF-8 bytes. Binary AUTH is explicitly unsupported.
pub fn valid_credentials(username: &[u8], password: &[u8]) -> bool {
    valid_username(username)
        && !password.is_empty()
        && password.len() <= 256
        && std::str::from_utf8(password).is_ok()
}

// ureq's default timed resolver starts an unjoined DNS helper. This resolver
// accepts only the already validated fixed URI and performs no name lookup.
#[derive(Debug)]
struct FixedResolver(Endpoint);
impl Resolver for FixedResolver {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        _: &ureq::config::Config,
        _: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        if uri.to_string() != format!("http://{}/verify", self.0.0) {
            return Err(ureq::Error::HostNotFound);
        }
        let mut result = self.empty();
        result.push(self.0.0);
        Ok(result)
    }
}

#[derive(Serialize)]
struct Request<'a> {
    version: u8,
    username: &'a str,
    password: &'a str,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    version: u8,
    allowed: bool,
    username: String,
    role: Role,
}

/// A synchronous client without an executor, DNS, retries or idle connections.
/// Clones share cancellation. It is recreated for each activation.
#[derive(Clone)]
pub struct Provider {
    agent: ureq::Agent,
    endpoint: Endpoint,
}
impl Provider {
    /// Constructs a client without starting threads or network I/O.
    pub fn new(endpoint: Endpoint) -> Self {
        let config = ureq::Agent::config_builder()
            .proxy(None)
            .max_redirects(0)
            .http_status_as_error(false)
            .max_response_header_size(HEADER_LIMIT)
            .input_buffer_size(8192)
            .output_buffer_size(4096)
            .max_idle_connections(0)
            .max_idle_connections_per_host(0)
            .timeout_global(Some(HTTP_DEADLINE))
            .timeout_connect(Some(Duration::from_millis(100)))
            .timeout_per_read(Some(Duration::from_millis(400)))
            .timeout_per_write(Some(Duration::from_millis(100)))
            .build();
        Self {
            agent: ureq::Agent::with_parts(
                config,
                DefaultConnector::default(),
                FixedResolver(endpoint),
            ),
            endpoint,
        }
    }
    /// Interrupts socket I/O and refuses later requests. A connect in progress
    /// remains bounded by the 100ms connect deadline. Does not join any thread.
    pub fn cancel(&self) {
        self.agent.clone().shutdown();
    }
    /// Returns only allow/deny. Provider data and errors are never formatted.
    /// Success must match both the requested name and its configured profile.
    pub fn verify(
        &self,
        username: &[u8],
        password: &[u8],
        role: Role,
        cancelled: &AtomicBool,
        deadline: Instant,
    ) -> bool {
        if !valid_credentials(username, password)
            || cancelled.load(Ordering::Acquire)
            || Instant::now() >= deadline
        {
            return false;
        }
        let Ok(username) = std::str::from_utf8(username) else {
            return false;
        };
        let Ok(password) = std::str::from_utf8(password) else {
            return false;
        };
        let Ok(mut body) = serde_json::to_vec(&Request {
            version: 1,
            username,
            password,
        }) else {
            return false;
        };
        if body.len() > REQUEST_LIMIT {
            body.fill(0);
            return false;
        }
        // The explicit JSON copy is confined to this worker and cleared after
        // send. Library/socket/allocator copies cannot be guaranteed erased.
        let result = self
            .agent
            .post(format!("http://{}/verify", self.endpoint.0))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .header("Connection", "close")
            .config()
            .timeout_global(Some(
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(HTTP_DEADLINE),
            ))
            .build()
            .send(body.as_slice());
        body.fill(0);
        let Ok(mut response) = result else {
            return false;
        };
        if response.status() != 200
            || response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                != Some("application/json")
            || response.headers().contains_key("content-encoding")
        {
            return false;
        }
        let Ok(bytes) = response
            .body_mut()
            .with_config()
            .limit(RESPONSE_LIMIT)
            .read_to_vec()
        else {
            return false;
        };
        let Ok(response) = serde_json::from_slice::<Response>(&bytes) else {
            return false;
        };
        !cancelled.load(Ordering::Acquire)
            && Instant::now() < deadline
            && response.version == 1
            && response.allowed
            && response.username == username
            && response.role == role
    }
}
