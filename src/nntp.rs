use anyhow::{anyhow, Result};
use std::io::{BufRead, BufReader, Write, Read};
use std::net::TcpStream;
use std::time::Duration;

use crate::config::UsenetConfig;

pub struct NntpClient {
    stream: TcpStream,
    reader: BufReader<TcpStream>,
    config: UsenetConfig,
}

impl NntpClient {
    pub fn connect(config: UsenetConfig) -> Result<Self> {
        let addr = format!("{}:{}", config.server, config.port);
        let socket_addr = addr.parse::<std::net::SocketAddr>()
            .or_else(|_| {
                // Try to resolve hostname
                use std::net::ToSocketAddrs;
                addr.to_socket_addrs()?.next().ok_or_else(|| {
                    anyhow!("Could not resolve address: {}", addr)
                })
            })?;

        let stream = TcpStream::connect_timeout(
            &socket_addr,
            Duration::from_secs(30)
        )?;

        stream.set_read_timeout(Some(Duration::from_secs(60)))?;
        stream.set_write_timeout(Some(Duration::from_secs(30)))?;

        let reader = BufReader::new(stream.try_clone()?);

        let mut client = Self {
            stream,
            reader,
            config,
        };

        // Read initial server greeting
        let response = client.read_response()?;
        if !response.starts_with("200") && !response.starts_with("201") {
            return Err(anyhow!("Server greeting failed: {}", response));
        }

        // Authenticate
        client.authenticate()?;

        Ok(client)
    }

    fn authenticate(&mut self) -> Result<()> {
        // Send username
        self.send_command(&format!("AUTHINFO USER {}", self.config.username))?;
        let response = self.read_response()?;

        if response.starts_with("381") {
            // Server wants password
            self.send_command(&format!("AUTHINFO PASS {}", self.config.password))?;
            let response = self.read_response()?;

            if !response.starts_with("281") {
                return Err(anyhow!("Authentication failed: {}", response));
            }
        } else if !response.starts_with("281") {
            return Err(anyhow!("Authentication failed: {}", response));
        }

        Ok(())
    }

    pub fn download_segment(&mut self, message_id: &str, group: &str) -> Result<Vec<u8>> {
        // Select group
        self.send_command(&format!("GROUP {}", group))?;
        let response = self.read_response()?;
        if !response.starts_with("211") {
            return Err(anyhow!("Failed to select group {}: {}", group, response));
        }

        // Request article body
        self.send_command(&format!("BODY <{}>", message_id))?;
        let response = self.read_response()?;
        if !response.starts_with("222") {
            return Err(anyhow!("Failed to get article body: {}", response));
        }

                // Read article body until we hit the termination line
        let mut body_lines = Vec::new();
        loop {
            let mut line_bytes = Vec::new();
            let mut byte = [0u8; 1];

            // Read byte by byte until we hit a newline
            loop {
                match self.reader.read(&mut byte)? {
                    0 => return Err(anyhow!("Unexpected end of stream")),
                    _ => {
                        if byte[0] == b'\n' {
                            break;
                        }
                        if byte[0] != b'\r' {
                            line_bytes.push(byte[0]);
                        }
                    }
                }
            }

            // Check for termination (single dot)
            if line_bytes.len() == 1 && line_bytes[0] == b'.' {
                break;
            }

            // Handle dot-stuffing (lines starting with .. become .)
            if line_bytes.len() >= 2 && line_bytes[0] == b'.' && line_bytes[1] == b'.' {
                line_bytes.remove(0);
            }

            body_lines.push(line_bytes);
        }

        // Decode yEnc data
        self.decode_yenc_bytes(&body_lines)
    }

    fn decode_yenc_bytes(&self, lines: &[Vec<u8>]) -> Result<Vec<u8>> {
        let mut data = Vec::new();
        let mut in_data = false;

        for line in lines {
            // Check for yEnc headers/footers
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

            if in_data {
                // Decode yEnc line
                let decoded = self.decode_yenc_line_bytes(line)?;
                data.extend(decoded);
            }
        }

        Ok(data)
    }

    fn decode_yenc_line_bytes(&self, line: &[u8]) -> Result<Vec<u8>> {
        let mut result = Vec::new();
        let mut i = 0;

        while i < line.len() {
            if line[i] == b'=' && i + 1 < line.len() {
                // Escaped character: subtract 64 from the next byte, then subtract 42
                let escaped_byte = line[i + 1];
                let decoded = escaped_byte.wrapping_sub(64).wrapping_sub(42);
                result.push(decoded);
                i += 2;
            } else {
                // Normal character: just subtract 42
                let decoded = line[i].wrapping_sub(42);
                result.push(decoded);
                i += 1;
            }
        }

        Ok(result)
    }

    fn send_command(&mut self, command: &str) -> Result<()> {
        writeln!(self.stream, "{}", command)?;
        self.stream.flush()?;
        Ok(())
    }

    fn read_response(&mut self) -> Result<String> {
        let mut response = String::new();
        self.reader.read_line(&mut response)?;

        // Remove CRLF
        if response.ends_with("\r\n") {
            response.truncate(response.len() - 2);
        } else if response.ends_with('\n') {
            response.truncate(response.len() - 1);
        }

        Ok(response)
    }

    pub fn quit(&mut self) -> Result<()> {
        self.send_command("QUIT")?;
        let _response = self.read_response()?;
        Ok(())
    }
}
