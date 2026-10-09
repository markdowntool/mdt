//! Argument routing for the public Markdown tool. The operation parser and
//! engine are also used by Emm issue descriptions.
use crate::{
    error::{Error, Result},
    feedback,
    markdown::{self, FileSource, Request},
};
use std::{ffi::OsString, path::PathBuf};

const VALUES: &[&str] = &[
    "--rev",
    "--since",
    "--node",
    "--target",
    "--old",
    "--new",
    "--body",
    "--before",
    "--after",
    "--cursor",
    "--context-words",
    "--max-bytes",
    "--attach",
];
/// Recognize flags without confusing option values for flags.
pub fn has_flag(args: &[OsString], flag: &str) -> bool {
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--" {
            break;
        }
        if arg == flag || (flag == "--help" && arg == "-h") {
            return true;
        }
        if arg.to_str().is_some_and(|s| VALUES.contains(&s)) {
            args.next();
        }
    }
    false
}
/// Continuations carry their output mode even when their retained query expired.
pub fn json_errors(args: &[OsString]) -> bool {
    if has_flag(args, "--json") {
        return true;
    }
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--" {
            break;
        }
        if arg == "--cursor" {
            return args
                .next()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.starts_with("Cj"));
        }
        if arg.to_str().is_some_and(|s| VALUES.contains(&s)) {
            args.next();
        }
    }
    false
}
pub fn error_output(args: &[OsString], error: &Error) -> String {
    let mut limit = 8192;
    let mut values = args.iter();
    while let Some(arg) = values.next() {
        if arg == "--" {
            break;
        }
        if arg == "--max-bytes" {
            limit = values
                .next()
                .and_then(|s| s.to_str())
                .and_then(|s| s.parse().ok())
                .unwrap_or(8192);
        } else if arg.to_str().is_some_and(|s| VALUES.contains(&s)) {
            values.next();
        }
    }
    limit = limit.clamp(256, 8192);
    let mut error = error.clone();
    loop {
        let output = if json_errors(args) {
            format!("{}\n", serde_json::json!({"error":error}))
        } else {
            format!("{error}\n")
        };
        if output.len() <= limit {
            return output;
        }
        if error.message.is_empty() {
            error.current_revision = None;
            error.bytes = None;
            continue;
        }
        let mut at = error
            .message
            .len()
            .saturating_sub((output.len() - limit).max(4));
        while !error.message.is_char_boundary(at) {
            at -= 1;
        }
        error.message.truncate(at);
    }
}
pub fn run(args: Vec<OsString>) -> Result<String> {
    if args.is_empty() {
        return Ok(markdown::help(None));
    }
    let command = args[0]
        .to_str()
        .ok_or_else(|| Error::new("usage", "Command must be UTF-8"))?;
    if command == "--version" && args.len() == 1 {
        return Ok(format!("mdt {}\n", feedback::BUILD));
    }
    if command == "--help" || command == "-h" {
        return Ok(markdown::help(None));
    }
    if has_flag(&args[1..], "--help") {
        return Ok(if command == "feedback" {
            feedback::HELP.into()
        } else {
            markdown::help(Some(command))
        });
    }
    if command == "feedback" {
        return send_feedback(&args[1..]);
    }
    markdown::Operation::parse(command)?;
    let file = args
        .get(1)
        .filter(|s| !s.to_str().is_some_and(|s| s.starts_with('-')))
        .ok_or_else(|| {
            Error::new(
                "usage",
                "Expected FILE after the command; use COMMAND --help",
            )
        })?;
    let request = Request::parse(command, args[2..].iter().cloned())?;
    let mut source = FileSource::new(&PathBuf::from(file))?;
    let response = markdown::execute(&mut source, &markdown::state_database()?, request)?;
    for notice in response.notices {
        eprintln!("{notice}");
    }
    Ok(response.stdout)
}
fn send_feedback(args: &[OsString]) -> Result<String> {
    let mut body = None;
    let mut attachments = vec![];
    let mut json = false;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--json" && !json {
            json = true;
        } else if arg == "--body" && body.is_none() {
            body = Some(
                args.next()
                    .ok_or_else(|| Error::new("usage", "--body requires FILE|-"))?,
            );
        } else if arg == "--attach" {
            attachments
                .push(PathBuf::from(args.next().ok_or_else(|| {
                    Error::new("usage", "--attach requires FILE")
                })?));
        } else {
            return Err(Error::new(
                "usage",
                "Unexpected feedback argument; use feedback --help",
            ));
        }
    }
    feedback::send(
        body.ok_or_else(|| Error::new("usage", "feedback requires --body FILE|-"))?,
        &attachments,
        json,
    )
}
