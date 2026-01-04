use bytes::Bytes;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::time::{timeout, Duration};
use tokio_native_tls::TlsConnector;

use crate::config::UsenetConfig;
use crate::error::{DlNzbError, NntpError};

type Result<T> = std::result::Result<T, DlNzbError>;

/// Async NNTP connection that can be pooled
pub struct AsyncNntpConnection {
    writer: Box<dyn AsyncWrite + Unpin + Send>,
    reader: BufReader<Box<dyn AsyncRead + Unpin + Send>>,
    current_group: Option<String>,
}

/// Request for pipelined downloading
#[derive(Clone)]
pub struct SegmentRequest {
    pub message_id: String,
    pub group: String,
    pub segment_number: u32,
}

impl AsyncNntpConnection {
    /// Create a new NNTP connection with optional shared TLS connector
    ///
    /// Using a shared TLS connector enables session reuse across connections to the same server,
    /// which significantly reduces TLS handshake overhead (can save ~35% CPU on SSL operations)
    pub async fn connect(
        config: &UsenetConfig,
        tls_connector: Option<Arc<TlsConnector>>,
    ) -> Result<Self> {
        let addr = format!("{}:{}", config.server, config.port);

        // Connect with timeout
        let tcp_stream = timeout(Duration::from_secs(30), TcpStream::connect(&addr))
            .await
            .map_err(|_| NntpError::Timeout { seconds: 30 })?
            .map_err(|e| NntpError::ConnectionFailed {
                server: config.server.clone(),
                port: config.port,
                source: e,
            })?;

        // Set socket options for better performance
        tcp_stream.set_nodelay(true)?;

        // Wrap in TLS if needed
        let (reader, writer): (
            Box<dyn AsyncRead + Unpin + Send>,
            Box<dyn AsyncWrite + Unpin + Send>,
        ) = if config.ssl {
            // Use shared connector if provided, otherwise create a new one
            let connector = if let Some(shared_connector) = tls_connector {
                shared_connector
            } else {
                // Fallback: create new connector (for backwards compatibility/testing)
                let mut tls_builder = native_tls::TlsConnector::builder();
                if !config.verify_ssl_certs {
                    tls_builder.danger_accept_invalid_certs(true);
                    tls_builder.danger_accept_invalid_hostnames(true);
                }
                let native_connector = tls_builder.build()?;
                Arc::new(TlsConnector::from(native_connector))
            };

            // Perform TLS handshake
            let tls_stream = timeout(
                Duration::from_secs(30),
                connector.connect(&config.server, tcp_stream),
            )
            .await
            .map_err(|_| NntpError::Timeout { seconds: 30 })?
            .map_err(|e| NntpError::TlsError(e.to_string()))?;

            // Split TLS stream
            let (read_half, write_half) = tokio::io::split(tls_stream);
            (Box::new(read_half), Box::new(write_half))
        } else {
            // Plain TCP
            let (read_half, write_half) = tokio::io::split(tcp_stream);
            (Box::new(read_half), Box::new(write_half))
        };

        let reader = BufReader::with_capacity(256 * 1024, reader); // 256KB read buffer for pipelining

        let mut conn = Self {
            writer,
            reader,
            current_group: None,
        };

        // Initialize connection
        conn.initialize(config).await?;

        Ok(conn)
    }

    async fn initialize(&mut self, config: &UsenetConfig) -> Result<()> {
        // Read server greeting
        let response = self.read_response().await?;
        if !response.starts_with("200") && !response.starts_with("201") {
            return Err(
                NntpError::ProtocolError(format!("Server greeting failed: {}", response)).into(),
            );
        }

        // Authenticate
        self.authenticate(config).await
    }

