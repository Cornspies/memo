//! Debug builds only: `memo-desktop --serve <port> <data dir> <web dist>` serves the web UI and the
//! IPC calls over local HTTP (`POST /ipc/<call>`), with secrets in a file instead of the keyring.
//! The end-to-end test (`web/e2e/desktop.ts`) drives the desktop frontend and this Rust side in
//! headless Firefox this way; only the Tauri window itself is left out.

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::calls::{self, Ctx, Secrets};

const CSP: &str = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self'; img-src 'self' blob:; object-src 'none'; base-uri 'none'; form-action 'none'";

pub fn main(args: Vec<String>) {
    let [port, dir, dist] = &args[..] else {
        eprintln!("usage: memo-desktop --serve <port> <data dir> <web dist>");
        std::process::exit(2);
    };
    let dir = PathBuf::from(dir);
    let ctx = Ctx::new(dir.join("data"), dir.join("downloads"), Secrets::File(dir.join("secrets.json")))
        .unwrap_or_else(|e| panic!("{e}"));
    let dist = Arc::new(PathBuf::from(dist));
    let listener = TcpListener::bind(("127.0.0.1", port.parse::<u16>().expect("port"))).expect("bind");
    eprintln!("serving on http://127.0.0.1:{port}");
    for stream in listener.incoming().flatten() {
        let (ctx, dist) = (Arc::clone(&ctx), Arc::clone(&dist));
        std::thread::spawn(move || {
            let _ = handle(stream, &ctx, &dist);
        });
    }
}

fn handle(stream: TcpStream, ctx: &Ctx, dist: &Path) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or("/").to_owned());
    let mut length = 0;
    let mut call = None;
    loop {
        let mut header = String::new();
        reader.read_line(&mut header)?;
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        let (name, value) = header.split_once(':').unwrap_or((header, ""));
        match name.to_ascii_lowercase().as_str() {
            "content-length" => length = value.trim().parse().unwrap_or(0),
            "memo-call" => call = Some(value.trim().to_owned()),
            _ => {}
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    let mut out = stream;
    if method == "POST" && path == "/ipc" {
        let Some(call) = call else { return respond(&mut out, 400, "text/plain", b"no memo-call header") };
        return match calls::dispatch(ctx, &call, &body) {
            Ok(bytes) => respond(&mut out, 200, "application/octet-stream", &bytes),
            Err(e) => respond(&mut out, 500, "text/plain", e.as_bytes()),
        };
    }
    let path = path.split('?').next().unwrap_or("/").trim_start_matches('/');
    let file = if path.is_empty() || path.contains("..") { dist.join("index.html") } else { dist.join(path) };
    match std::fs::read(&file) {
        Ok(bytes) => respond(&mut out, 200, content_type(&file), &bytes),
        Err(_) => respond(&mut out, 404, "text/plain", b"not found"),
    }
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript",
        Some("css") => "text/css",
        Some("wasm") => "application/wasm",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    }
}

fn respond(out: &mut TcpStream, status: u16, content_type: &str, body: &[u8]) -> std::io::Result<()> {
    write!(
        out,
        "HTTP/1.1 {status} X\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nContent-Security-Policy: {CSP}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    out.write_all(body)
}
