use bytes::Bytes;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, AsyncRead, AsyncWrite, BufReader};
use tokio::net::TcpStream;
use tokio::time::{timeout, Duration};
use tokio_native_tls::TlsConnector;
use native_tls::TlsConnector as NativeTlsConnector;

use crate::config::UsenetConfig;
use crate::error::{DlNzbError, NntpError};

type Result<T> = std::result::Result<T, DlNzbError>;

/// Async NNTP connection that can be pooled
pub struct AsyncNntpConnection {
    writer: Box<dyn AsyncWrite + Unpin + Send>,
    reader: BufReader<Box<dyn AsyncRead + Unpin + Send>>,
    current_group: Option<String>,
}

impl AsyncNntpConnection {
    /// Create a new NNTP connection
    pub async fn connect(config: &UsenetConfig) -> Result<Self> {
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
        let (reader, writer): (Box<dyn AsyncRead + Unpin + Send>, Box<dyn AsyncWrite + Unpin + Send>) = if config.ssl {
            // Create TLS connector
            let mut tls_builder = NativeTlsConnector::builder();
            if !config.verify_ssl_certs {
                tls_builder.danger_accept_invalid_certs(true);
                tls_builder.danger_accept_invalid_hostnames(true);
            }
            let native_connector = tls_builder.build()?;
            let connector = TlsConnector::from(native_connector);

            // Perform TLS handshake
            let tls_stream = timeout(
                Duration::from_secs(30),
                connector.connect(&config.server, tcp_stream)
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

        let reader = BufReader::with_capacity(64 * 1024, reader);

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
            return Err(NntpError::ProtocolError(format!(
                "Server greeting failed: {}",
                response
            ))
            .into());
        }

        // Authenticate
        self.authenticate(config).await
    }

    async fn authenticate(&mut self, config: &UsenetConfig) -> Result<()> {
        // Send username
        self.send_command(&format!("AUTHINFO USER {}", config.username)).await?;
        let response = self.read_response().await?;

        if response.starts_with("381") {
            // Server wants password
            self.send_command(&format!("AUTHINFO PASS {}", config.password)).await?;
            let response = self.read_response().await?;

            if !response.starts_with("281") {
                return Err(NntpError::AuthFailed(response).into());
            }
        } else if !response.starts_with("281") {
            return Err(NntpError::AuthFailed(response).into());
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

        // Read and decode the body with timeout
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

        let mut body = Vec::with_capacity(512 * 1024); // Pre-allocate 512KB
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

    /// Optimized yEnc decoder with pre-allocation and efficient iteration
    fn decode_yenc_simple(&self, data: &[u8]) -> Result<Vec<u8>> {
        // Pre-allocate based on expected output size (roughly same as input)
        let mut decoded = Vec::with_capacity(data.len());
        let mut in_data = false;

        // Use split for efficient line iteration
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
                // Decode the line using iterator for better performance
                let mut iter = line.iter().copied();
                while let Some(byte) = iter.next() {
                    if byte == b'=' {
                        // Escaped character
                        if let Some(next_byte) = iter.next() {
                            decoded.push(next_byte.wrapping_sub(64).wrapping_sub(42));
                        }
                    } else if byte != b'\r' {
                        // Normal character (skip carriage returns)
                        decoded.push(byte.wrapping_sub(42));
                    }
                }
            }
        }

        // Shrink to actual size if we over-allocated
        decoded.shrink_to_fit();
        Ok(decoded)
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

    /// Close the connection gracefully
    pub async fn close(&mut self) -> Result<()> {
        let _ = self.send_command("QUIT").await;
        let _ = timeout(Duration::from_secs(2), self.read_response()).await;
        // Note: OwnedWriteHalf doesn't need explicit shutdown
        Ok(())
    }
}

