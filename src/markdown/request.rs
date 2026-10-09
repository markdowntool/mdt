use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, ffi::OsString, fs, io::Read};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Read,
    Diff,
    Find,
    Outline,
    Replace,
    Insert,
    History,
    Forget,
}
impl Operation {
    pub fn parse(name: &str) -> Result<Self> {
        match name {
            "read" => Ok(Self::Read),
            "diff" => Ok(Self::Diff),
            "find" => Ok(Self::Find),
            "outline" => Ok(Self::Outline),
            "replace" => Ok(Self::Replace),
            "insert" => Ok(Self::Insert),
            "history" => Ok(Self::History),
            "forget" => Ok(Self::Forget),
            _ => Err(Error::new("usage", "Unknown command; use --help")),
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Diff => "diff",
            Self::Find => "find",
            Self::Outline => "outline",
            Self::Replace => "replace",
            Self::Insert => "insert",
            Self::History => "history",
            Self::Forget => "forget",
        }
    }
    pub fn is_write(self) -> bool {
        matches!(self, Self::Replace | Self::Insert | Self::Forget)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    pub operation: Operation,
    pub revision: Option<String>,
    pub since: Option<String>,
    pub nodes: Vec<String>,
    pub target: Option<String>,
    pub text: Option<String>,
    pub old: Option<String>,
    pub new: Option<String>,
    /// Exact input source; parser reads --body FILE or stdin without trimming.
    pub body: Option<String>,
    pub before: Option<String>,
    pub after: Option<String>,
    pub force: bool,
    pub dry_run: bool,
    pub context_words: Option<usize>,
    pub max_bytes: usize,
    pub json: bool,
    pub cursor: Option<String>,
}
impl Request {
    pub fn new(operation: Operation) -> Self {
        Self {
            operation,
            revision: None,
            since: None,
            nodes: vec![],
            target: None,
            text: None,
            old: None,
            new: None,
            body: None,
            before: None,
            after: None,
            force: false,
            dry_run: false,
            context_words: None,
            max_bytes: 8192,
            json: false,
            cursor: None,
        }
    }
    /// Parse arguments after the operation and source identifier. `find`'s
    /// literal is the one positional argument. Help must be handled by caller.
    pub fn parse(operation: &str, args: impl IntoIterator<Item = OsString>) -> Result<Self> {
        let mut request = Self::new(Operation::parse(operation)?);
        let mut seen = HashSet::new();
        let mut args = args.into_iter();
        let mut positional = false;
        let mut body = None;
        while let Some(arg) = args.next() {
            let arg = arg
                .into_string()
                .map_err(|_| Error::new("usage", "Arguments must be UTF-8"))?;
            if arg == "--" && !positional {
                positional = true;
                continue;
            }
            if !positional && arg.starts_with('-') {
                if arg != "--node" && !seen.insert(arg.clone()) {
                    return Err(Error::new("usage", "Repeated option"));
                }
                let mut value = || {
                    args.next()
                        .ok_or_else(|| Error::new("usage", format!("{arg} requires a value")))?
                        .into_string()
                        .map_err(|_| Error::new("usage", "Option value must be UTF-8"))
                };
                match arg.as_str() {
                    "--rev" => request.revision = Some(value()?),
                    "--since" => request.since = Some(value()?),
                    "--node" => request.nodes.push(value()?),
                    "--target" => request.target = Some(value()?),
                    "--old" => request.old = Some(value()?),
                    "--new" => request.new = Some(value()?),
                    "--body" => body = Some(value()?),
                    "--before" => request.before = Some(value()?),
                    "--after" => request.after = Some(value()?),
                    "--cursor" => request.cursor = Some(value()?),
                    "--context-words" => request.context_words = Some(number(&value()?)?),
                    "--max-bytes" => request.max_bytes = number(&value()?)?,
                    "--json" => request.json = true,
                    "--force" => request.force = true,
                    "--dry-run" => request.dry_run = true,
                    _ => return Err(Error::new("usage", format!("Unknown option {arg}"))),
                }
            } else if request.operation == Operation::Find && request.text.is_none() {
                request.text = Some(arg);
            } else {
                return Err(Error::new("usage", "Unexpected argument"));
            }
        }
        if request.cursor.is_some()
            && (seen.len() != 1 || !request.nodes.is_empty() || request.text.is_some())
        {
            return Err(Error::new(
                "usage",
                "--cursor retains all query options; supply only the cursor",
            ));
        }
        // Validate option combinations before reading a body stream.
        request.body = body.as_ref().map(|_| String::new());
        request.validate()?;
        if let Some(input) = body {
            request.body = Some(if input == "-" {
                let mut text = String::new();
                std::io::stdin()
                    .lock()
                    .read_to_string(&mut text)
                    .map_err(Error::io)?;
                text
            } else {
                fs::read_to_string(input).map_err(Error::io)?
            });
        }
        Ok(request)
    }
    pub fn validate(&self) -> Result<()> {
        use Operation::*;
        let bad = |message| Err(Error::new("usage", message));
        if self.cursor.is_some() {
            return if self.operation.is_write() {
                bad("Writes do not accept --cursor")
            } else {
                Ok(())
            };
        }
        if self.max_bytes < 256 {
            return Err(Error::new(
                "budget_too_small",
                "Response budget must be at least 256 bytes",
            ));
        }
        if self.target.is_some() && (!self.nodes.is_empty() || self.revision.is_none()) {
            return bad("--target requires --rev and cannot combine with --node");
        }
        if self.revision.is_some() && matches!(self.operation, History | Forget) {
            return bad("This command does not accept --rev");
        }
        if self.since.is_some() && self.operation != Diff {
            return bad("Only diff accepts --since");
        }
        if self.target.is_some() && !matches!(self.operation, Read | Replace) {
            return bad("Only read and replace accept --target");
        }
        if !self.nodes.is_empty() && matches!(self.operation, History | Forget | Insert) {
            return bad("This command does not accept --node");
        }
        if self.context_words.is_some() && !matches!(self.operation, Diff | Find) {
            return bad("Only diff and find accept --context-words");
        }
        if self.operation == Diff && self.since.is_none() {
            return bad("diff requires --since REV");
        }
        if self.operation == Find && self.text.as_deref().is_none_or(str::is_empty) {
            return bad("find requires nonempty literal text");
        }
        if self.text.is_some() && self.operation != Find {
            return bad("Only find accepts a literal argument");
        }
        if matches!(self.operation, Replace | Insert) && self.revision.is_none() {
            return bad("Writes require --rev REV");
        }
        if self.operation == Replace {
            if self.before.is_some() || self.after.is_some() {
                return bad("replace does not accept insertion anchors");
            }
            let literal = self.old.is_some()
                && self.new.is_some()
                && self.body.is_none()
                && self.target.is_none();
            let range = self.old.is_none()
                && self.new.is_none()
                && self.body.is_some()
                && (self.target.is_some() || !self.nodes.is_empty());
            if !literal && !range {
                return bad(
                    "Use --old TEXT --new TEXT, or select --node/--target with --body FILE|-",
                );
            }
            if self.old.as_deref() == Some("") {
                return bad("--old must be nonempty");
            }
        } else if self.old.is_some() || self.new.is_some() {
            return bad("Only replace accepts --old/--new");
        }
        if self.operation == Insert {
            if self.before.is_some() == self.after.is_some() || self.body.is_none() {
                return bad(
                    "insert requires exactly one of --before/--after TARGET and --body FILE|-",
                );
            }
        } else if self.before.is_some() || self.after.is_some() {
            return bad("Only insert accepts --before/--after");
        }
        if self.body.is_some() && !matches!(self.operation, Replace | Insert) {
            return bad("Only replace and insert accept --body");
        }
        if self.force && !matches!(self.operation, Replace | Insert) {
            return bad("Only replace and insert accept --force");
        }
        if self.dry_run && !self.operation.is_write() {
            return bad("Only writes accept --dry-run");
        }
        Ok(())
    }
}
fn number(value: &str) -> Result<usize> {
    value
        .parse()
        .map_err(|_| Error::new("usage", "Expected a nonnegative integer"))
}

pub fn help(operation: Option<&str>) -> String {
    let Some(operation) = operation else {
        return include_str!("../help.txt").into();
    };
    let syntax = match operation {
        "read" => "read FILE [--rev REV] [--node LABEL]... [--target TARGET]\nRead complete exact selected source in bounded pages. --target requires --rev.",
        "diff" => "diff FILE --since REV [--rev REV] [--node LABEL]... [--context-words N]\nCompare retained versions; --rev defaults to current saved text. Includes removals.",
        "find" => "find FILE TEXT [--rev REV] [--node LABEL]... [--context-words N]\nFind literal, case-sensitive source. Return every match with its exact range target.",
        "outline" => "outline FILE [--rev REV] [--node LABEL]...\nList heading and list-item paths and targets. D identifies the complete document.",
        "replace" => "replace FILE --rev REV [--node LABEL]... --old TEXT --new TEXT\nmdt replace FILE --rev REV (--node LABEL... | --target TARGET) --body FILE|-\nReplace one literal occurrence or entire selected range, including children.\n--force permits a changed, uniquely identified target; never ambiguous matches.\n--dry-run validates without changing source. Empty replacement deletes.\nInput is exact: no whitespace or final-newline normalization.",
        "insert" => "insert FILE --rev REV (--before TARGET | --after TARGET) --body FILE|-\nInsert exact source before/after a range including its subtree. D is the document.\n--force permits a changed but unambiguous anchor. --dry-run reserves nothing.\nSupply desired indentation and newlines; empty input is a no-op.",
        "history" => "history FILE\nList retained observations newest first. Observation dates are not edit dates.\nRead old source with read --rev; compare using diff --since A --rev B.",
        "forget" => "forget FILE [--dry-run]\nRemove retained versions and continuations for this source, never its Markdown.",
        _ => return "Unknown command; use mdt --help\n".into(),
    };
    format!("mdt {syntax}\n\n--max-bytes N caps the whole UTF-8 response (default 8192); --json is structured.\n--node uses exact labels, repeated for successive ancestry; duplicates are errors.\nRead/diff/find/outline/history: --cursor CURSOR continues pinned versions/options;\nsupply only --cursor. complete=false means more source/results remain.\nData: stdout. Errors and text notices: stderr. -h/--help: no state or file access.\nRevisions/targets are local and bounded; unavailable versions are never guessed.\nExit: 0 success; 2 usage/budget; 3 not found; 4 ambiguous; 5 conflict;\n6 revision unavailable; 9 contention; 10 storage/I/O. See man mdt.\n")
}
