use bytes::Bytes;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::time::{timeout, Duration};
use tokio_native_tls::TlsConnector;

use super::counting::CountingReader;
use super::yenc;
use crate::config::UsenetConfig;
use crate::error::{DlNzbError, NntpError};

type Result<T> = std::result::Result<T, DlNzbError>;

const READ_BUFFER_BYTES: usize = 256 * 1024;
const MAX_ARTICLE_BODY_BYTES: usize = 32 * 1024 * 1024;
const MAX_RESPONSE_LINE_BYTES: usize = 8192;

const TIMEOUT_CONNECT: Duration = Duration::from_secs(30);
const TIMEOUT_AUTH: Duration = Duration::from_secs(30);
const TIMEOUT_GROUP: Duration = Duration::from_secs(15);
const TIMEOUT_RESPONSE_HEAD: Duration = Duration::from_secs(60);
const TIMEOUT_RESPONSE_BODY: Duration = Duration::from_secs(120);
const TIMEOUT_HEALTH: Duration = Duration::from_secs(5);

/// NNTP response codes we react to (RFC 3977).
mod code {
    pub const GREETING_OK: &str = "200";
    pub const GREETING_NO_POST: &str = "201";
    pub const AUTH_ACCEPTED: &str = "281";
    pub const AUTH_PASSWORD_REQUIRED: &str = "381";
    pub const GROUP_OK: &str = "211";
    pub const STAT_OK: &str = "223";
    pub const BODY_FOLLOWS: &str = "222";
    /// Statuses meaning "no such article" — the body is not sent. Retrying is futile.
    pub const NO_ARTICLE: [&str; 2] = ["430", "423"];
    /// "No newsgroup selected" — recoverable by (re-)selecting a group.
    pub const NO_GROUP_SELECTED: &str = "412";
}

/// Outcome of fetching a single article. Unlike a `Result`, this distinguishes
/// the failure modes so the caller can choose the right recovery: permanent
/// failures are never retried, decode failures are retried a small bounded
/// number of times, and transient (wire/connection) failures are retried
/// without counting against the per-article budget.
#[derive(Debug)]
pub enum ArticleOutcome {
    /// Successfully downloaded and decoded — placement uses the yEnc `=ypart` offset.
    Ok {
        message_id: String,
        offset: u64,
        data: Bytes,
        /// True iff the article carried a pcrc32/crc32 that matched on decode.
        crc_verified: bool,
    },
    /// Server reported the article doesn't exist (430/423). Permanent.
    Missing { message_id: String },
    /// Body arrived but failed to yEnc-decode (CRC/size mismatch). The wire is
    /// still in sync (whole body consumed); retry a bounded number of times.
    DecodeFailed { message_id: String },
    /// Wire/timeout/unexpected error. The connection is poisoned; retry the
    /// article on a fresh connection without counting it against the budget.
    Transient { message_id: String },
}

/// A request for one article.
#[derive(Clone, Debug)]
pub struct SegmentRequest {
    pub message_id: String,
    pub group: String,
}

/// Async NNTP connection that can be pooled.
pub struct AsyncNntpConnection {
    writer: Box<dyn AsyncWrite + Unpin + Send>,
    reader: BufReader<Box<dyn AsyncRead + Unpin + Send>>,
    current_group: Option<String>,
    /// Once a connection enters an unrecoverable state (mid-stream read failure),
    /// flip this so the pool can recycle it instead of returning poisoned bytes.
    poisoned: bool,
}

