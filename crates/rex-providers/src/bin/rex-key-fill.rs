//! rex-key-fill: one-shot secure key fill server.
//!
//! Security contract:
//! - Binds 127.0.0.1 only. Meant to sit behind an authenticated private
//!   HTTPS preview (e.g. a GitHub Codespaces private forwarded port).
//! - GET  /health  -> {"ok":true}; no state, no token.
//! - GET  /fill    -> one HTML form embedding a one-time CSRF token.
//!   The key field is a password input (masked), autocomplete off,
//!   Cache-Control: no-store.
//! - POST /fill    -> form fields token + key. On token match, writes the
//!   FileSecretStore (dir 0700, file 0600) for the requested provider,
//!   replies with a non-secret JSON ack, then the process EXITS. The
//!   route is gone after one fill.
//! - No request logging, no body logging, no access log. Nothing prints
//!   the token, the key, or any body bytes, ever.
//! - A failed token check returns 403 and keeps the token alive so a
//!   mistyped paste can be retried from a fresh GET /fill.

use rex_providers::{FileSecretStore, SecretStore};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Mutex;

fn gen_token() -> String {
    let mut buf = [0u8; 16];
    let mut f = std::fs::File::open("/dev/urandom").expect("urandom");
    use std::io::Read;
    f.read_exact(&mut buf).expect("urandom read");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hex = &s[i + 1..i + 3];
                if let Ok(v) = u8::from_str_radix(hex, 16) {
                    out.push(v);
                    i += 2;
                } else {
                    out.push(bytes[i]);
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn respond(stream: &mut std::net::TcpStream, status: &str, ctype: &str, body: &str) {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
    let _ = stream.flush();
}

fn form_page(token: &str, provider: &str) -> String {
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><title>rex key fill</title></head>
<body style="font-family:sans-serif;max-width:32em;margin:4em auto">
<h2>REX one-shot key fill</h2>
<p>Provider: <b>{provider}</b>. This form works once; the server exits after a successful fill.</p>
<form method="POST" action="/fill" autocomplete="off">
<input type="hidden" name="token" value="{token}">
<label>Key <input type="password" name="key" autocomplete="off" size="48"></label>
<button type="submit">Store key</button>
</form></body></html>"#
    )
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut port: u16 = 8788;
    let mut provider = "gemini".to_string();
    let mut config_dir = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
        .join(".config")
        .join("rex-harness");
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--port" if i + 1 < args.len() => {
                port = args[i + 1].parse().expect("port");
                i += 2;
            }
            "--provider" if i + 1 < args.len() => {
                provider = args[i + 1].clone();
                i += 2;
            }
            "--config-dir" if i + 1 < args.len() => {
                config_dir = PathBuf::from(&args[i + 1]);
                i += 2;
            }
            other => {
                eprintln!("rex-key-fill: unknown arg {other}");
                std::process::exit(2);
            }
        }
    }

    let token = gen_token();
    let used = Mutex::new(false);
    let listener = TcpListener::bind(("127.0.0.1", port)).expect("bind");
    eprintln!("rex-key-fill listening on 127.0.0.1:{port} (one-shot; no logging)");

    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let mut reader = BufReader::new(stream.try_clone().expect("clone"));
        let mut request_line = String::new();
        if reader.read_line(&mut request_line).is_err() {
            continue;
        }
        let mut parts = request_line.trim().split_whitespace();
        let method = parts.next().unwrap_or("").to_string();
        let path = parts.next().unwrap_or("").to_string();
        let mut content_length = 0usize;
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    let t = line.trim();
                    if t.is_empty() {
                        break;
                    }
                    if let Some((n, v)) = t.split_once(':') {
                        if n.eq_ignore_ascii_case("content-length") {
                            content_length = v.trim().parse().unwrap_or(0);
                        }
                    }
                }
                Err(_) => break,
            }
        }
        let mut body = vec![0u8; content_length.min(64 * 1024)];
        use std::io::Read;
        if reader.read_exact(&mut body).is_err() {
            continue;
        }
        let body = String::from_utf8_lossy(&body).to_string();

        if method == "GET" && path == "/health" {
            respond(&mut stream, "200 OK", "application/json", "{\"ok\":true}");
        } else if method == "GET" && path == "/fill" {
            if *used.lock().unwrap() {
                respond(&mut stream, "410 Gone", "text/plain", "fill already completed");
            } else {
                respond(&mut stream, "200 OK", "text/html", &form_page(&token, &provider));
            }
        } else if method == "POST" && path == "/fill" {
            let mut got_token = String::new();
            let mut got_key = String::new();
            for pair in body.split('&') {
                if let Some((k, v)) = pair.split_once('=') {
                    match k {
                        "token" => got_token = url_decode(v),
                        "key" => got_key = url_decode(v),
                        _ => {}
                    }
                }
            }
            let mut used_guard = used.lock().unwrap();
            if *used_guard || got_token != token || got_key.is_empty() {
                drop(used_guard);
                respond(&mut stream, "403 Forbidden", "application/json", "{\"ok\":false}");
                continue;
            }
            let store = match FileSecretStore::new(config_dir.clone()) {
                Ok(s) => s,
                Err(_) => {
                    respond(&mut stream, "500 Internal Server Error", "application/json", "{\"ok\":false,\"stage\":\"store\"}");
                    continue;
                }
            };
            match store.set_key(&provider, &got_key) {
                Ok(()) => {
                    // Scrub the in-memory copy before exit (defense in depth;
                    // the process exits immediately after the ack anyway).
                    unsafe {
                        let bytes = got_key.as_bytes_mut();
                        bytes.fill(0);
                    }
                    *used_guard = true;
                    drop(used_guard);
                    respond(
                        &mut stream,
                        "200 OK",
                        "application/json",
                        "{\"ok\":true,\"stored\":true,\"perms\":\"0600\",\"server\":\"exiting\"}",
                    );
                    eprintln!("rex-key-fill: fill complete; exiting (route closed)");
                    std::process::exit(0);
                }
                Err(_) => {
                    respond(&mut stream, "500 Internal Server Error", "application/json", "{\"ok\":false,\"stage\":\"write\"}");
                }
            }
        } else {
            respond(&mut stream, "404 Not Found", "text/plain", "not found");
        }
    }
}
