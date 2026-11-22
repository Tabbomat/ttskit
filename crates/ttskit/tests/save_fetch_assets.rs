//! `ttskit save fetch-assets` end to end against a throwaway server on
//! 127.0.0.1; nothing here touches the internet.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use sha2::{Digest, Sha256};

const PNG_A: &[u8] = b"\x89PNG\r\n\x1a\nimage a";
const PNG_B: &[u8] = b"\x89PNG\r\n\x1a\nimage b, a bit longer";

fn big() -> Vec<u8> {
    vec![b'x'; 5000]
}

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
    let path = request_line.split_whitespace().nth(1).unwrap_or("");
    let (head, body): (&str, Vec<u8>) = match path {
        "/a.png" => ("200 OK\r\nContent-Type: image/png", PNG_A.to_vec()),
        "/b.png" => ("200 OK\r\nContent-Type: image/png", PNG_B.to_vec()),
        "/moved.png" => ("302 Found\r\nLocation: /a.png", Vec::new()),
        "/page.png" => (
            "200 OK\r\nContent-Type: text/html",
            b"<!DOCTYPE html><p>gone".to_vec(),
        ),
        "/big.bin" | "/stream.bin" => ("200 OK\r\nContent-Type: application/octet-stream", big()),
        _ => (
            "404 Not Found\r\nContent-Type: text/plain",
            b"nope".to_vec(),
        ),
    };
    // /stream.bin has no Content-Length: the body ends when the connection closes.
    let length = if path == "/stream.bin" {
        String::new()
    } else {
        format!("Content-Length: {}\r\n", body.len())
    };
    let response = format!("HTTP/1.1 {head}\r\n{length}Connection: close\r\n\r\n");
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.write_all(&body);
    // Close gracefully: a plain drop can reset the connection and cut the body short.
    let _ = stream.flush();
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(2)));
    let mut sink = [0u8; 256];
    while std::io::Read::read(&mut stream, &mut sink).is_ok_and(|n| n > 0) {}
}

fn save_with(dir: &Path, urls: &[String]) -> PathBuf {
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

fn run(save: &Path, out: &Path, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ttskit"))
        .args(["save", "fetch-assets", save.to_str().unwrap(), "--out"])
        .arg(out)
        .args(["--retries", "0"])
        .args(extra)
        .output()
        .unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn json(o: &Output) -> serde_json::Value {
    serde_json::from_slice(&o.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&o.stdout)))
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn index(out: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(out.join("index.json")).unwrap()).unwrap()
}