impl AsyncNntpConnection {
    pub async fn connect(
        config: &UsenetConfig,
        tls_connector: Option<Arc<TlsConnector>>,
        bytes_read_counter: Arc<AtomicU64>,
    ) -> Result<Self> {
        let addr = format!("{}:{}", config.server, config.port);

        let tcp_stream = timeout(TIMEOUT_CONNECT, TcpStream::connect(&addr))
            .await
            .map_err(|_| NntpError::Timeout {
                seconds: TIMEOUT_CONNECT.as_secs(),
            })?
            .map_err(|e| NntpError::ConnectionFailed {
                server: config.server.clone(),
                port: config.port,
                source: e,
            })?;

        tcp_stream.set_nodelay(true)?;

        // Enlarge the receive buffer so a single high-RTT connection isn't capped by
        // the bandwidth-delay product, and enable keepalive so half-open connections
        // from flaky providers are detected by the OS rather than only by read timeouts.
        {
            use socket2::{SockRef, TcpKeepalive};
            let sref = SockRef::from(&tcp_stream);
            let _ = sref.set_recv_buffer_size(4 * 1024 * 1024);
            let _ = sref.set_tcp_keepalive(
                &TcpKeepalive::new().with_time(std::time::Duration::from_secs(60)),
            );
        }

        let (reader, writer): (
            Box<dyn AsyncRead + Unpin + Send>,
            Box<dyn AsyncWrite + Unpin + Send>,
        ) = if config.ssl {
            let connector = if let Some(shared) = tls_connector {
                shared
            } else {
                let mut tls_builder = native_tls::TlsConnector::builder();
                if !config.verify_ssl_certs {
                    tls_builder.danger_accept_invalid_certs(true);
                    tls_builder.danger_accept_invalid_hostnames(true);
                }
                let native_connector = tls_builder.build()?;
                Arc::new(TlsConnector::from(native_connector))
            };

            let tls_stream = timeout(
                TIMEOUT_CONNECT,
                connector.connect(&config.server, tcp_stream),
            )
            .await
            .map_err(|_| NntpError::Timeout {
                seconds: TIMEOUT_CONNECT.as_secs(),
            })?
            .map_err(|e| NntpError::TlsError(e.to_string()))?;

            let (read_half, write_half) = tokio::io::split(tls_stream);
            let counted = CountingReader::new(read_half, bytes_read_counter);
            (Box::new(counted), Box::new(write_half))
        } else {
            let (read_half, write_half) = tokio::io::split(tcp_stream);
            let counted = CountingReader::new(read_half, bytes_read_counter);
            (Box::new(counted), Box::new(write_half))
        };

        let reader = BufReader::with_capacity(READ_BUFFER_BYTES, reader);

        let mut conn = Self {
            writer,
            reader,
            current_group: None,
            poisoned: false,
        };

        conn.initialize(config).await?;
        Ok(conn)
    }

    async fn initialize(&mut self, config: &UsenetConfig) -> Result<()> {
        let greeting = timeout(TIMEOUT_AUTH, self.read_response())
            .await
            .map_err(|_| NntpError::Timeout {
                seconds: TIMEOUT_AUTH.as_secs(),
            })??;
        if !greeting.starts_with(code::GREETING_OK) && !greeting.starts_with(code::GREETING_NO_POST)
        {
            return Err(
                NntpError::ProtocolError(format!("Server greeting failed: {}", greeting)).into(),
            );
        }
        self.authenticate(config).await
    }

    async fn authenticate(&mut self, config: &UsenetConfig) -> Result<()> {
        self.send_command(&format!("AUTHINFO USER {}", config.username))
            .await?;
        let response = timeout(TIMEOUT_AUTH, self.read_response())
            .await
            .map_err(|_| NntpError::Timeout {
                seconds: TIMEOUT_AUTH.as_secs(),
            })??;
        if response.starts_with(code::AUTH_PASSWORD_REQUIRED) {
            self.send_command(&format!("AUTHINFO PASS {}", config.password))
                .await?;
            let response = timeout(TIMEOUT_AUTH, self.read_response())
                .await
                .map_err(|_| NntpError::Timeout {
                    seconds: TIMEOUT_AUTH.as_secs(),
                })??;
            if !response.starts_with(code::AUTH_ACCEPTED) {
                return Err(NntpError::AuthFailed(sanitize_response(&response)).into());
            }
        } else if !response.starts_with(code::AUTH_ACCEPTED) {
            return Err(NntpError::AuthFailed(sanitize_response(&response)).into());
        }
        Ok(())
    }

    /// Returns true once the connection has hit an unrecoverable mid-stream
    /// error. Callers should discard it instead of recycling.
    pub fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    /// Read a response line with a deadline; any failure (I/O error or timeout)
    /// poisons the connection because the wire is now out of sync.
    async fn read_response_poisoning(&mut self, deadline: Duration) -> Result<String> {
        match timeout(deadline, self.read_response()).await {
            Ok(Ok(r)) => Ok(r),
            Ok(Err(e)) => Err(self.poison(e)),
            Err(_) => Err(self.poison_timeout(deadline)),
        }
    }

    async fn read_article_body_poisoning(&mut self, deadline: Duration) -> Result<Vec<u8>> {
        match timeout(deadline, self.read_article_body()).await {
            Ok(Ok(b)) => Ok(b),
            Ok(Err(e)) => Err(self.poison(e)),
            Err(_) => Err(self.poison_timeout(deadline)),
        }
    }

    fn poison(&mut self, e: DlNzbError) -> DlNzbError {
        self.poisoned = true;
        e
    }