    async fn authenticate(&mut self, config: &UsenetConfig) -> Result<()> {
        // Send username
        self.send_command(&format!("AUTHINFO USER {}", config.username))
            .await?;
        let response = self.read_response().await?;

        if response.starts_with("381") {
            // Server wants password
            self.send_command(&format!("AUTHINFO PASS {}", config.password))
                .await?;
            let response = self.read_response().await?;

            if !response.starts_with("281") {
                // Sanitize response to avoid leaking sensitive info
                let sanitized = response.split_whitespace().next().unwrap_or("Unknown");
                return Err(NntpError::AuthFailed(format!(
                    "Authentication failed ({})",
                    sanitized
                ))
                .into());
            }
        } else if !response.starts_with("281") {
            // Sanitize response to avoid leaking sensitive info
            let sanitized = response.split_whitespace().next().unwrap_or("Unknown");
            return Err(
                NntpError::AuthFailed(format!("Authentication failed ({})", sanitized)).into(),
            );
        }

        Ok(())
    }

    /// Download a segment and return the decoded data
    pub async fn download_segment(&mut self, message_id: &str, group: &str) -> Result<Bytes> {
        // Select group if different from current
        if self.current_group.as_deref() != Some(group) {
            self.send_command(&format!("GROUP {}", group)).await?;
            let response = timeout(Duration::from_secs(10), self.read_response())
                .await
                .map_err(|_| NntpError::Timeout { seconds: 10 })??;
            if !response.starts_with("211") {
                return Err(NntpError::GroupNotFound {
                    group: group.to_string(),
                }
                .into());
            }
            self.current_group = Some(group.to_string());
        }

        // Request article body
        self.send_command(&format!("BODY <{}>", message_id)).await?;
        let response = timeout(Duration::from_secs(10), self.read_response())
            .await
            .map_err(|_| NntpError::Timeout { seconds: 10 })??;
        if !response.starts_with("222") {
            return Err(NntpError::ArticleNotFound {
                message_id: message_id.to_string(),
            }
            .into());
        }

        // Read and decode the body
        let encoded_data = timeout(Duration::from_secs(30), self.read_article_body())
            .await
            .map_err(|_| NntpError::Timeout { seconds: 30 })??;

        // Simple yEnc decoding
        let decoded = self.decode_yenc_simple(&encoded_data)?;

        Ok(Bytes::from(decoded))
    }

    /// Read article body until termination
    async fn read_article_body(&mut self) -> Result<Vec<u8>> {
        use tokio::io::AsyncBufReadExt;

        let mut body = Vec::with_capacity(1024 * 1024); // Pre-allocate 1MB for larger segments
        let mut line = Vec::new();

        loop {
            line.clear();

            // Read line efficiently using BufRead
            let bytes_read = self.reader.read_until(b'\n', &mut line).await?;
            if bytes_read == 0 {
                break; // EOF
            }

            // Check for termination (single dot followed by newline)
            if line == b".\r\n" || line == b".\n" {
                break;
            }

            // Handle dot-stuffing (lines starting with .. become .)
            if line.len() >= 2 && line[0] == b'.' && line[1] == b'.' {
                line.remove(0);
            }

            // Add line to body (without CRLF, but keep newline for yenc decoder)
            if line.ends_with(b"\r\n") {
                body.extend_from_slice(&line[..line.len() - 2]);
            } else if line.ends_with(b"\n") {
                body.extend_from_slice(&line[..line.len() - 1]);
            } else {
                body.extend_from_slice(&line);
            }

            body.push(b'\n'); // Add newline back for yenc decoder
        }

        Ok(body)
    }

    /// SIMD-accelerated yEnc decoder
    ///
    /// Uses vectorized operations to decode 16-32 bytes at a time on supported platforms:
    /// - x86_64: SSE2 (always available on 64-bit x86)
    /// - aarch64: NEON (always available on 64-bit ARM)
    /// - Fallback: Optimized scalar for other platforms
    fn decode_yenc_simple(&self, data: &[u8]) -> Result<Vec<u8>> {
        // Pre-allocate based on expected output size
        let mut decoded = Vec::with_capacity(data.len());
        let mut in_data = false;

        for line in data.split(|&b| b == b'\n') {
            // Check for yEnc markers
            if line.starts_with(b"=ybegin") {
                in_data = true;
                continue;
            }
            if line.starts_with(b"=yend") {
                break;
            }
            if line.starts_with(b"=ypart") {
                continue;
            }

            if in_data && !line.is_empty() {
                Self::decode_yenc_line_simd(line, &mut decoded);
            }
        }

        decoded.shrink_to_fit();
        Ok(decoded)
    }

