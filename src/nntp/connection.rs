use bytes::Bytes;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::io::{
    AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf,
};
use tokio::net::TcpStream;
use tokio::time::{timeout, Duration};
use tokio_native_tls::TlsConnector;

use super::counting::CountingReader;
use super::speed_limit::{ConnThrottle, ReadThrottle, SpeedLimiter};
use super::yenc;
use crate::config::UsenetConfig;
use crate::error::{DlNzbError, NntpError};

type Result<T> = std::result::Result<T, DlNzbError>;

const READ_BUFFER_BYTES: usize = 256 * 1024;
const MAX_ARTICLE_BODY_BYTES: usize = 32 * 1024 * 1024;
const MAX_RESPONSE_LINE_BYTES: usize = 8192;
/// RFC 3977 §3.6: a message id is at most 250 octets with its angle brackets.
const MAX_MESSAGE_ID_BYTES: usize = 248;

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
    /// Greetings that turn the connection away: service temporarily (400)
    /// or permanently (502) unavailable, often "too many connections".
    pub const GREETING_REFUSED: [&str; 2] = ["400", "502"];
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
        /// The whole file's size as the article's `=ybegin size=` gives it,
        /// if it does (the part lies inside it).
        file_size: Option<u64>,
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

/// Whether `id` (without its angle brackets) may go into a command. RFC 3977
/// §3.6 allows printable US-ASCII only, and no `>`; `<` is refused too. Above
/// all this keeps out CR and LF, which would end the command early and have
/// the server read the rest as further commands.
pub fn is_valid_message_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_MESSAGE_ID_BYTES
        && id
            .bytes()
            .all(|b| b.is_ascii_graphic() && b != b'<' && b != b'>')
}

/// Whether `name` may go into a `GROUP` command: not empty, and no spaces or
/// control characters (CR and LF above all).
pub fn is_valid_group_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b > b' ' && b != 0x7f)
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
    /// Commands sent whose responses have not been fully read yet. Non-zero when
    /// the connection is handed back means an exchange was abandoned midway (a
    /// cancelled read), so the wire is out of sync and the connection must be
    /// discarded rather than recycled. Leftover pipelined `222` lines would
    /// otherwise pass the `DATE` health check and corrupt the next user.
    outstanding: usize,
    /// Plaintext bytes read on this connection since the last
    /// [`take_bytes_read`](Self::take_bytes_read). Per connection rather than
    /// per pool so each job counts only its own traffic.
    bytes_read: Arc<AtomicU64>,
    /// This connection's link to the engine's speed limit, if it has one:
    /// article bodies are metered, and time spent waiting on the limit does
    /// not count toward read timeouts.
    throttle: Option<Arc<ConnThrottle>>,
    /// The server's greeting line (e.g. `200 news.example.com ready`).
    greeting: String,
    /// The server's host name, for errors.
    server: String,
}

impl AsyncNntpConnection {
    pub async fn connect(
        config: &UsenetConfig,
        tls_connector: Option<Arc<TlsConnector>>,
    ) -> Result<Self> {
        Self::connect_limited(config, tls_connector, None).await
    }