    fn poison_timeout(&mut self, deadline: Duration) -> DlNzbError {
        self.poisoned = true;
        NntpError::Timeout {
            seconds: deadline.as_secs(),
        }
        .into()
    }

    /// Select an NNTP group, caching the last group to avoid redundant traffic.
    ///
    /// We retrieve articles by message-id (`BODY <id>`), which per RFC 3977 is
    /// group-independent, so this only needs to be called once per connection to
    /// satisfy legacy servers that demand a `GROUP` before serving any article.
    /// A failed selection is non-fatal: most servers still serve `BODY <id>`.
    /// A wire/timeout error poisons the connection (the stream is out of sync).
    pub async fn ensure_group(&mut self, group: &str) -> Result<()> {
        if self.current_group.as_deref() == Some(group) {
            return Ok(());
        }
        self.send_command(&format!("GROUP {}", group)).await?;
        let response = self.read_response_poisoning(TIMEOUT_GROUP).await?;
        if !response.starts_with(code::GROUP_OK) {
            // GROUP failure (e.g. 411) is not poisoning — no body follows.
            return Err(NntpError::GroupNotFound {
                group: group.to_string(),
            }
            .into());
        }
        self.current_group = Some(group.to_string());
        Ok(())
    }

    /// Queue a `BODY <message-id>` request without flushing. The caller keeps
    /// several requests in flight (a sliding window) to hide round-trip latency,
    /// flushing once per fill and draining responses in order with
    /// [`read_body_outcome`]. A write error poisons the connection.
    pub async fn send_body(&mut self, message_id: &str) -> Result<()> {
        let mut buf = Vec::with_capacity(message_id.len() + 8);
        buf.extend_from_slice(b"BODY <");
        buf.extend_from_slice(message_id.as_bytes());
        buf.extend_from_slice(b">\r\n");
        self.writer
            .write_all(&buf)
            .await
            .map_err(|e| self.poison_with(e))
    }

    /// Flush queued requests to the socket.
    pub async fn flush(&mut self) -> Result<()> {
        self.writer.flush().await.map_err(|e| self.poison_with(e))
    }

    /// Read and classify the response for one previously-sent `BODY` request.
    ///
    /// Never returns `Err`: any wire/timeout failure poisons the connection and
    /// is reported as `Transient` so the caller retries the article elsewhere.
    /// Responses must be drained in the same order the `BODY` commands were sent.
    pub async fn read_body_outcome(&mut self, message_id: &str) -> ArticleOutcome {
        let mid = || message_id.to_string();
        let response = match self.read_response_poisoning(TIMEOUT_RESPONSE_HEAD).await {
            Ok(r) => r,
            Err(_) => return ArticleOutcome::Transient { message_id: mid() },
        };

        if response.starts_with(code::BODY_FOLLOWS) {
            let body = match self
                .read_article_body_poisoning(TIMEOUT_RESPONSE_BODY)
                .await
            {
                Ok(b) => b,
                Err(_) => return ArticleOutcome::Transient { message_id: mid() },
            };
            match yenc::decode_article(&body) {
                Ok(decoded) => ArticleOutcome::Ok {
                    message_id: mid(),
                    offset: decoded.offset,
                    data: Bytes::from(decoded.data),
                    crc_verified: decoded.crc_verified,
                },
                Err(e) => {
                    // The wire is in sync (full body consumed); only the payload
                    // is bad. Bounded retry happens at the caller.
                    tracing::debug!("yEnc decode failed for {}: {}", message_id, e);
                    ArticleOutcome::DecodeFailed { message_id: mid() }
                }
            }
        } else if code::NO_ARTICLE.iter().any(|c| response.starts_with(c)) {
            ArticleOutcome::Missing { message_id: mid() }
        } else if response.starts_with(code::NO_GROUP_SELECTED) {
            // Server insists on a selected group; drop the cache so the worker
            // re-selects before retrying this (transient) article.
            self.current_group = None;
            ArticleOutcome::Transient { message_id: mid() }
        } else {
            // Unknown status — the wire may be desynced relative to our request
            // stream. Poison so the connection is discarded.
            self.poisoned = true;
            ArticleOutcome::Transient { message_id: mid() }
        }
    }