fn file_names(out: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(out.join("files"))
        .map(|rd| {
            rd.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

#[test]
fn fresh_run_then_resume() {
    let port = serve();
    let base = format!("http://127.0.0.1:{port}");
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("backup");
    let save = save_with(
        dir.path(),
        &[format!("{base}/a.png"), format!("{base}/b.png")],
    );

    let first = run(&save, &out, &[]);
    assert_eq!(first.status.code(), Some(0), "{}", stdout(&first));
    let text = stdout(&first);
    assert!(
        text.contains("2 URLs: 0 already present, 2 to download, 0 skipped"),
        "{text}"
    );
    assert!(text.contains("downloaded 2, failed 0"), "{text}");
    // The plan comes before the result.
    assert!(text.find("to download").unwrap() < text.find("downloaded 2").unwrap());

    let name_a = format!("{}.png", sha(PNG_A));
    let name_b = format!("{}.png", sha(PNG_B));
    let mut expected = vec![name_a.clone(), name_b.clone()];
    expected.sort();
    assert_eq!(file_names(&out), expected);
    assert_eq!(
        std::fs::read(out.join("files").join(&name_a)).unwrap(),
        PNG_A
    );
    assert!(!out.join(".partial").exists());

    let idx = index(&out);
    assert_eq!(idx["version"], 1);
    let files = idx["files"].as_array().unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(files[0]["url"], format!("{base}/a.png"));
    assert_eq!(files[0]["file"], format!("files/{name_a}"));
    assert_eq!(files[0]["size"], PNG_A.len());
    assert_eq!(files[0]["sha256"], sha(PNG_A));
    assert_eq!(files[0]["type"], "png");
    assert_eq!(files[1]["url"], format!("{base}/b.png"));

    let index_before = std::fs::read(out.join("index.json")).unwrap();
    let second = run(&save, &out, &[]);
    assert_eq!(second.status.code(), Some(0));
    let text = stdout(&second);
    assert!(
        text.contains("2 URLs: 2 already present, 0 to download, 0 skipped"),
        "{text}"
    );
    assert!(text.contains("downloaded 0, failed 0"), "{text}");
    assert_eq!(std::fs::read(out.join("index.json")).unwrap(), index_before);
}

#[test]
fn dry_run_creates_nothing() {
    let port = serve();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("not").join("there");
    let save = save_with(dir.path(), &[format!("http://127.0.0.1:{port}/a.png")]);
    let output = run(&save, &out, &["--dry-run"]);
    assert_eq!(output.status.code(), Some(0));
    let text = stdout(&output);
    assert!(
        text.contains("1 URLs: 0 already present, 1 to download, 0 skipped"),
        "{text}"
    );
    assert!(!text.contains("downloaded"), "{text}");
    assert!(!dir.path().join("not").exists());

    let output = run(&save, &out, &["--dry-run", "--json"]);
    let report = json(&output);
    assert_eq!(report["plan"]["to_download"], 1);
    assert_eq!(report["downloaded"], serde_json::json!([]));
    assert_eq!(report["failed"], serde_json::json!([]));
    assert!(!dir.path().join("not").exists());
}

#[test]
fn failures_exit_2_but_the_rest_is_downloaded() {
    let port = serve();
    let base = format!("http://127.0.0.1:{port}");
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("backup");
    let save = save_with(
        dir.path(),
        &[
            format!("{base}/a.png"),
            format!("{base}/missing.png"),
            format!("{base}/page.png"),
        ],
    );
    let output = run(&save, &out, &[]);
    assert_eq!(output.status.code(), Some(2));
    let text = stdout(&output);
    assert!(
        text.contains(&format!("failed: {base}/missing.png: ")),
        "{text}"
    );
    assert!(text.contains("404"), "{text}");
    assert!(
        text.contains(&format!("failed: {base}/page.png: ")),
        "{text}"
    );
    assert!(text.contains("html"), "{}", text.to_lowercase());
    assert!(text.contains("nickname: Card 1, guid: g1"), "{text}");
    assert!(text.contains("downloaded 1, failed 2"), "{text}");
    let idx = index(&out);
    assert_eq!(idx["files"].as_array().unwrap().len(), 1);
    assert_eq!(file_names(&out).len(), 1);
}

#[test]
fn max_size_rejects_big_files_with_and_without_content_length() {
    let port = serve();
    let base = format!("http://127.0.0.1:{port}");
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("backup");
    let save = save_with(
        dir.path(),
        &[
            format!("{base}/a.png"),
            format!("{base}/big.bin"),
            format!("{base}/stream.bin"),
        ],
    );
    let output = run(&save, &out, &["--max-size", "1k", "--json"]);
    assert_eq!(output.status.code(), Some(2));
    let report = json(&output);
    assert_eq!(report["downloaded"].as_array().unwrap().len(), 1);
    let failed = report["failed"].as_array().unwrap();
    assert_eq!(failed.len(), 2);
    for f in failed {
        assert!(f["reason"].as_str().unwrap().contains("size limit"), "{f}");
    }
    assert_eq!(file_names(&out).len(), 1);

    // Big enough limit: both are accepted.
    let output = run(&save, &out, &["--max-size", "8k"]);
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
    assert_eq!(file_names(&out).len(), 2, "same content is stored once");
    assert_eq!(index(&out)["files"].as_array().unwrap().len(), 3);
}

#[test]
fn bad_max_size_is_a_usage_error() {
    let dir = tempfile::tempdir().unwrap();
    let save = save_with(dir.path(), &[]);
    let output = run(&save, &dir.path().join("o"), &["--max-size", "12x"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid size"));
    assert!(!dir.path().join("o").exists());
}

#[test]
fn redirects_are_followed() {
    let port = serve();
    let base = format!("http://127.0.0.1:{port}");
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("backup");
    let save = save_with(dir.path(), &[format!("{base}/moved.png")]);
    let output = run(&save, &out, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
    let idx = index(&out);
    assert_eq!(idx["files"][0]["url"], format!("{base}/moved.png"));
    assert_eq!(idx["files"][0]["sha256"], sha(PNG_A));
}

#[test]
fn verify_redownloads_a_corrupted_file() {
    let port = serve();
    let base = format!("http://127.0.0.1:{port}");
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("backup");
    let save = save_with(dir.path(), &[format!("{base}/a.png")]);
    assert_eq!(run(&save, &out, &[]).status.code(), Some(0));
    let file = out.join("files").join(format!("{}.png", sha(PNG_A)));
    std::fs::write(&file, vec![b'!'; PNG_A.len()]).unwrap();

    // Same size: only --verify notices.
    let plain = stdout(&run(&save, &out, &[]));
    assert!(plain.contains("1 already present"), "{plain}");
    assert_eq!(std::fs::read(&file).unwrap()[0], b'!');

    let output = run(&save, &out, &["--verify"]);
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
    let text = stdout(&output);
    assert!(text.contains("0 verified"), "{text}");
    assert!(
        text.contains("1 invalid (will be downloaded again)"),
        "{text}"
    );
    assert!(text.contains("downloaded 1, failed 0"), "{text}");
    assert_eq!(std::fs::read(&file).unwrap(), PNG_A);

    let again = stdout(&run(&save, &out, &["--verify"]));
    assert!(again.contains("1 verified"), "{again}");
    assert!(again.contains("1 already present"), "{again}");
}

#[test]
fn json_output_shape() {
    let port = serve();
    let base = format!("http://127.0.0.1:{port}");
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("backup");
    let save = save_with(
        dir.path(),
        &[
            format!("{base}/b.png"),
            format!("{base}/a.png"),
            format!("{base}/missing.png"),
            "http://127.0.0.1:9/unreachable.png".to_owned(),
        ],
    );
    let output = run(&save, &out, &["--json"]);
    assert_eq!(output.status.code(), Some(2));
    let report = json(&output);
    let mut keys: Vec<&str> = report
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["downloaded", "failed", "index_error", "out", "plan", "save"]
    );
    let plan = &report["plan"];
    assert_eq!(plan["total"], 4);
    assert_eq!(plan["present"], 0);
    assert_eq!(plan["to_download"], 4);
    assert_eq!(plan["skipped"], 0);
    assert_eq!(plan["invalid"], 0);
    assert_eq!(plan["verified"], 0);
    let downloaded = report["downloaded"].as_array().unwrap();
    assert_eq!(downloaded[0]["url"], format!("{base}/a.png"));
    assert_eq!(downloaded[1]["url"], format!("{base}/b.png"));
    assert_eq!(downloaded[0]["file"], format!("files/{}.png", sha(PNG_A)));
    assert_eq!(downloaded[0]["size"], PNG_A.len());
    assert_eq!(downloaded[0]["sha256"], sha(PNG_A));
    assert_eq!(downloaded[0]["type"], "png");
    let failed = report["failed"].as_array().unwrap();
    assert_eq!(failed.len(), 2);
    let urls: Vec<&str> = failed.iter().map(|f| f["url"].as_str().unwrap()).collect();
    let mut sorted = urls.clone();
    sorted.sort_unstable();
    assert_eq!(urls, sorted);
    let failed = [
        failed
            .iter()
            .find(|f| f["url"].as_str().unwrap().ends_with("/unreachable.png"))
            .unwrap(),
        failed
            .iter()
            .find(|f| f["url"].as_str().unwrap().ends_with("/missing.png"))
            .unwrap(),
    ];
    assert_eq!(failed[1]["occurrences"][0]["nickname"], "Card 2");
    assert_eq!(failed[1]["occurrences"][0]["guid"], "g2");
    assert!(failed[1]["occurrences"][0]["path"].is_string());
    assert!(failed[1]["reason"].is_string());
    assert!(report["index_error"].is_null());
}

#[test]
fn excluded_hosts_are_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("backup");
    let save = save_with(dir.path(), &["http://127.0.0.1:9/a.png".to_owned()]);
    let output = run(&save, &out, &["--skip-host", "127.0.0.1"]);
    assert_eq!(output.status.code(), Some(0));
    let text = stdout(&output);
    assert!(
        text.contains("1 URLs: 0 already present, 0 to download, 1 skipped"),
        "{text}"
    );
    assert!(!out.exists());
}

#[test]
fn invalid_index_exits_1_and_is_left_alone() {
    let port = serve();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("backup");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("index.json"), "this is not json").unwrap();
    let save = save_with(dir.path(), &[format!("http://127.0.0.1:{port}/a.png")]);
    for extra in [&[][..], &["--dry-run"][..]] {
        let output = run(&save, &out, extra);
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("index.json"));
    }
    assert_eq!(
        std::fs::read_to_string(out.join("index.json")).unwrap(),
        "this is not json"
    );
    assert!(!out.join("files").exists());
}

#[test]
fn unreadable_save_exits_1() {
    let dir = tempfile::tempdir().unwrap();
    let output = run(&dir.path().join("missing.json"), &dir.path().join("o"), &[]);
    assert_eq!(output.status.code(), Some(1));
    assert!(!dir.path().join("o").exists());
}
