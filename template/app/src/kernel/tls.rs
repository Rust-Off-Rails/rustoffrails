//! Plain HTTP on the TLS port (a bare `localhost:PORT` in the address bar, an `http://` bookmark)
//! gets a 307 to `https://` instead of TLS bytes (`ERR_INVALID_HTTP_RESPONSE`). An inner acceptor
//! for axum-server's `RustlsAcceptor`: it peeks at the first byte; `0x16` (a TLS handshake) goes on
//! to rustls, anything else is answered and closed. 307, never 308: a cached permanent redirect
//! would break `APP_TLS=off` on the same port later. Spec: .offrails/guides/deployment.md#direct-tls-optional
//!
//! Only `serve`'s direct-TLS branch uses it (development with a trusted certificate); production
//! serves plain HTTP behind its TLS proxies and never reaches this code.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::time::Duration;

use axum_server::accept::Accept;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

/// How long a connection may stay silent before its first byte (rustls' handshake timeout starts
/// only after this acceptor hands the stream on).
pub const SNIFF_TIMEOUT: Duration = Duration::from_secs(10);
/// The largest plain-HTTP request head read before answering.
pub const MAX_HEAD: usize = 8 * 1024;

#[derive(Clone, Copy, Debug, Default)]
pub struct HttpToHttps;

impl<S: Send + 'static> Accept<TcpStream, S> for HttpToHttps {
    type Stream = TcpStream;
    type Service = S;
    type Future = Pin<Box<dyn Future<Output = io::Result<(TcpStream, S)>> + Send>>;

    fn accept(&self, stream: TcpStream, service: S) -> Self::Future {
        Box::pin(async move {
            let mut first = [0u8; 1];
            let n = timeout(SNIFF_TIMEOUT, stream.peek(&mut first))
                .await
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::TimedOut, "no bytes before timeout")
                })??;
            if n == 1 && first[0] == 0x16 {
                return Ok((stream, service));
            }
            let _ = timeout(SNIFF_TIMEOUT, redirect_to_https(stream)).await;
            Err(io::Error::other("plain HTTP on the HTTPS port: redirected"))
        })
    }
}

/// Reads the request head (up to `MAX_HEAD`), writes the 307 (or a 400 when the head has no usable
/// Host or path), and shuts the connection down.
async fn redirect_to_https(mut stream: TcpStream) -> io::Result<()> {
    let mut head = Vec::new();
    let mut buf = [0u8; 1024];
    while head.len() < MAX_HEAD && !head.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        head.extend_from_slice(&buf[..n]);
    }
    let response = match redirect_target(&String::from_utf8_lossy(&head)) {
        Some(location) => format!(
            "HTTP/1.1 307 Temporary Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        ),
        None => {
            "HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned()
        }
    };
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}

/// The `https://` URL for a plain-HTTP request head: `https://<Host><path>`. `None` without a
/// Host, with a Host holding anything but host/port characters, or with a path that doesn't start
/// with exactly one `/` (`//evil.example` would be a protocol-relative open redirect).
pub fn redirect_target(head: &str) -> Option<String> {
    let mut lines = head.split("\r\n");
    let mut request = lines.next()?.split(' ');
    let (_method, path) = (request.next()?, request.next()?);
    if !path.starts_with('/') || path.starts_with("//") || path.contains(['\r', '\n', ' ']) {
        return None;
    }
    let host = lines.take_while(|l| !l.is_empty()).find_map(|l| {
        let (name, value) = l.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case("host")
            .then(|| value.trim())
    })?;
    let host_ok = !host.is_empty()
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']'));
    host_ok.then(|| format!("https://{host}{path}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[test]
    fn keeps_host_and_path() {
        assert_eq!(
            redirect_target("GET /books?x=1 HTTP/1.1\r\nHost: localhost:3311\r\n\r\n").as_deref(),
            Some("https://localhost:3311/books?x=1")
        );
        assert_eq!(
            redirect_target("GET / HTTP/1.1\r\nhost: [::1]:3000\r\nAccept: */*\r\n\r\n").as_deref(),
            Some("https://[::1]:3000/")
        );
    }

    #[test]
    fn refuses_open_redirects_odd_hosts_and_missing_host() {
        assert_eq!(
            redirect_target("GET //evil.example/ HTTP/1.1\r\nHost: a\r\n\r\n"),
            None
        );
        assert_eq!(
            redirect_target("GET http://x/ HTTP/1.1\r\nHost: a\r\n\r\n"),
            None
        );
        assert_eq!(
            redirect_target("GET / HTTP/1.1\r\nHost: evil.example/x\r\n\r\n"),
            None
        );
        assert_eq!(redirect_target("GET / HTTP/1.1\r\nHost: a@b\r\n\r\n"), None);
        assert_eq!(
            redirect_target("GET / HTTP/1.1\r\nAccept: */*\r\n\r\n"),
            None
        );
        assert_eq!(redirect_target(""), None);
    }

    async fn accept_one(listener: TcpListener) -> io::Result<(TcpStream, ())> {
        let (stream, _) = listener.accept().await?;
        HttpToHttps.accept(stream, ()).await
    }

    #[tokio::test]
    async fn answers_plain_http_with_a_307() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(accept_one(listener));
        let mut client = TcpStream::connect(addr).await.unwrap();
        client
            .write_all(b"GET /docs?q=1 HTTP/1.1\r\nHost: localhost:3000\r\n\r\n")
            .await
            .unwrap();
        let mut out = String::new();
        client.read_to_string(&mut out).await.unwrap();
        assert!(out.starts_with("HTTP/1.1 307 "), "{out}");
        assert!(
            out.contains("Location: https://localhost:3000/docs?q=1\r\n"),
            "{out}"
        );
        assert!(
            server.await.unwrap().is_err(),
            "the connection is not handed to rustls"
        );
    }

    #[tokio::test]
    async fn passes_a_tls_handshake_through() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(accept_one(listener));
        let mut client = TcpStream::connect(addr).await.unwrap();
        client.write_all(&[0x16, 0x03, 0x01]).await.unwrap();
        let (mut stream, ()) = server.await.unwrap().unwrap();
        let mut first = [0u8; 3];
        stream.read_exact(&mut first).await.unwrap();
        assert_eq!(first, [0x16, 0x03, 0x01], "nothing consumed before rustls");
    }
}
