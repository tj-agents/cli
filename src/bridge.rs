use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tj_cli::process::agent;

use crate::paths;

const SOCKET: &str = "CLAUDE_CODE_MESSAGING_SOCKET";
const TOKEN: &str = "CLAUDE_CODE_MESSAGING_TOKEN";
const SESSION: &str = "CLAUDE_CODE_SESSION_ID";

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn endpoint(session: &str) -> Result<PathBuf> {
    if session.is_empty() || !session.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        bail!("session id {session:?} must be letters, digits, '-' or '_'");
    }
    Ok(paths::cache_dir()?.join("bridge").join(format!("{session}.json")))
}

/// Native Windows verifies an own-child message only by the token in its first line.
pub fn post_own(text: &str) -> Result<()> {
    let socket = env(SOCKET).with_context(|| format!("{SOCKET} is not set"))?;
    let token = env(TOKEN).with_context(|| format!("{TOKEN} is not set"))?;
    let lines = format!(
        "{}\n{}\n",
        json!({"type": "auth", "token": token}),
        json!({"type": "user", "message": {"role": "user", "content": text}})
    );
    #[cfg(windows)]
    let inbox = std::fs::OpenOptions::new().read(true).write(true).open(&socket);
    #[cfg(unix)]
    let inbox = std::os::unix::net::UnixStream::connect(&socket);
    let mut inbox = inbox.with_context(|| format!("could not open the session inbox {socket}"))?;
    inbox.write_all(lines.as_bytes())?;
    inbox.flush()?;
    Ok(())
}

pub fn post(session: &str, text: &str) -> Result<()> {
    let path = endpoint(session)?;
    let record: Value = serde_json::from_slice(
        &std::fs::read(&path)
            .with_context(|| format!("the session has no message relay ({} is missing)", path.display()))?,
    )?;
    let port = record["port"].as_u64().and_then(|port| u16::try_from(port).ok()).context("relay record has no port")?;
    let address = (Ipv4Addr::LOCALHOST, port).into();
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(5))
        .context("the session's message relay is not running")?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    writeln!(stream, "{}", json!({"secret": record["secret"], "text": text}))?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply)?;
    let reply: Value = serde_json::from_str(&reply).context("the relay closed without replying")?;
    if reply["ok"] != Value::Bool(true) {
        bail!("relay: {}", reply["error"].as_str().unwrap_or("failed"));
    }
    Ok(())
}

pub fn run() -> Result<ExitCode> {
    if [SOCKET, TOKEN, SESSION].iter().any(|name| env(name).is_none()) {
        return Ok(ExitCode::SUCCESS);
    }
    serve()
}

fn secret() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| anyhow::anyhow!("no OS randomness: {error}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn write_private(path: &PathBuf, contents: &str) -> Result<()> {
    let folder = path.parent().context("endpoint has no folder")?;
    std::fs::create_dir_all(folder)?;
    let staging = path.with_extension(format!("{}.tmp", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(&staging)?.write_all(contents.as_bytes())?;
    std::fs::rename(&staging, path)?;
    Ok(())
}

fn owned_by_me(path: &PathBuf) -> bool {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .is_some_and(|record| record["pid"].as_u64() == Some(u64::from(std::process::id())))
}

fn serve() -> Result<ExitCode> {
    let session = env(SESSION).with_context(|| format!("{SESSION} is not set"))?;
    let path = endpoint(&session)?;
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let secret = secret()?;
    let record = json!({"port": listener.local_addr()?.port(), "secret": secret, "pid": std::process::id()});
    write_private(&path, &record.to_string())?;
    if let Some(pid) = env("CLAUDE_PID").and_then(|pid| pid.parse::<u32>().ok()) {
        let born = agent::started_at(pid);
        let path = path.clone();
        std::thread::spawn(move || {
            while agent::started_at(pid) == born {
                std::thread::sleep(Duration::from_secs(5));
            }
            if owned_by_me(&path) {
                let _ = std::fs::remove_file(&path);
            }
            std::process::exit(0);
        });
    }
    for stream in listener.incoming().flatten() {
        let _ = relay(stream, &secret);
    }
    Ok(ExitCode::SUCCESS)
}

fn relay(stream: TcpStream, secret: &str) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
    let request: Value = serde_json::from_str(&line)?;
    let reply = if request["secret"].as_str() != Some(secret) {
        json!({"ok": false, "error": "wrong relay secret"})
    } else {
        match request["text"].as_str().context("no text").and_then(post_own) {
            Ok(()) => json!({"ok": true}),
            Err(error) => json!({"ok": false, "error": format!("{error:#}")}),
        }
    };
    writeln!(&stream, "{reply}")?;
    Ok(())
}
