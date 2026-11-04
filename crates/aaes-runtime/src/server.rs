//! Resident HTTP hook server (ADR-021): Claude Code `type: "http"` hooks POST here instead of
//! spawning a process per tool call (measured on Windows: +362 ms → +71 ms per call).
//!
//! Endpoints (127.0.0.1 only):
//! - `GET  /aaes/v1/health`   → `{"ok":true,"version":…}` (no auth; reveals nothing)
//! - `POST /aaes/v1/hook`     → hook response JSON (bearer token required)
//! - `POST /aaes/v1/shutdown` → stops the server (bearer token required)
//!
//! Synchronous std networking, a thread per connection, HTTP/1.1 keep-alive.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use crate::{Runtime, hook, paths, store};

const MAX_BODY: usize = 64 * 1024 * 1024;

/// Read the hook token, creating a random one on first use.
pub fn token(home: &Path) -> std::io::Result<String> {
    let p = home.join("hook-token");
    if let Ok(t) = std::fs::read_to_string(&p) {
        let t = t.trim().to_owned();
        if t.len() >= 32 {
            return Ok(t);
        }
    }
    let t = format!("{}{}", &store::new_handle()[2..], &store::new_handle()[2..]);
    paths::write_private(&p, t.as_bytes())?;
    Ok(t)
}

struct Request {
    method: String,
    path: String,
    auth: Option<String>,
    close: bool,
    body: Vec<u8>,
}

fn read_request(r: &mut BufReader<TcpStream>) -> Option<Request> {
    let mut line = String::new();
    if r.read_line(&mut line).ok()? == 0 {
        return None;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_owned();
    let path = parts.next()?.to_owned();
    let http10 = parts.next().is_some_and(|v| v == "HTTP/1.0");
    let (mut len, mut auth, mut close) = (0usize, None, http10);
    loop {
        line.clear();
        if r.read_line(&mut line).ok()? == 0 {
            return None;
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
            match k.as_str() {
                "content-length" => len = v.parse().ok()?,
                "authorization" => auth = Some(v.to_owned()),
                "connection" => close = v.eq_ignore_ascii_case("close"),
                _ => {}
            }
        }
    }
    if len > MAX_BODY {
        return None;
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).ok()?;
    Some(Request {
        method,
        path,
        auth,
        close,
        body,
    })
}

fn respond(s: &mut TcpStream, status: &str, body: &str, close: bool) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: {}\r\n\r\n",
        body.len(),
        if close { "close" } else { "keep-alive" }
    );
    s.write_all(head.as_bytes())?;
    s.write_all(body.as_bytes())?;
    s.flush()
}

fn handle_conn(stream: TcpStream, rt: &Runtime, token: &str) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(120)));
    let _ = stream.set_nodelay(true);
    let Ok(mut w) = stream.try_clone() else {
        return;
    };
    let mut r = BufReader::new(stream);
    let expected = format!("Bearer {token}");
    while let Some(req) = read_request(&mut r) {
        let authed = req.auth.as_deref() == Some(expected.as_str());
        let (status, body, stop) = match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/aaes/v1/health") => (
                "200 OK",
                json!({ "ok": true, "version": env!("CARGO_PKG_VERSION") }).to_string(),
                false,
            ),
            ("POST", "/aaes/v1/hook") if authed => {
                let input: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
                ("200 OK", hook::handle(rt, &input).to_string(), false)
            }
            ("POST", "/aaes/v1/shutdown") if authed => ("200 OK", "{}".to_owned(), true),
            ("POST", _) => ("401 Unauthorized", "{}".to_owned(), false),
            _ => ("404 Not Found", "{}".to_owned(), false),
        };
        let sent = respond(&mut w, status, &body, req.close || stop).is_ok();
        if stop {
            std::process::exit(0);
        }
        if !sent || req.close {
            break;
        }
    }
}

