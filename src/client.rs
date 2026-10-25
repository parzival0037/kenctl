//! Dependency-free HTTP client used by the CLI subcommands that talk to a
//! running `kenctl run` daemon.

use anyhow::{anyhow, Context, Result};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

pub struct Response {
    pub status: u16,
    pub body: String,
}

/// Issue a request against `127.0.0.1:port`.
///
/// `Connection: close` keeps this honest with hyper's keep-alive: we read to
/// EOF instead of parsing chunked framing.
pub fn request(method: &str, port: u16, path: &str, body: Option<&str>) -> Result<Response> {
    let addr = format!("127.0.0.1:{port}");
    let mut stream =
        TcpStream::connect(&addr).with_context(|| format!("cannot reach daemon at {addr}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;

    let body = body.unwrap_or("");
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\
         Accept: application/json\r\n"
    );
    if !body.is_empty() {
        req.push_str("Content-Type: application/json\r\n");
        req.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    req.push_str("\r\n");
    req.push_str(body);

    stream.write_all(req.as_bytes())?;
    stream.flush()?;

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw)?;
    let text = String::from_utf8_lossy(&raw).to_string();

    let (head, payload) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| anyhow!("malformed HTTP response"))?;
    let status = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| anyhow!("missing status line"))?;

    // hyper may use chunked encoding when we ask for connection close; strip
    // chunk framing so callers always see plain JSON.
    let body = if head.to_ascii_lowercase().contains("transfer-encoding: chunked") {
        dechunk(payload)
    } else {
        payload.to_string()
    };

    Ok(Response { status, body })
}

fn dechunk(input: &str) -> String {
    let mut out = String::new();
    let mut rest = input;
    loop {
        let Some((size_line, tail)) = rest.split_once("\r\n") else {
            break;
        };
        let size_str = size_line.split(';').next().unwrap_or("").trim();
        let Ok(size) = usize::from_str_radix(size_str, 16) else {
            out.push_str(rest);
            break;
        };
        if size == 0 {
            break;
        }
        if tail.len() < size {
            out.push_str(&tail[..tail.len().min(size)]);
            break;
        }
        out.push_str(&tail[..size]);
        rest = tail.get(size..).unwrap_or("").trim_start_matches("\r\n");
    }
    out
}

/// GET a JSON endpoint, failing on non-2xx with the server's message.
pub fn get_json(port: u16, path: &str) -> Result<String> {
    let res = request("GET", port, path, None)?;
    if !(200..300).contains(&res.status) {
        return Err(anyhow!("HTTP {}: {}", res.status, res.body.trim()));
    }
    Ok(res.body)
}

/// POST/PUT a JSON body, failing on non-2xx with the server's message.
pub fn send_json(port: u16, method: &str, path: &str, body: &str) -> Result<String> {
    let res = request(method, port, path, Some(body))?;
    if !(200..300).contains(&res.status) {
        return Err(anyhow!("HTTP {}: {}", res.status, res.body.trim()));
    }
    Ok(res.body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dechunk_handles_single_and_multiple_frames() {
        assert_eq!(dechunk("5\r\nhello\r\n0\r\n\r\n"), "hello");
        assert_eq!(dechunk("3\r\nfoo\r\n3\r\nbar\r\n0\r\n\r\n"), "foobar");
        assert_eq!(dechunk(""), "");
    }
}
