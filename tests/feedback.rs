//! Actual helper processes against a loopback HTTP receiver, never production.
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    process::{Command, Output, Stdio},
    thread,
    time::Duration,
};

use serde_json::Value;

fn cli(endpoint: &str, args: &[&str], body: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_mdt"))
        .env("QM_FEEDBACK_URL", endpoint)
        .env_remove("HOME")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let _ = child.stdin.take().unwrap().write_all(body);
    child.wait_with_output().unwrap()
}

fn server(status: u16, body: Vec<u8>) -> (String, thread::JoinHandle<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/api/feedback", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0; 8192];
        loop {
            let read = stream.read(&mut buffer).unwrap();
            assert_ne!(read, 0, "request truncated");
            request.extend_from_slice(&buffer[..read]);
            if let Some(end) = request.windows(4).position(|s| s == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..end]).to_lowercase();
                let length = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: "))
                    .unwrap()
                    .parse::<usize>()
                    .unwrap();
                if request.len() >= end + 4 + length {
                    break;
                }
            }
        }
        // status 0 simulates a lost reply after the server received the report.
        if status != 0 {
            let reply = format!("HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let _ = stream.write_all(reply.as_bytes());
            let _ = stream.write_all(&body);
        }
        request
    });
    (endpoint, handle)
}

#[test]
fn sends_only_explicit_text_files_and_build_then_prints_confirmed_receipt() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("example.md");
    fs::write(&file, "Synthetic attachment — no personal content.\n").unwrap();
    let image = temp.path().join("image.png");
    let image_bytes = b"\x89PNG\r\n\x1a\n\0\xffimage-fixture";
    fs::write(&image, image_bytes).unwrap();
    let (url, server) = server(200, br#"{"ok":true,"id":42}"#.to_vec());
    let output = cli(
        &url,
        &[
            "feedback",
            "--body",
            "-",
            "--attach",
            file.to_str().unwrap(),
            "--attach",
            image.to_str().unwrap(),
            "--json",
        ],
        "Agent observation: synthetic test 🐕.\n".as_bytes(),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let receipt: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(receipt["id"], 42);
    assert_eq!(receipt["ok"], true);
    let request = server.join().unwrap();
    let text = String::from_utf8_lossy(&request);
    assert!(text.starts_with("POST /api/feedback HTTP/1.1\r\n"));
    assert!(text.contains("Agent observation: synthetic test 🐕."));
    assert!(text.contains("filename=\"example.md\"\r\nContent-Type: text/markdown"));
    assert!(text.contains("filename=\"image.png\"\r\nContent-Type: image/png"));
    assert!(request
        .windows(image_bytes.len())
        .any(|part| part == image_bytes));
    assert!(text.contains(receipt["build"].as_str().unwrap()));
    assert!(
        !text.contains(temp.path().to_str().unwrap()),
        "absolute path leaked"
    );
    let version = cli("invalid", &["--version"], b"");
    assert!(version.status.success());
    assert!(String::from_utf8_lossy(&version.stdout).contains(receipt["build"].as_str().unwrap()));
}

#[test]
fn body_file_and_plain_receipt_work() {
    let temp = tempfile::tempdir().unwrap();
    let body = temp.path().join("report.md");
    fs::write(&body, "Synthetic report from a file.").unwrap();
    let (url, server) = server(201, br#"{"ok":true,"id":73}"#.to_vec());
    let output = cli(&url, &["feedback", "--body", body.to_str().unwrap()], b"");
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("Feedback sent: 73\n"));
    assert!(
        String::from_utf8_lossy(&server.join().unwrap()).contains("Synthetic report from a file.")
    );
}

#[test]
fn failed_or_ambiguous_receipts_are_never_success_or_retried() {
    for (status, body) in [
        (500, br#"{"ok":true,"id":1}"#.to_vec()),
        (302, vec![]),
        (200, b"not JSON".to_vec()),
        (200, br#"{"ok":true}"#.to_vec()),
        (200, br#"{"ok":false,"id":1}"#.to_vec()),
        (200, br#"{"ok":true,"id":0}"#.to_vec()),
        (200, vec![b'x'; 9000]),
        (0, vec![]),
    ] {
        let (url, server) = server(status, body);
        let output = cli(
            &url,
            &["feedback", "--body", "-", "--json"],
            b"Synthetic report",
        );
        server.join().unwrap();
        assert_eq!(output.status.code(), Some(10));
        assert!(output.stdout.is_empty());
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["code"], "feedback_unconfirmed");
        assert!(error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("do not automatically resend"));
    }
}

#[test]
fn invalid_input_is_refused_before_network_and_help_needs_no_home_or_cache() {
    let endpoint = "http://127.0.0.1:1/api/feedback";
    for args in [
        vec!["feedback"],
        vec!["feedback", "--body", "-", "--cache-dir", "/unused"],
        vec!["feedback", "--body", "-", "--body", "-"],
        vec!["feedback", "--body", "-", "--unknown"],
        vec!["doc", "--attach", "image.png"],
    ] {
        let output = cli(endpoint, &args, b"report");
        assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
    }
    for body in [
        vec![],
        b"  \n".to_vec(),
        vec![0],
        vec![0xff],
        vec![b'x'; 10001],
    ] {
        let output = cli(endpoint, &["feedback", "--body", "-"], &body);
        assert_eq!(output.status.code(), Some(2));
    }
    let temp = tempfile::tempdir().unwrap();
    for (name, size) in [
        ("run.sh", 1),
        ("fake.svg", 1),
        ("too-big.md", 5 * 1024 * 1024 + 1),
        ("injection\r\n.png", 1),
    ] {
        let path = temp.path().join(name);
        fs::write(&path, vec![b'x'; size]).unwrap();
        let output = cli(
            endpoint,
            &[
                "feedback",
                "--body",
                "-",
                "--attach",
                path.to_str().unwrap(),
            ],
            b"report",
        );
        assert_eq!(output.status.code(), Some(2), "{name}: {output:?}");
    }
    let directory = temp.path().join("directory.md");
    fs::create_dir(&directory).unwrap();
    let output = cli(
        endpoint,
        &["feedback", "--body", directory.to_str().unwrap()],
        b"",
    );
    assert_eq!(output.status.code(), Some(2));
    let args = [
        "feedback", "--body", "-", "--attach", "1.md", "--attach", "2.md", "--attach", "3.md",
        "--attach", "4.md", "--attach", "5.md",
    ];
    assert_eq!(cli(endpoint, &args, b"report").status.code(), Some(2));
    assert_eq!(
        cli(
            "https://another.example/api/feedback",
            &["feedback", "--body", "-"],
            b"report"
        )
        .status
        .code(),
        Some(2)
    );
    let help = cli("invalid", &["feedback", "--help"], b"");
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("--attach"));
}