    /// [`connect`](Self::connect), reading article bodies no faster than
    /// `limiter` allows (shared with every other connection it is given to).
    pub async fn connect_limited(
        config: &UsenetConfig,
        tls_connector: Option<Arc<TlsConnector>>,
        limiter: Option<Arc<SpeedLimiter>>,
    ) -> Result<Self> {
        // Resolve first, separately from connecting, so a bad host name is
        // reported as a DNS failure rather than a generic connection error.
        let addrs: Vec<std::net::SocketAddr> = timeout(
            TIMEOUT_CONNECT,
            tokio::net::lookup_host((config.server.as_str(), config.port)),
        )
        .await
        .map_err(|_| NntpError::Timeout {
            seconds: TIMEOUT_CONNECT.as_secs(),
        })?
        .map_err(|e| NntpError::DnsFailed {
            server: config.server.clone(),
            source: e,
        })?
        .collect();
        if addrs.is_empty() {
            return Err(NntpError::DnsFailed {
                server: config.server.clone(),
                source: std::io::Error::new(std::io::ErrorKind::NotFound, "no addresses"),
            }
            .into());
        }

        let tcp_stream = timeout(TIMEOUT_CONNECT, TcpStream::connect(&addrs[..]))
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

        let bytes_read_counter = Arc::new(AtomicU64::new(0));
        let throttle = limiter.map(|l| Arc::new(ConnThrottle::new(l)));
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

            // A handshake can fail because the server reset or closed the
            // connection (a provider turning away one connection too many
            // does that); that is a lost connection, not a TLS problem. One
            // that fails because the server answered in plain text (an NNTP
            // greeting where a TLS record belongs) is the wrong port for SSL.
            let (tcp_stream, watch) = Watched::new(tcp_stream);
            let tls_stream = timeout(
                TIMEOUT_CONNECT,
                connector.connect(&config.server, tcp_stream),
            )
            .await
            .map_err(|_| NntpError::Timeout {
                seconds: TIMEOUT_CONNECT.as_secs(),
            })?
            .map_err(|e| {
                if watch.plain.load(Ordering::Acquire) {
                    return NntpError::NotTls {
                        server: config.server.clone(),
                        port: config.port,
                    };
                }
                match watch.lost.get() {
                    Some(&kind) => NntpError::ConnectionLost {
                        server: config.server.clone(),
                        source: std::io::Error::new(kind, e.to_string()),
                    },
                    None => NntpError::TlsError {
                        server: config.server.clone(),
                        detail: e.to_string(),
                    },
                }
            })?;

            let (read_half, write_half) = tokio::io::split(tls_stream);
            let counted = counted(read_half, &bytes_read_counter, &throttle);
            (Box::new(counted), Box::new(write_half))
        } else {
            let (read_half, write_half) = tokio::io::split(tcp_stream);
            let counted = counted(read_half, &bytes_read_counter, &throttle);
            (Box::new(counted), Box::new(write_half))
        };

        let reader = BufReader::with_capacity(READ_BUFFER_BYTES, reader);