    /// STAT a batch of message IDs to check existence without downloading.
    /// STAT with message-id form doesn't require GROUP selection. Errors that
    /// don't affect the wire state (timeouts on individual responses) leave
    /// the connection usable; we treat unanswered STATs as "missing".
    pub async fn check_articles_exist(
        &mut self,
        requests: &[SegmentRequest],
    ) -> Result<Vec<(String, bool)>> {
        if requests.is_empty() {
            return Ok(Vec::new());
        }

        let mut buf = Vec::with_capacity(requests.len() * 64);
        for req in requests {
            buf.extend_from_slice(b"STAT <");
            buf.extend_from_slice(req.message_id.as_bytes());
            buf.extend_from_slice(b">\r\n");
        }
        self.writer
            .write_all(&buf)
            .await
            .map_err(|e| self.poison_with(e))?;
        self.writer.flush().await.map_err(|e| self.poison_with(e))?;

        let mut results = Vec::with_capacity(requests.len());
        for req in requests {
            let response = self.read_response_poisoning(TIMEOUT_RESPONSE_HEAD).await?;
            let exists = response.starts_with(code::STAT_OK);
            results.push((req.message_id.clone(), exists));
        }
        Ok(results)
    }

    async fn read_article_body(&mut self) -> Result<Vec<u8>> {
        let mut body = Vec::with_capacity(768 * 1024);
        let mut line = Vec::new();
        let mut terminated = false;

        loop {
            line.clear();
            let n = self.reader.read_until(b'\n', &mut line).await?;
            if n == 0 {
                break;
            }

            if line == b".\r\n" || line == b".\n" {
                terminated = true;
                break;
            }
            // Dot-stuffing: leading ".." becomes "."
            if line.len() >= 2 && line[0] == b'.' && line[1] == b'.' {
                line.remove(0);
            }
            if line.ends_with(b"\r\n") {
                body.extend_from_slice(&line[..line.len() - 2]);
            } else if line.ends_with(b"\n") {
                body.extend_from_slice(&line[..line.len() - 1]);
            } else {
                body.extend_from_slice(&line);
            }
            body.push(b'\n');

            if body.len() > MAX_ARTICLE_BODY_BYTES {
                return Err(NntpError::ProtocolError(format!(
                    "article body exceeds {} bytes",
                    MAX_ARTICLE_BODY_BYTES
                ))
                .into());
            }
        }

        if !terminated {
            return Err(NntpError::ProtocolError("unterminated article body".into()).into());
        }
        Ok(body)
    }

    async fn send_command(&mut self, command: &str) -> Result<()> {
        self.writer
            .write_all(command.as_bytes())
            .await
            .map_err(|e| self.poison_with(e))?;
        self.writer
            .write_all(b"\r\n")
            .await
            .map_err(|e| self.poison_with(e))?;
        self.writer.flush().await.map_err(|e| self.poison_with(e))?;
        Ok(())
    }

    async fn read_response(&mut self) -> Result<String> {
        let mut response = String::new();
        self.reader.read_line(&mut response).await?;
        if response.len() > MAX_RESPONSE_LINE_BYTES {
            return Err(NntpError::ProtocolError("response line too long".into()).into());
        }
        if response.ends_with("\r\n") {
            response.truncate(response.len() - 2);
        } else if response.ends_with('\n') {
            response.truncate(response.len() - 1);
        }
        Ok(response)
    }

    /// Health probe. Sends `DATE` (mandatory READER command per RFC 3977 §7.1)
    /// because some providers reject `NOOP`. Any well-formed 1xx/2xx response
    /// proves the connection is round-trip-capable.
    pub async fn is_healthy(&mut self) -> bool {
        if self.poisoned {
            return false;
        }
        if self.send_command("DATE").await.is_err() {
            self.poisoned = true;
            return false;
        }
        match timeout(TIMEOUT_HEALTH, self.read_response()).await {
            Ok(Ok(response)) => {
                let first = response.chars().next();
                let healthy = matches!(first, Some('1') | Some('2'));
                if !healthy {
                    tracing::debug!("DATE returned non-OK response: {}", response);
                }
                healthy
            }
            Ok(Err(e)) => {
                tracing::debug!("DATE read_response error: {}", e);
                self.poisoned = true;
                false
            }
            Err(_) => {
                tracing::debug!("DATE timed out");
                self.poisoned = true;
                false
            }
        }
    }

    pub async fn close(&mut self) -> Result<()> {
        let _ = self.send_command("QUIT").await;
        let _ = timeout(Duration::from_secs(2), self.read_response()).await;
        Ok(())
    }

    fn poison_with(&mut self, e: std::io::Error) -> DlNzbError {
        self.poisoned = true;
        DlNzbError::from(e)
    }
}

fn sanitize_response(response: &str) -> String {
    // Strip everything after the response code so credentials can't leak.
    let code = response.split_whitespace().next().unwrap_or("");
    if code.is_empty() {
        "Unknown".into()
    } else {
        code.to_string()
    }
}
