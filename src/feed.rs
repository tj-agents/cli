//! Each session's feed: what other sessions post for it with `tj message`. Nothing reaches the session's
//! conversation until it runs `tj feed` itself.

use std::io::{self, Write};
use std::num::ParseIntError;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::Value;

use crate::{host::Host, paths};

const POLL: Duration = Duration::from_secs(2);

#[derive(clap::Args)]
pub(crate) struct Args {
    /// Wait until a message arrives instead of returning when there is none
    #[arg(long)]
    wait: bool,
    /// Stop waiting after this many seconds
    #[arg(long, requires = "wait", default_value_t = 12 * 60 * 60)]
    timeout: u64,
}

/// The folder every session's feed lives in.
pub(crate) fn folder() -> Result<PathBuf> {
    Ok(paths::cache_dir()?.join("feed"))
}

/// One session's feed: `<host>-<id>.jsonl` holds its messages, `<host>-<id>.read` the byte offset read so far.
pub(crate) struct Feed {
    messages: PathBuf,
    read: PathBuf,
}

#[derive(Debug, thiserror::Error)]
#[error("session handle {0:?} must be host:id of letters, digits, '-' or '_'")]
pub(crate) struct HandleError(String);

#[derive(Debug, thiserror::Error)]
pub(crate) enum PostError {
    #[error("could not create the feed folder")]
    Folder(#[source] io::Error),
    #[error("could not append to {}", .0.display())]
    Append(PathBuf, #[source] io::Error),
}

#[derive(Debug, thiserror::Error)]
enum ReadError {
    #[error("could not read {}", .0.display())]
    File(PathBuf, #[source] io::Error),
    #[error("{} holds no byte offset", .0.display())]
    Offset(PathBuf, #[source] ParseIntError),
    #[error("the feed holds a line that is not JSON")]
    Line(#[source] serde_json::Error),
}

#[derive(Debug, thiserror::Error)]
#[error("could not record what was read")]
struct MarkError(#[source] io::Error);

/// The complete messages past the read offset, and the offset after them.
struct Unread {
    messages: Vec<Value>,
    end: u64,
}

impl Feed {
    pub(crate) fn new(folder: &Path, handle: &str) -> Result<Self, HandleError> {
        let valid =
            |part: &str| !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || "-_".contains(c));
        let (host, id) = handle
            .split_once(':')
            .filter(|(host, id)| valid(host) && valid(id))
            .ok_or_else(|| HandleError(handle.to_owned()))?;
        Ok(Self { messages: folder.join(format!("{host}-{id}.jsonl")), read: folder.join(format!("{host}-{id}.read")) })
    }

    /// Appends one message in a single write, so concurrent senders never interleave a line.
    pub(crate) fn post(&self, message: &Value) -> Result<(), PostError> {
        if let Some(folder) = self.messages.parent() {
            std::fs::create_dir_all(folder).map_err(PostError::Folder)?;
        }
        std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.messages)
            .and_then(|mut file| file.write_all(format!("{message}\n").as_bytes()))
            .map_err(|error| PostError::Append(self.messages.clone(), error))
    }

    /// A line still being written has no newline yet, so it waits for the next read.
    fn unread(&self) -> Result<Unread, ReadError> {
        let start = match std::fs::read_to_string(&self.read) {
            Ok(text) => text.trim().parse::<u64>().map_err(|error| ReadError::Offset(self.read.clone(), error))?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
            Err(error) => return Err(ReadError::File(self.read.clone(), error)),
        };
        let bytes = match std::fs::read(&self.messages) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(ReadError::File(self.messages.clone(), error)),
        };
        let start = usize::try_from(start).ok().filter(|&start| start <= bytes.len()).unwrap_or(0);
        let end = bytes[start..].iter().rposition(|&byte| byte == b'\n').map_or(start, |last| start + last + 1);
        let messages = String::from_utf8_lossy(&bytes[start..end])
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(serde_json::from_str)
            .collect::<Result<_, _>>()
            .map_err(ReadError::Line)?;
        Ok(Unread { messages, end: u64::try_from(end).expect("a file length fits in u64") })
    }

    fn mark(&self, offset: u64) -> Result<(), MarkError> {
        let staging = self.read.with_extension(format!("read.{}.tmp", std::process::id()));
        std::fs::write(&staging, offset.to_string())
            .and_then(|()| std::fs::rename(&staging, &self.read))
            .map_err(MarkError)
    }
}

fn show(message: &Value) {
    let text = message["text"].as_str().unwrap_or_default();
    let at = message["at"].as_str().unwrap_or("?");
    match message["from"].as_str() {
        Some(from) => {
            let name = message["name"].as_str().unwrap_or(from);
            println!("[{at}] {name} ({from}): {text}\n  reply: tj message {from} \"<text>\"");
        }
        None => println!("[{at}] a terminal: {text}"),
    }
}

pub(crate) fn run(args: &Args) -> Result<ExitCode> {
    let me = Host::current()
        .and_then(Host::handle)
        .context("tj feed reads the calling session's own feed; run it from a Claude or Codex session")?;
    let feed = Feed::new(&folder()?, &me)?;
    let deadline = Instant::now() + Duration::from_secs(args.timeout);
    loop {
        let unread = feed.unread()?;
        if !unread.messages.is_empty() {
            unread.messages.iter().for_each(show);
            feed.mark(unread.end)?;
            return Ok(ExitCode::SUCCESS);
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if !args.wait {
            println!("no unread messages");
            return Ok(ExitCode::SUCCESS);
        }
        if left.is_zero() {
            println!("no message arrived within {} seconds", args.timeout);
            return Ok(ExitCode::SUCCESS);
        }
        std::thread::sleep(POLL.min(left));
    }
}