        let mut conn = Self {
            writer,
            reader,
            current_group: None,
            poisoned: false,
            outstanding: 0,
            bytes_read: bytes_read_counter,
            throttle,
            greeting: String::new(),
            server: config.server.clone(),
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
            // 400/502 instead of a welcome: the server is turning this
            // connection away (too many open, or no service right now).
            if code::GREETING_REFUSED
                .iter()
                .any(|c| greeting.starts_with(c))
            {
                return Err(NntpError::ConnectionFailed {
                    server: config.server.clone(),
                    port: config.port,
                    source: std::io::Error::new(
                        std::io::ErrorKind::ConnectionRefused,
                        greeting.chars().take(200).collect::<String>(),
                    ),
                }
                .into());
            }
            return Err(
                NntpError::ProtocolError(format!("Server greeting failed: {}", greeting)).into(),
            );
        }
        self.greeting = greeting.chars().take(200).collect();
        self.authenticate(config).await
    }

    /// The server's greeting line, as received (at most 200 characters).
    pub fn greeting(&self) -> &str {
        &self.greeting
    }

    /// Return and reset the plaintext bytes read since the last call. Workers
    /// call this after each response to attribute traffic to their job.
    pub fn take_bytes_read(&self) -> u64 {
        self.bytes_read.swap(0, Ordering::Relaxed)
    }

    /// The tally behind [`take_bytes_read`](Self::take_bytes_read), for a
    /// job that counts bytes as they arrive (it empties it with `swap`, so
    /// every byte is counted once whoever takes it).
    pub fn bytes_counter(&self) -> Arc<AtomicU64> {
        self.bytes_read.clone()
    }

    /// True when the connection can safely serve another user: not poisoned and
    /// no response left unread on the wire.
    pub fn is_clean(&self) -> bool {
        !self.poisoned && self.outstanding == 0
    }

    async fn authenticate(&mut self, config: &UsenetConfig) -> Result<()> {
        self.send_command(&format!("AUTHINFO USER {}", config.username))
            .await?;
        let response = timeout(TIMEOUT_AUTH, self.read_response())
            .await
            .map_err(|_| NntpError::Timeout {
                seconds: TIMEOUT_AUTH.as_secs(),
            })??;
        self.response_consumed();
        if response.starts_with(code::AUTH_PASSWORD_REQUIRED) {
            self.send_command(&format!("AUTHINFO PASS {}", config.password))
                .await?;
            let response = timeout(TIMEOUT_AUTH, self.read_response())
                .await
                .map_err(|_| NntpError::Timeout {
                    seconds: TIMEOUT_AUTH.as_secs(),
                })??;
            self.response_consumed();
            if !response.starts_with(code::AUTH_ACCEPTED) {
                return Err(login_refused(config, &response));
            }
        } else if !response.starts_with(code::AUTH_ACCEPTED) {
            return Err(login_refused(config, &response));
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
        let throttle = self.throttle.clone();
        match timeout_unthrottled(throttle.as_deref(), deadline, self.read_response()).await {
            Some(Ok(r)) => Ok(r),
            Some(Err(e)) => Err(self.poison(e)),
            None => Err(self.poison_timeout(deadline)),
        }
    }

    async fn read_article_body_poisoning(&mut self, deadline: Duration) -> Result<Vec<u8>> {
        let throttle = self.throttle.clone();
        match timeout_unthrottled(throttle.as_deref(), deadline, self.read_article_body()).await {
            Some(Ok(b)) => Ok(b),
            Some(Err(e)) => Err(self.poison(e)),
            None => Err(self.poison_timeout(deadline)),
        }
    }

    /// Meter the reads that follow against the speed limit (article bodies)
    /// or let them through and charge them afterwards (control traffic).
    fn set_metered(&self, metered: bool) {
        if let Some(throttle) = &self.throttle {
            throttle.set_metered(metered);
        }
    }

    /// One previously-sent command's response has been read in full.
    fn response_consumed(&mut self) {
        self.outstanding = self.outstanding.saturating_sub(1);
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
        if !is_valid_group_name(group) {
            // Nothing was sent, so the connection is still in sync.
            return Err(NntpError::ProtocolError("malformed newsgroup name".into()).into());
        }
        self.send_command(&format!("GROUP {}", group)).await?;
        let response = self.read_response_poisoning(TIMEOUT_GROUP).await?;
        self.response_consumed();
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
    /// [`read_body_outcome`]. A write error poisons the connection; a
    /// malformed message id ([`is_valid_message_id`]) is refused before
    /// anything is written, leaving the connection usable.
    pub async fn send_body(&mut self, message_id: &str) -> Result<()> {
        if !is_valid_message_id(message_id) {
            return Err(NntpError::ProtocolError("malformed message id".into()).into());
        }
        let mut buf = Vec::with_capacity(message_id.len() + 8);
        buf.extend_from_slice(b"BODY <");
        buf.extend_from_slice(message_id.as_bytes());
        buf.extend_from_slice(b">\r\n");
        self.writer
            .write_all(&buf)
            .await
            .map_err(|e| self.poison_with(e))?;
        self.outstanding += 1;
        Ok(())
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
    ///
    /// The only metered read: it waits on the engine's speed limit, if any,
    /// and the read timeouts exclude that waiting.
    pub async fn read_body_outcome(&mut self, message_id: &str) -> ArticleOutcome {
        self.read_body_outcome_within(message_id, TIMEOUT_RESPONSE_HEAD)
            .await
    }

    /// [`read_body_outcome`](Self::read_body_outcome), waiting at most
    /// `head_timeout` for the reply's status line.
    pub async fn read_body_outcome_within(
        &mut self,
        message_id: &str,
        head_timeout: Duration,
    ) -> ArticleOutcome {
        self.set_metered(true);
        let outcome = self
            .read_body_outcome_metered(message_id, head_timeout)
            .await;
        self.set_metered(false);
        outcome
    }

    async fn read_body_outcome_metered(
        &mut self,
        message_id: &str,
        head_timeout: Duration,
    ) -> ArticleOutcome {
        let mid = || message_id.to_string();
        let response = match self.read_response_poisoning(head_timeout).await {
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
            self.response_consumed();
            match yenc::decode_article(&body) {
                Ok(decoded) => ArticleOutcome::Ok {
                    message_id: mid(),
                    offset: decoded.offset,
                    data: Bytes::from(decoded.data),
                    crc_verified: decoded.crc_verified,
                    file_size: decoded.file_size,
                },
                Err(e) => {
                    // The wire is in sync (full body consumed); only the payload
                    // is bad. Bounded retry happens at the caller.
                    tracing::debug!("yEnc decode failed for {}: {}", message_id, e);
                    ArticleOutcome::DecodeFailed { message_id: mid() }
                }
            }
        } else if code::NO_ARTICLE.iter().any(|c| response.starts_with(c)) {
            self.response_consumed();
            ArticleOutcome::Missing { message_id: mid() }
        } else if response.starts_with(code::NO_GROUP_SELECTED) {
            // Server insists on a selected group; drop the cache so the worker
            // re-selects before retrying this (transient) article.
            self.response_consumed();
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
    /// STAT with message-id form doesn't require GROUP selection. Each result
    /// is `Some(true)` (223), `Some(false)` (430/423, no such article; also a
    /// malformed id, which is never sent) or `None` when the reply says
    /// nothing about the article (480, 500, 502, ...).
    pub async fn check_articles_exist(
        &mut self,
        requests: &[SegmentRequest],
    ) -> Result<Vec<(String, Option<bool>)>> {
        let sent = requests
            .iter()
            .filter(|r| is_valid_message_id(&r.message_id))
            .count();
        if sent > 0 {
            let mut buf = Vec::with_capacity(sent * 64);
            for req in requests
                .iter()
                .filter(|r| is_valid_message_id(&r.message_id))
            {
                buf.extend_from_slice(b"STAT <");
                buf.extend_from_slice(req.message_id.as_bytes());
                buf.extend_from_slice(b">\r\n");
            }
            self.writer
                .write_all(&buf)
                .await
                .map_err(|e| self.poison_with(e))?;
            self.outstanding += sent;
            self.writer.flush().await.map_err(|e| self.poison_with(e))?;
        }

        let mut results = Vec::with_capacity(requests.len());
        for req in requests {
            if !is_valid_message_id(&req.message_id) {
                results.push((req.message_id.clone(), Some(false)));
                continue;
            }
            let response = self.read_response_poisoning(TIMEOUT_RESPONSE_HEAD).await?;
            self.response_consumed();
            let exists = if response.starts_with(code::STAT_OK) {
                Some(true)
            } else if code::NO_ARTICLE.iter().any(|c| response.starts_with(c)) {
                Some(false)
            } else {
                None
            };
            results.push((req.message_id.clone(), exists));
        }
        Ok(results)
    }

    /// Read a dot-terminated body, undoing dot-stuffing and ending every line
    /// with a bare `\n`. Lines are read straight into the body and never past
    /// the room left under [`MAX_ARTICLE_BODY_BYTES`], so a line that never
    /// ends can't hold more memory than that.
    async fn read_article_body(&mut self) -> Result<Vec<u8>> {
        let too_long = || {
            DlNzbError::from(NntpError::ProtocolError(format!(
                "article body exceeds {} bytes",
                MAX_ARTICLE_BODY_BYTES
            )))
        };
        /// The line that ends a body.
        const TERMINATOR: &[u8] = b".\r\n";
        let mut body = Vec::with_capacity(768 * 1024);
        loop {
            let start = body.len();
            // Room for one more line whose content still fits (its CRLF
            // becomes a single `\n`), and always for the terminator, which
            // isn't part of the body: the limit is on the body.
            let room = (MAX_ARTICLE_BODY_BYTES + 2)
                .saturating_sub(start)
                .max(TERMINATOR.len()) as u64;
            let n = (&mut self.reader)
                .take(room)
                .read_until(b'\n', &mut body)
                .await
                .map_err(|e| lost(&self.server, e))?;
            if n == 0 {
                return Err(lost(&self.server, closed_by_server()));
            }
            let line = &body[start..];
            if line.last() != Some(&b'\n') {
                if n as u64 == room {
                    return Err(too_long());
                }
                return Err(NntpError::ProtocolError("unterminated article body".into()).into());
            }
            if line == TERMINATOR || line == b".\n" {
                body.truncate(start);
                return Ok(body);
            }
            // Dot-stuffing: leading ".." becomes "."
            if line.starts_with(b"..") {
                body.remove(start);
            }
            let line_end = if body.ends_with(b"\r\n") { 2 } else { 1 };
            body.truncate(body.len() - line_end);
            body.push(b'\n');

            if body.len() > MAX_ARTICLE_BODY_BYTES {
                return Err(too_long());
            }
        }
    }

    async fn send_command(&mut self, command: &str) -> Result<()> {
        // A line break would make the rest of `command` another command.
        if command.contains(['\r', '\n']) {
            return Err(NntpError::ProtocolError("line break in a command".into()).into());
        }
        self.writer
            .write_all(command.as_bytes())
            .await
            .map_err(|e| self.poison_with(e))?;
        self.writer
            .write_all(b"\r\n")
            .await
            .map_err(|e| self.poison_with(e))?;
        self.outstanding += 1;
        self.writer.flush().await.map_err(|e| self.poison_with(e))?;
        Ok(())
    }

    /// Read one response line of at most [`MAX_RESPONSE_LINE_BYTES`] (line
    /// end included); a longer one is an error before more of it is read.
    /// The server closing or resetting the connection is
    /// [`NntpError::ConnectionLost`].
    async fn read_response(&mut self) -> Result<String> {
        let mut line = Vec::with_capacity(128);
        (&mut self.reader)
            .take(MAX_RESPONSE_LINE_BYTES as u64)
            .read_until(b'\n', &mut line)
            .await
            .map_err(|e| lost(&self.server, e))?;
        if line.last() != Some(&b'\n') {
            if line.len() >= MAX_RESPONSE_LINE_BYTES {
                return Err(NntpError::ProtocolError("response line too long".into()).into());
            }
            // The stream ended before the line did.
            return Err(lost(&self.server, closed_by_server()));
        }
        if line.ends_with(b"\r\n") {
            line.truncate(line.len() - 2);
        } else if line.ends_with(b"\n") {
            line.truncate(line.len() - 1);
        }
        Ok(String::from_utf8_lossy(&line).into_owned())
    }

    /// Health probe. Sends `DATE` (mandatory READER command per RFC 3977 §7.1)
    /// because some providers reject `NOOP`. Any well-formed 1xx/2xx response
    /// proves the connection is round-trip-capable.
    pub async fn is_healthy(&mut self) -> bool {
        if self.poisoned {
            return false;
        }
        // Control traffic: never held up by the speed limit.
        self.set_metered(false);
        if self.send_command("DATE").await.is_err() {
            self.poisoned = true;
            return false;
        }
        match timeout(TIMEOUT_HEALTH, self.read_response()).await {
            Ok(Ok(response)) => {
                self.response_consumed();
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

    /// A write failed: the connection is gone (or going).
    fn poison_with(&mut self, e: std::io::Error) -> DlNzbError {
        self.poisoned = true;
        lost(&self.server, e)
    }
}

/// The server reset or closed the connection to `server`.
fn lost(server: &str, source: std::io::Error) -> DlNzbError {
    NntpError::ConnectionLost {
        server: server.to_string(),
        source,
    }
    .into()
}

/// The error for a stream that ended where more was expected.
fn closed_by_server() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::UnexpectedEof,
        "the server closed the connection",
    )
}

/// The TCP stream under TLS, noting the first time the server closes or
/// resets it (or a write finds it gone): `lost` tells a handshake that failed
/// for that reason from one that failed on its own terms (certificate,
/// protocol). It also looks at the first byte the server sends: a TLS record
/// starts with a content type (0x14 to 0x18) and an old SSLv2 one with its
/// high bit set, never with printable ASCII, which is a plain-text server (an
/// NNTP greeting, "200 ..."). Then `plain` is set and the read fails, so the
/// handshake ends at once. On the read path it costs one comparison per read.
struct Watched<S> {
    inner: S,
    watch: Arc<HandshakeWatch>,
    /// The first byte from the server was looked at.
    first_seen: bool,
}

/// What [`Watched`] noticed during a TLS handshake.
#[derive(Default)]
struct HandshakeWatch {
    lost: std::sync::OnceLock<std::io::ErrorKind>,
    plain: std::sync::atomic::AtomicBool,
}

impl<S> Watched<S> {
    fn new(inner: S) -> (Self, Arc<HandshakeWatch>) {
        let watch = Arc::new(HandshakeWatch::default());
        (
            Self {
                inner,
                watch: watch.clone(),
                first_seen: false,
            },
            watch,
        )
    }

    fn note<T>(&self, result: &Poll<std::io::Result<T>>) {
        if let Poll::Ready(Err(e)) = result {
            let _ = self.watch.lost.set(e.kind());
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Watched<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let wanted = buf.remaining() > 0;
        let result = Pin::new(&mut self.inner).poll_read(cx, buf);
        match &result {
            Poll::Ready(Ok(())) if wanted && buf.filled().len() == before => {
                let _ = self.watch.lost.set(std::io::ErrorKind::UnexpectedEof);
            }
            Poll::Ready(Ok(())) if !self.first_seen && buf.filled().len() > before => {
                self.first_seen = true;
                if (0x20..0x7f).contains(&buf.filled()[before]) {
                    self.watch.plain.store(true, Ordering::Release);
                    return Poll::Ready(Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "the server answered in plain text, not TLS",
                    )));
                }
            }
            _ => self.note(&result),
        }
        result
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Watched<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let result = Pin::new(&mut self.inner).poll_write(cx, buf);
        self.note(&result);
        result
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let result = Pin::new(&mut self.inner).poll_flush(cx);
        self.note(&result);
        result
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// The connection's read half, counted and (with a throttle) speed-limited.
fn counted<R>(
    read_half: R,
    counter: &Arc<AtomicU64>,
    throttle: &Option<Arc<ConnThrottle>>,
) -> CountingReader<R> {
    let reader = CountingReader::new(read_half, counter.clone());
    match throttle {
        Some(t) => reader.with_throttle(ReadThrottle::new(t.clone())),
        None => reader,
    }
}

/// `timeout(deadline, fut)`, except that time the connection spends waiting on
/// the speed limit doesn't count: a low limit shared by many connections makes
/// each article slow without the server being at fault. `None` on timeout.
async fn timeout_unthrottled<F: std::future::Future>(
    throttle: Option<&ConnThrottle>,
    deadline: Duration,
    fut: F,
) -> Option<F::Output> {
    let Some(throttle) = throttle else {
        return timeout(deadline, fut).await.ok();
    };
    let mut fut = std::pin::pin!(fut);
    let mut counted = throttle.throttled();
    let mut until = tokio::time::Instant::now() + deadline;
    loop {
        match tokio::time::timeout_at(until, fut.as_mut()).await {
            Ok(output) => return Some(output),
            Err(_) => {
                // Push the deadline back by the time spent throttled since
                // it was set; give up only when none was.
                let throttled = throttle.throttled();
                let extra = throttled.saturating_sub(counted);
                if extra.is_zero() {
                    return None;
                }
                counted = throttled;
                until += extra;
            }
        }
    }
}

/// The error for a login the server refused with `response`. Providers
/// answer one connection too many at login ("502 Too many connections", "482
/// Too many connections for your user"): that is the connection turned away,
/// like a refusing greeting, and worth asking again; anything else is the
/// username or password.
fn login_refused(config: &UsenetConfig, response: &str) -> DlNzbError {
    let lower = response.to_ascii_lowercase();
    let too_many = [
        "too many connection",
        "too many session",
        "connection limit",
    ]
    .iter()
    .any(|phrase| lower.contains(phrase));
    if too_many {
        return NntpError::ConnectionFailed {
            server: config.server.clone(),
            port: config.port,
            source: std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                format!("{} too many connections", sanitize_response(response)),
            ),
        }
        .into();
    }
    NntpError::AuthFailed(sanitize_response(response)).into()
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    /// Read 128 KiB at 160 KiB/s: the 64 KiB burst at once, then ~400 ms
    /// waiting on the limit.
    async fn slow_read(throttle: Arc<ConnThrottle>) {
        throttle.set_metered(true);
        let counter = Arc::new(AtomicU64::new(0));
        let mut reader = counted(tokio::io::repeat(0), &counter, &Some(throttle));
        let mut buf = vec![0u8; 128 * 1024];
        reader.read_exact(&mut buf).await.unwrap();
    }

    #[tokio::test]
    async fn read_timeouts_exclude_time_waiting_on_the_limit() {
        let deadline = Duration::from_millis(100);
        let limiter = Arc::new(SpeedLimiter::new(Some(160 * 1024)));

        let throttle = Arc::new(ConnThrottle::new(limiter.clone()));
        let read = slow_read(throttle.clone());
        let done = timeout_unthrottled(Some(&throttle), deadline, read).await;
        assert!(
            done.is_some(),
            "waiting on the limit counted toward the deadline"
        );
        assert!(throttle.throttled() >= Duration::from_millis(250));

        // A plain deadline gives up on the same read (fresh bucket).
        limiter.set(None);
        limiter.set(Some(160 * 1024));
        let throttle = Arc::new(ConnThrottle::new(limiter));
        let done = timeout_unthrottled(None, deadline, slow_read(throttle)).await;
        assert!(done.is_none());

        // With no time spent throttled, the deadline holds.
        let idle = Arc::new(ConnThrottle::new(Arc::new(SpeedLimiter::new(None))));
        let started = tokio::time::Instant::now();
        let never = std::future::pending::<()>();
        assert!(timeout_unthrottled(Some(&idle), deadline, never)
            .await
            .is_none());
        assert!(started.elapsed() < Duration::from_millis(300));
    }
}
