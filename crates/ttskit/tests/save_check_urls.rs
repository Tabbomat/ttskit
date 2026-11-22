//! `ttskit save check-urls` end to end against a throwaway server on
//! 127.0.0.1; nothing here touches the internet.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Output};

/// Serves a few fixed routes until the process ends.
fn serve() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || handle(stream));
        }
    });
    port
}

fn handle(mut stream: TcpStream) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    reader.read_line(&mut request_line).unwrap();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        if line.trim().is_empty() {
            break;
        }
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("");
    let (head, body): (&str, &[u8]) = match path {
        "/ok.png" => ("200 OK\r\nContent-Type: image/png", b"\x89PNG\r\n\x1a\n"),
        "/moved.png" => ("302 Found\r\nLocation: /ok.png", b""),
        "/page.png" => (
            "200 OK\r\nContent-Type: text/html",
            b"<!DOCTYPE html><p>gone",
        ),
        _ => ("404 Not Found\r\nContent-Type: text/plain", b"nope"),
    };
    let body = if method == "HEAD" { &[][..] } else { body };
    let response = format!(
        "HTTP/1.1 {head}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.write_all(body);
}

fn save_with(dir: &Path, urls: &[String]) -> std::path::PathBuf {
    let objects: Vec<_> = urls
        .iter()
        .enumerate()
        .map(|(i, u)| {
            serde_json::json!({
                "GUID": format!("g{i}"),
                "Nickname": format!("Card {i}"),
                "CustomImage": {"ImageURL": u},
            })
        })
        .collect();
    let path = dir.join("save.json");
    let save = serde_json::json!({"SaveName": "t", "ObjectStates": objects});
    std::fs::write(&path, serde_json::to_vec_pretty(&save).unwrap()).unwrap();
    path
}

fn run(save: &Path, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ttskit"))
        .args(["save", "check-urls", save.to_str().unwrap()])
        .args(extra)
        .output()
        .unwrap()
}

fn json(o: &Output) -> serde_json::Value {
    serde_json::from_slice(&o.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&o.stdout)))
}

#[test]
fn dead_links_exit_2_and_name_the_object() {
    let port = serve();
    let base = format!("http://127.0.0.1:{port}");
    let dir = tempfile::tempdir().unwrap();
    let save = save_with(
        dir.path(),
        &[
            format!("{base}/ok.png"),
            format!("{base}/moved.png"),
            format!("{base}/missing.png"),
        ],
    );
    let output = run(&save, &["--json", "--retries", "0"]);
    assert_eq!(output.status.code(), Some(2));
    let report = json(&output);
    assert_eq!(report["summary"]["total"], 3);
    assert_eq!(report["summary"]["ok"], 1);
    assert_eq!(report["summary"]["redirect"], 1);
    assert_eq!(report["summary"]["dead"], 1);
    let urls = report["urls"].as_array().unwrap();
    // Sorted by URL: missing, moved, ok.
    assert_eq!(urls[0]["result"], "dead");
    assert_eq!(urls[0]["status"], 404);
    assert_eq!(urls[0]["occurrences"][0]["nickname"], "Card 2");
    assert_eq!(urls[0]["occurrences"][0]["guid"], "g2");
    assert_eq!(urls[1]["result"], "redirect");
    assert_eq!(urls[1]["target"], format!("{base}/ok.png"));
    assert_eq!(urls[2]["result"], "ok");

    let text = run(&save, &["--retries", "0"]);
    assert_eq!(text.status.code(), Some(2));
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(
        text.contains(&format!("dead (HTTP 404): {base}/missing.png")),
        "{text}"
    );
    assert!(text.contains("nickname: Card 2, guid: g2"), "{text}");
    assert!(text.contains("3 URLs: 1 ok, 1 redirected, 1 dead, 0 unreachable, 0 skipped"));
}

#[test]
fn all_ok_exits_0() {
    let port = serve();
    let dir = tempfile::tempdir().unwrap();
    let save = save_with(dir.path(), &[format!("http://127.0.0.1:{port}/ok.png")]);
    let output = run(&save, &["--verify-content"]);
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn verify_content_flags_html_pages() {
    let port = serve();
    let dir = tempfile::tempdir().unwrap();
    let save = save_with(dir.path(), &[format!("http://127.0.0.1:{port}/page.png")]);
    assert_eq!(run(&save, &[]).status.code(), Some(0));
    let output = run(&save, &["--verify-content", "--json"]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        json(&output)["urls"][0]["detail"],
        "content type is text/html"
    );
}

#[test]
fn closed_port_is_unreachable() {
    let port = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let dir = tempfile::tempdir().unwrap();
    let save = save_with(dir.path(), &[format!("http://127.0.0.1:{port}/a.png")]);
    let output = run(&save, &["--json", "--retries", "0", "--timeout", "5"]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(json(&output)["urls"][0]["result"], "unreachable");
}

#[test]
fn excluded_hosts_are_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let save = save_with(dir.path(), &["http://127.0.0.1:9/a.png".to_owned()]);
    let output = run(&save, &["--skip-host", "127.0.0.1", "--json"]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(json(&output)["urls"][0]["result"], "skipped");
}

#[test]
fn unreadable_save_exits_1() {
    let dir = tempfile::tempdir().unwrap();
    let output = run(&dir.path().join("missing.json"), &[]);
    assert_eq!(output.status.code(), Some(1));
}