/// Run the server until shut down. Fails if the port is taken (e.g. already running).
pub fn serve(rt: Arc<Runtime>, port: u16) -> std::io::Result<()> {
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port)))?;
    let token = Arc::new(token(&rt.home)?);
    let retention = Duration::from_secs(rt.cfg.retention_hours * 3600);
    {
        let home = rt.home.clone();
        std::thread::spawn(move || {
            loop {
                store::sweep(&home, retention);
                std::thread::sleep(Duration::from_secs(3600));
            }
        });
    }
    for stream in listener.incoming().flatten() {
        let (rt, token) = (rt.clone(), token.clone());
        std::thread::spawn(move || handle_conn(stream, &rt, &token));
    }
    Ok(())
}

/// Is a healthy AAES server listening on `port`?
pub fn healthy(port: u16, timeout: Duration) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut s) = TcpStream::connect_timeout(&addr, timeout) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(timeout));
    if s.write_all(b"GET /aaes/v1/health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .is_err()
    {
        return false;
    }
    let mut buf = String::new();
    let _ = s.read_to_string(&mut buf);
    buf.starts_with("HTTP/1.1 200") && buf.contains("\"ok\":true")
}

/// Ask a running server to stop. Returns true if it acknowledged.
pub fn shutdown(port: u16, token: &str) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_millis(500)) else {
        return false;
    };
    let req = format!(
        "POST /aaes/v1/shutdown HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    if s.write_all(req.as_bytes()).is_err() {
        return false;
    }
    let mut buf = String::new();
    let _ = s.read_to_string(&mut buf);
    buf.starts_with("HTTP/1.1 200")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn post(
        port: u16,
        path: &str,
        auth: Option<&str>,
        body: &str,
        keep: &mut Option<TcpStream>,
    ) -> String {
        let s = keep.get_or_insert_with(|| TcpStream::connect(("127.0.0.1", port)).unwrap());
        let auth = auth
            .map(|t| format!("Authorization: Bearer {t}\r\n"))
            .unwrap_or_default();
        let req = format!(
            "POST {path} HTTP/1.1\r\nHost: x\r\n{auth}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        s.write_all(req.as_bytes()).unwrap();
        let mut r = BufReader::new(s.try_clone().unwrap());
        let mut status = String::new();
        r.read_line(&mut status).unwrap();
        let mut len = 0;
        loop {
            let mut l = String::new();
            r.read_line(&mut l).unwrap();
            if l == "\r\n" {
                break;
            }
            if let Some(v) = l.to_ascii_lowercase().strip_prefix("content-length:") {
                len = v.trim().parse().unwrap();
            }
        }
        let mut b = vec![0; len];
        r.read_exact(&mut b).unwrap();
        format!("{} {}", status.trim(), String::from_utf8(b).unwrap())
    }

    #[test]
    fn health_auth_keepalive_and_hook() {
        let home = std::env::temp_dir().join(format!("aaes-srv-{}", std::process::id()));
        let rt = Arc::new(Runtime::new(home.clone()));
        let port = {
            let l = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            l.local_addr().unwrap().port()
        };
        let rt2 = rt.clone();
        std::thread::spawn(move || serve(rt2, port));
        let mut ok = false;
        for _ in 0..50 {
            if healthy(port, Duration::from_millis(200)) {
                ok = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(ok, "server healthy");
        let tok = token(&home).unwrap();
        let mut conn = None;
        assert!(post(port, "/aaes/v1/hook", None, "{}", &mut conn).starts_with("HTTP/1.1 401"));
        let ev = r#"{"hook_event_name":"PostToolUse","session_id":"k","tool_name":"Read","tool_input":{"file_path":"a.ts"},"tool_response":{"type":"text","file":{"content":"x"}}}"#;
        // two requests on the same connection (keep-alive)
        assert_eq!(
            post(port, "/aaes/v1/hook", Some(&tok), ev, &mut conn),
            "HTTP/1.1 200 OK {}"
        );
        assert_eq!(
            post(port, "/aaes/v1/hook", Some(&tok), ev, &mut conn),
            "HTTP/1.1 200 OK {}"
        );
        let _ = std::fs::remove_dir_all(&home);
    }
}
