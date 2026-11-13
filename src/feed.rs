//! A Codex session's feed: what other sessions post for it with `tj message`, since Codex has no native
//! messaging. Nothing reaches the session's conversation until it runs `tj feed` itself.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::num::ParseIntError;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::{host::Host, paths};

const POLL: Duration = Duration::from_secs(2);

/// Exit code when `--wait` gave up with no message, so a background waiter can tell it from a reply.
const TIMED_OUT: u8 = 1;

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
}

#[derive(Debug, thiserror::Error)]
#[error("could not record what was read")]
struct MarkError(#[source] io::Error);

/// The complete messages past the read offset, the lines that were not JSON, and the offset after them.
struct Unread {
    messages: Vec<Value>,
    unreadable: usize,
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

    /// Appends one message in a single write, so concurrent senders never interleave a line. On Unix the
    /// folder and feed are readable only by you.
    pub(crate) fn post(&self, message: &Value) -> Result<(), PostError> {
        if let Some(folder) = self.messages.parent() {
            let mut builder = std::fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
            builder.create(folder).map_err(PostError::Folder)?;
        }
        let mut options = OpenOptions::new();
        options.append(true).create(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        options
            .open(&self.messages)
            .and_then(|mut file| file.write_all(format!("{message}\n").as_bytes()))
            .map_err(|error| PostError::Append(self.messages.clone(), error))
    }

    fn offset(&self) -> Result<u64, ReadError> {
        match std::fs::read_to_string(&self.read) {
            Ok(text) => text.trim().parse().map_err(|error| ReadError::Offset(self.read.clone(), error)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
            Err(error) => Err(ReadError::File(self.read.clone(), error)),
        }
    }

    /// Reads only past the offset. A line still being written has no newline yet, so it waits for the next
    /// read; a line that is not JSON, such as one torn by a killed sender, is counted and passed over.
    fn unread(&self) -> Result<Unread, ReadError> {
        let failed = |error| ReadError::File(self.messages.clone(), error);
        let mut file = match File::open(&self.messages) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Unread { messages: Vec::new(), unreadable: 0, end: 0 });
            }
            Err(error) => return Err(failed(error)),
        };
        let length = file.metadata().map_err(failed)?.len();
        let start = Some(self.offset()?).filter(|&start| start <= length).unwrap_or(0);
        let mut bytes = Vec::new();
        file.seek(SeekFrom::Start(start)).and_then(|_| file.read_to_end(&mut bytes)).map_err(failed)?;
        let complete = bytes.iter().rposition(|&byte| byte == b'\n').map_or(0, |last| last + 1);
        let (mut messages, mut unreadable) = (Vec::new(), 0);
        for line in String::from_utf8_lossy(&bytes[..complete]).lines().filter(|line| !line.trim().is_empty()) {
            match serde_json::from_str(line) {
                Ok(message) => messages.push(message),
                Err(_) => unreadable += 1,
            }
        }
        let end = start + u64::try_from(complete).expect("a read length fits in u64");
        Ok(Unread { messages, unreadable, end })
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
    let host = Host::current().context("tj feed reads the calling Codex session's own feed; run it from Codex")?;
    if host == Host::Claude {
        bail!("a Claude session receives tj message in its own inbox, so it has no feed; tj feed is for Codex");
    }
    let me = host.handle().context("tj feed needs CODEX_THREAD_ID to find this session's feed")?;
    let feed = Feed::new(&folder()?, &me)?;
    let deadline = Instant::now().checked_add(Duration::from_secs(args.timeout));
    loop {
        let unread = feed.unread()?;
        if unread.unreadable > 0 {
            println!("passed over {} line(s) in the feed that were not JSON", unread.unreadable);
        }
        unread.messages.iter().for_each(show);
        if unread.end > feed.offset()? {
            feed.mark(unread.end)?;
        }
        if !unread.messages.is_empty() {
            return Ok(ExitCode::SUCCESS);
        }
        if !args.wait {
            println!("no unread messages");
            return Ok(ExitCode::SUCCESS);
        }
        let left = deadline.map_or(POLL, |deadline| deadline.saturating_duration_since(Instant::now()));
        if left.is_zero() {
            println!("no message arrived within {} seconds", args.timeout);
            return Ok(ExitCode::from(TIMED_OUT));
        }
        std::thread::sleep(POLL.min(left));
    }
}
