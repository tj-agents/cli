use std::io::Write;
use std::process::{Command, Stdio};
use std::thread;

use anyhow::{Context, Result, bail};

pub fn ensure() -> Result<()> {
    if which::which("fzf").is_err() {
        bail!("fzf is not on PATH - install it (Arch: pacman -S fzf, Windows: winget install junegunn.fzf)");
    }
    Ok(())
}

/// Runs fzf over `lines` and returns its stdout split into lines, or None when the user cancelled.
/// fzf draws on the terminal itself, so only stdin and stdout are ours.
pub fn run(args: &[String], lines: Vec<String>) -> Result<Option<Vec<String>>> {
    let exe = which::which("fzf").context("fzf is not on PATH")?;
    let mut child = Command::new(exe)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("failed to start fzf")?;

    // Fed from its own thread: a large row set fills the pipe before fzf starts reading.
    let mut stdin = child.stdin.take().expect("stdin is piped");
    let feeder = thread::spawn(move || {
        let mut buf = std::io::BufWriter::new(&mut stdin);
        for line in lines {
            if buf.write_all(line.as_bytes()).and_then(|_| buf.write_all(b"\n")).is_err() {
                break; // fzf exited early - ENTER on the first rows before the rest were written
            }
        }
    });

    let out = child.wait_with_output().context("fzf failed")?;
    let _ = feeder.join();
    // 0 = selected, 1 = no match (still prints the query with --print-query), 130 = ESC / CTRL-C.
    match out.status.code() {
        Some(0) | Some(1) => {}
        Some(130) => return Ok(None),
        code => bail!("fzf exited with {code:?}"),
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(Some(text.lines().map(|l| l.trim_end_matches('\r').to_string()).collect()))
}

/// One pick from a short list; None when cancelled or nothing chosen.
pub fn pick(args: &[&str], items: Vec<String>) -> Result<Option<String>> {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    Ok(run(&args, items)?.and_then(|l| l.into_iter().next()).filter(|s| !s.trim().is_empty()))
}