    /// Decode a single yEnc line using SIMD when possible
    #[inline]
    fn decode_yenc_line_simd(line: &[u8], output: &mut Vec<u8>) {
        // First, check if the line has any escape characters or CR
        // If not, we can use fast SIMD path
        let has_escapes = line.iter().any(|&b| b == b'=' || b == b'\r');

        if !has_escapes {
            // Fast path: no escapes, use SIMD for the entire line
            Self::decode_yenc_fast(line, output);
        } else {
            // Slow path: has escapes, use scalar with skip logic
            Self::decode_yenc_scalar(line, output);
        }
    }

    /// Fast SIMD path for lines without escape characters
    #[inline]
    #[cfg(target_arch = "x86_64")]
    fn decode_yenc_fast(line: &[u8], output: &mut Vec<u8>) {
        use std::arch::x86_64::*;

        let len = line.len();
        let start = output.len();
        output.reserve(len);

        // Process 16 bytes at a time with SSE2
        let mut i = 0;

        // Safety: SSE2 is always available on x86_64
        unsafe {
            let sub_vec = _mm_set1_epi8(42i8);

            while i + 16 <= len {
                let chunk = _mm_loadu_si128(line.as_ptr().add(i) as *const __m128i);
                let result = _mm_sub_epi8(chunk, sub_vec);

                output.set_len(start + i + 16);
                _mm_storeu_si128(output.as_mut_ptr().add(start + i) as *mut __m128i, result);

                i += 16;
            }
        }

        // Handle remaining bytes with scalar
        for &byte in &line[i..] {
            output.push(byte.wrapping_sub(42));
        }
    }

    /// Fast SIMD path for lines without escape characters (ARM NEON)
    #[inline]
    #[cfg(target_arch = "aarch64")]
    fn decode_yenc_fast(line: &[u8], output: &mut Vec<u8>) {
        use std::arch::aarch64::*;

        let len = line.len();
        let start = output.len();
        output.reserve(len);

        let mut i = 0;

        // Safety: NEON is always available on aarch64
        unsafe {
            let sub_vec = vdupq_n_u8(42);

            while i + 16 <= len {
                let chunk = vld1q_u8(line.as_ptr().add(i));
                let result = vsubq_u8(chunk, sub_vec);

                output.set_len(start + i + 16);
                vst1q_u8(output.as_mut_ptr().add(start + i), result);

                i += 16;
            }
        }

        // Handle remaining bytes with scalar
        for &byte in &line[i..] {
            output.push(byte.wrapping_sub(42));
        }
    }

    /// Fallback scalar path for non-SIMD platforms
    #[inline]
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    fn decode_yenc_fast(line: &[u8], output: &mut Vec<u8>) {
        output.reserve(line.len());
        for &byte in line {
            output.push(byte.wrapping_sub(42));
        }
    }

    /// Scalar decoder for lines with escape characters
    #[inline]
    fn decode_yenc_scalar(line: &[u8], output: &mut Vec<u8>) {
        let mut iter = line.iter().copied();
        while let Some(byte) = iter.next() {
            if byte == b'=' {
                // Escaped character
                if let Some(next_byte) = iter.next() {
                    output.push(next_byte.wrapping_sub(64).wrapping_sub(42));
                }
            } else if byte != b'\r' {
                // Normal character (skip carriage returns)
                output.push(byte.wrapping_sub(42));
            }
        }
    }

