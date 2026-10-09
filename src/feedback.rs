//! The only network operation in the CLI, reached by an explicit `feedback`.
use std::{
    ffi::OsStr,
    fs::File,
    io::{IsTerminal, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::{json, Value};

use crate::error::{Error, Result};

pub const BUILD: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("QM_CLI_BUILD"), ")");
const ENDPOINT: &str = "https://qm.venka.com/api/feedback";
const MAX_CHARACTERS: usize = 10_000;
const MAX_ATTACHMENT: usize = 5 * 1024 * 1024;
const MAX_RESPONSE: u64 = 8192;

pub const HELP: &str = "mdt feedback --body -|FILE [--attach FILE ...] [--json]
Send a UTF-8 message to mdt's developers. --body - reads stdin; FILE reads a file.
Requires nonempty text, at most 10,000 characters. No account or API key needed.
Optional attachments: .md, .json, .csv, .tsv; PNG, JPEG, GIF, WebP, HEIC/HEIF,
TIFF, BMP or AVIF images. Up to four regular files, at most 5 MiB each.
Only the supplied text and files, their basenames, this CLI's build identity, and
the email and LinkedIn in saved feedback contact preferences (if any) are sent, so the
makers know whose agent wrote in. The service also sees the connection's IP
address. No note, log or diagnostic is collected automatically. Get permission
before sharing private content; an agent must follow the user's feedback and
attachment permissions.
Success prints the server's feedback ID; --json returns ok, id and build.
Exit 2: invalid input; 3: missing file; 10: I/O or delivery error (JSON error.code).
A timeout, HTTP error or missing receipt leaves delivery unconfirmed: the report
may have arrived. Keep it locally and do not automatically resend. No retries.
The network deadline is 30 seconds. Other mdt commands stay local and headless.
";

struct Attachment {
    name: String,
    mime: &'static str,
    bytes: Vec<u8>,
}

fn regular_file(path: &Path, limit: usize) -> Result<Vec<u8>> {
    // Inspect before opening so a FIFO or device cannot block on open().
    let metadata = path.metadata().map_err(Error::io)?;
    if !metadata.is_file() {
        return Err(Error::new("usage", "Feedback inputs must be regular files"));
    }
    if metadata.len() > limit as u64 {
        return Err(Error::new(
            "usage",
            "Feedback input exceeds its size limit; nothing sent",
        ));
    }
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(Error::io)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(Error::io)?;
    if bytes.len() > limit {
        return Err(Error::new(
            "usage",
            "Feedback input exceeds its size limit; nothing sent",
        ));
    }
    Ok(bytes)
}

fn attachment(path: &Path) -> Result<Attachment> {
    let extension = path
        .extension()
        .and_then(OsStr::to_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    let mime = match extension.as_str() {
        "md" => "text/markdown",
        "json" => "application/json",
        "csv" => "text/csv",
        "tsv" => "text/tab-separated-values",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "heic" => "image/heic",
        "heif" => "image/heif",
        "tif" | "tiff" => "image/tiff",
        "bmp" => "image/bmp",
        "avif" => "image/avif",
        _ => {
            return Err(Error::new(
                "usage",
                "Unsupported attachment type; use mdt feedback --help. Nothing sent",
            ))
        }
    };
    let name = path
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| Error::new("usage", "Attachment filenames must be UTF-8"))?;
    if name
        .chars()
        .any(|c| c.is_control() || matches!(c, '"' | '\\'))
    {
        return Err(Error::new(
            "usage",
            "Rename the attachment to remove quotes, backslashes or control characters",
        ));
    }
    Ok(Attachment {
        name: name.into(),
        mime,
        bytes: regular_file(path, MAX_ATTACHMENT)?,
    })
}

fn description(body: &OsStr) -> Result<String> {
    let bytes = if body == "-" {
        let input = std::io::stdin();
        if input.is_terminal() {
            return Err(Error::new(
                "usage",
                "Pipe the message to --body - or use --body FILE; nothing sent",
            ));
        }
        let mut bytes = Vec::new();
        input
            .lock()
            .take((MAX_CHARACTERS * 4 + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(Error::io)?;
        bytes
    } else {
        regular_file(Path::new(body), MAX_CHARACTERS * 4)?
    };
    let text = String::from_utf8(bytes)
        .map_err(|_| Error::new("usage", "Feedback must be UTF-8 text; nothing sent"))?;
    if text.trim().is_empty() || text.contains('\0') || text.chars().count() > MAX_CHARACTERS {
        return Err(Error::new(
            "usage",
            "Feedback needs 1–10,000 characters of text, without NUL; nothing sent",
        ));
    }
    Ok(text)
}

fn endpoint() -> Result<String> {
    let endpoint = std::env::var("QM_FEEDBACK_URL").unwrap_or_else(|_| ENDPOINT.into());
    // A local test server is the only override. Never send production feedback
    // to a different public service because of a leftover environment value.
    let loopback = endpoint
        .strip_prefix("http://")
        .and_then(|s| s.strip_suffix("/api/feedback"))
        .and_then(|s| s.parse::<std::net::SocketAddr>().ok())
        .is_some_and(|addr| addr.ip().is_loopback() && addr.port() != 0);
    if endpoint != ENDPOINT && !loopback {
        return Err(Error::new("usage", "QM_FEEDBACK_URL must be the production URL or http://LOOPBACK_IP:PORT/api/feedback; nothing sent"));
    }
    Ok(endpoint)
}

fn unconfirmed(reason: impl std::fmt::Display) -> Error {
    Error::new("feedback_unconfirmed", format!("Feedback delivery unconfirmed ({reason}). The report may have arrived; keep it locally and do not automatically resend"))
}

/// The email and LinkedIn in saved feedback contact preferences, read
/// from where the app keeps them, so a report an agent sends says whose
/// agent it was. Under `QM_STATE_DIR` there is normally no such file — an isolated
/// copy stays apart from the user's — and the fields go empty.
fn remembered_contact() -> (String, String) {
    crate::state::paths()
        .map(|paths| contact_in(&paths.gates))
        .unwrap_or_default()
}

fn contact_in(dir: &Path) -> (String, String) {
    #[derive(Default, serde::Deserialize)]
    #[serde(default)]
    struct Contact {
        feedback_email: String,
        feedback_linkedin: String,
    }
    let contact: Contact = std::fs::read_to_string(dir.join("state.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    (contact.feedback_email, contact.feedback_linkedin)
}

pub fn send(body: &OsStr, paths: &[PathBuf], as_json: bool) -> Result<String> {
    let endpoint = endpoint()?;
    if paths.len() > 4 {
        return Err(Error::new(
            "usage",
            "At most four attachments; nothing sent",
        ));
    }
    let attachments = paths
        .iter()
        .map(|path| attachment(path))
        .collect::<Result<Vec<_>>>()?;
    let description = description(body)?;
    let (email, linkedin) = remembered_contact();
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let boundary = format!("qm-feedback-{}-{nonce:x}", std::process::id());
    let mut payload = Vec::new();
    for (name, value) in [
        ("description", description.as_str()),
        ("email", email.as_str()),
        ("linkedin", linkedin.as_str()),
        ("product", "qm"),
        ("app_version", BUILD),
        ("app_commit", env!("QM_CLI_COMMIT")),
    ] {
        write!(
            payload,
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
        )
        .unwrap();
    }
    for file in attachments {
        write!(payload, "--{boundary}\r\nContent-Disposition: form-data; name=\"attachments\"; filename=\"{}\"\r\nContent-Type: {}\r\n\r\n", file.name, file.mime).unwrap();
        payload.extend_from_slice(&file.bytes);
        payload.extend_from_slice(b"\r\n");
    }
    write!(payload, "--{boundary}--\r\n").unwrap();

    // Use the Mac's existing transport for this infrequent explicit operation;
    // no HTTP runtime or GUI dependency is loaded by ordinary CLI invocations.
    // Disable curlrc and retries. Do not follow redirects with private content.
    let mut child = Command::new("/usr/bin/curl")
        .args([
            "--disable",
            "--silent",
            "--connect-timeout",
            "10",
            "--max-time",
            "30",
            "--retry",
            "0",
            "--proto",
            "=http,https",
            "--write-out",
            "\n%{http_code}",
            "--header",
        ])
        .arg(format!(
            "Content-Type: multipart/form-data; boundary={boundary}"
        ))
        .args([
            "--user-agent",
            concat!("qm-cli/", env!("CARGO_PKG_VERSION")),
            "--data-binary",
            "@-",
            "--url",
            &endpoint,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| {
            Error::new(
                "feedback_not_sent",
                format!("Cannot start /usr/bin/curl: {e}; nothing sent"),
            )
        })?;
    let mut input = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || input.write_all(&payload));
    let mut response = Vec::new();
    let read = child
        .stdout
        .take()
        .unwrap()
        .take(MAX_RESPONSE + 1)
        .read_to_end(&mut response);
    if read.is_err() || response.len() > MAX_RESPONSE as usize {
        let _ = child.kill();
    }
    let status = child.wait();
    let written = writer.join();
    read.map_err(|e| unconfirmed(e))?;
    if response.len() > MAX_RESPONSE as usize {
        return Err(unconfirmed("oversized response"));
    }
    let status = status.map_err(|e| unconfirmed(e))?;
    if !status.success() || !matches!(written, Ok(Ok(()))) {
        return Err(unconfirmed(format!(
            "transport exit {}",
            status.code().unwrap_or(-1)
        )));
    }
    let response = std::str::from_utf8(&response).map_err(|_| unconfirmed("invalid response"))?;
    let (body, status) = response
        .rsplit_once('\n')
        .ok_or_else(|| unconfirmed("missing HTTP status"))?;
    if status != "200" && status != "201" {
        return Err(unconfirmed(format!("HTTP {status}")));
    }
    let receipt: Value = serde_json::from_str(body).map_err(|_| unconfirmed("invalid receipt"))?;
    let id = receipt
        .get("id")
        .and_then(Value::as_u64)
        .filter(|id| *id > 0);
    let Some(id) = id.filter(|_| receipt.get("ok") == Some(&Value::Bool(true))) else {
        return Err(unconfirmed("missing feedback ID"));
    };
    Ok(if as_json {
        format!("{}\n", json!({"ok":true, "id":id, "build":BUILD}))
    } else {
        format!("Feedback sent: {id}\nBuild: {BUILD}\n")
    })
}