    async fn send_command(&mut self, command: &str) -> Result<()> {
        self.writer.write_all(command.as_bytes()).await?;
        self.writer.write_all(b"\r\n").await?;
        self.writer.flush().await?;
        Ok(())
    }

    async fn read_response(&mut self) -> Result<String> {
        let mut response = String::new();
        self.reader.read_line(&mut response).await?;

        // Remove CRLF
        if response.ends_with("\r\n") {
            response.truncate(response.len() - 2);
        } else if response.ends_with('\n') {
            response.truncate(response.len() - 1);
        }

        Ok(response)
    }

    /// Check if connection is healthy by sending a NOOP
    pub async fn is_healthy(&mut self) -> bool {
        match self.send_command("NOOP").await {
            Ok(_) => match timeout(Duration::from_secs(5), self.read_response()).await {
                Ok(Ok(response)) => response.starts_with("200"),
                _ => false,
            },
            Err(_) => false,
        }
    }

    /// Download multiple segments using pipelining for maximum throughput
    ///
    /// This sends multiple BODY commands before waiting for responses,
    /// dramatically reducing round-trip latency overhead
    pub async fn download_segments_pipelined(
        &mut self,
        requests: &[SegmentRequest],
    ) -> Result<Vec<(u32, Option<Bytes>)>> {
        if requests.is_empty() {
            return Ok(Vec::new());
        }

        // Switch to the group if needed (all requests should be from same group)
        let group = &requests[0].group;
        if self.current_group.as_deref() != Some(group) {
            self.send_command(&format!("GROUP {}", group)).await?;
            let response = timeout(Duration::from_secs(10), self.read_response())
                .await
                .map_err(|_| NntpError::Timeout { seconds: 10 })??;
            if !response.starts_with("211") {
                return Err(NntpError::GroupNotFound {
                    group: group.to_string(),
                }
                .into());
            }
            self.current_group = Some(group.to_string());
        }

        // Pipeline all BODY requests - send them all without waiting
        for req in requests {
            self.writer
                .write_all(format!("BODY <{}>\r\n", req.message_id).as_bytes())
                .await?;
        }
        self.writer.flush().await?;

        // Now read all responses in order
        let mut results = Vec::with_capacity(requests.len());

        for req in requests {
            // Read response code
            let response = match timeout(Duration::from_secs(10), self.read_response()).await {
                Ok(Ok(r)) => r,
                _ => {
                    results.push((req.segment_number, None));
                    continue;
                }
            };

            if !response.starts_with("222") {
                // Article not found or error - we still need to read the body if server sent one
                // to keep the connection in sync for remaining pipelined responses
                if response.starts_with("430") || response.starts_with("423") {
                    // 430 = no such article, 423 = no such article number
                    // These don't send a body, safe to skip
                    results.push((req.segment_number, None));
                    continue;
                } else {
                    // Unknown response, try to read body anyway to avoid desync
                    let _ = timeout(Duration::from_secs(30), self.read_article_body()).await;
                    results.push((req.segment_number, None));
                    continue;
                }
            }

            // Read and decode the body
            let encoded_data =
                match timeout(Duration::from_secs(30), self.read_article_body()).await {
                    Ok(Ok(data)) => data,
                    _ => {
                        results.push((req.segment_number, None));
                        continue;
                    }
                };

            // Decode yEnc
            match self.decode_yenc_simple(&encoded_data) {
                Ok(decoded) => {
                    results.push((req.segment_number, Some(Bytes::from(decoded))));
                }
                Err(_) => {
                    results.push((req.segment_number, None));
                }
            }
        }

        Ok(results)
    }

    /// Close the connection gracefully
    pub async fn close(&mut self) -> Result<()> {
        let _ = self.send_command("QUIT").await;
        let _ = timeout(Duration::from_secs(2), self.read_response()).await;
        // Note: OwnedWriteHalf doesn't need explicit shutdown
        Ok(())
    }
}
