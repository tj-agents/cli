mod index;
mod parse;
mod render;
mod scope;
mod text;

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local};
use clap::ValueEnum;
use regex::Regex;
use walkdir::WalkDir;

use crate::{fzf, paths};
use index::{Entry, FileInfo, Index};
use scope::{Choice, Scope, project_key};

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Agent {
    Claude,
    Codex,
}

impl Agent {
    fn name(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
        }
    }
}

/// Type `cr` to open a search bar over this repo's sessions, keep typing to filter across everything
/// said in them, and press ENTER to resume one in the directory it was started from.
#[derive(clap::Args)]
pub struct Args {
    /// Open pre-filtered to sessions where all of these words were said
    query: Vec<String>,
    /// Every project, not just this repo (CTRL-A does the same from inside the bar)
    #[arg(short, long)]
    all: bool,
    /// Search Claude Code sessions without asking
    #[arg(long, conflicts_with = "codex")]
    claude: bool,
    /// Search Codex sessions without asking
    #[arg(long)]
    codex: bool,
    /// How many of the most recent sessions to search (0 = all)
    #[arg(short = 'n', long, default_value_t = 500)]
    count: usize,
    /// Print the rows instead of opening the picker
    #[arg(long)]
    list: bool,
    /// Discard the index and rebuild it from scratch
    #[arg(long)]
    rebuild: bool,
}

/// The hidden command fzf runs to draw the TAB pane.
#[derive(clap::Args)]
pub struct PreviewArgs {
    agent: Agent,
    pane: String,
    query: Vec<String>,
}

pub fn run(args: Args) -> Result<ExitCode> {
    fzf::ensure()?;
    let agent = if args.codex {
        Agent::Codex
    } else if args.claude {
        Agent::Claude
    } else {
        let header = [
            "--prompt",
            "agent> ",
            "--height",
            "30%",
            "--border",
            "--reverse",
            "--header",
            "search whose sessions? ESC = Claude",
        ];
        match fzf::pick(&header, vec!["Claude".into(), "Codex".into()])?.as_deref().map(str::trim) {
            Some("Codex") => Agent::Codex,
            _ => Agent::Claude,
        }
    };
    let query = args.query.join(" ").trim().to_string();
    search(agent, &args, query, args.all)
}

/// A pane laid out at the width the terminal has now: 55% of it, less the border and scrollbar gutter.
fn pane_width() -> usize {
    let cols = terminal_size::terminal_size()
        .or_else(|| terminal_size::terminal_size_of(std::io::stderr()))
        .map_or(120, |(w, _)| w.0 as usize);
    ((cols as f64 * 0.55) as usize).saturating_sub(8).max(40)
}

fn jsonl_in(dir: &Path, recursive: bool) -> Vec<FileInfo> {
    let depth = if recursive { usize::MAX } else { 1 };
    WalkDir::new(dir)
        .max_depth(depth)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file() && e.path().extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|e| FileInfo::read(e.into_path()))
        .collect()
}

fn newest(mut files: Vec<FileInfo>, count: usize) -> Vec<FileInfo> {
    files.sort_by_key(|f| std::cmp::Reverse(f.mtime));
    if count > 0 {
        files.truncate(count);
    }
    files
}

fn when(mtime: u128) -> String {
    let t: DateTime<Local> = (UNIX_EPOCH + Duration::from_nanos(mtime as u64)).into();
    if t.date_naive() == Local::now().date_naive() {
        format!("{:>11}", format!("today {}", t.format("%H:%M")))
    } else {
        t.format("%m-%d %H:%M").to_string()
    }
}

struct Found {
    rows: Vec<String>,
    /// pane key -> (session id, cwd to resume from)
    targets: HashMap<String, (String, String)>,
    scope_name: String,
    widened: bool,
}

fn search(agent: Agent, args: &Args, query: String, all: bool) -> Result<ExitCode> {
    let found = match agent {
        Agent::Claude => claude_rows(args, all)?,
        Agent::Codex => codex_rows(args, all)?,
    };
    let all = all || found.widened;
    if found.rows.is_empty() {
        bail!("no matching conversations");
    }
    if args.list {
        // A closed pipe (`| head`) just ends the listing.
        let mut out = std::io::stdout().lock();
        for r in &found.rows {
            if writeln!(out, "{}", r.split("  ||  ").next().unwrap_or(r)).is_err() {
                break;
            }
        }
        return Ok(ExitCode::SUCCESS);
    }

    let exe = std::env::current_exe()?.to_string_lossy().into_owned();
    let exe = if cfg!(windows) { format!("\"{exe}\"") } else { format!("'{}'", exe.replace('\'', r"'\''")) };
    let widen = if agent == Agent::Claude { "search every project" } else { "search every session" };
    let mut fzf_args: Vec<String> = [
        "--prompt",
        "search> ",
        "--height",
        "70%",
        "--border",
        "--reverse",
        "--exact",
        "--no-sort",
        "--delimiter",
        "\t",
        "--with-nth",
        "1",
        "--print-query",
        "--expect",
        "ctrl-a",
        "--preview-window",
        "right:55%:wrap-word:follow:hidden",
        "--preview-wrap-sign",
        "  ",
        "--bind",
        "tab:toggle-preview",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    fzf_args.extend([
        "--preview".into(),
        format!("{exe} cr-preview {} {{2}} {{q}}", agent.name()),
        "--header".into(),
        format!(
            "scope: {} / TAB = how it ended / ENTER resumes / CTRL-A = {widen} / PLAN = unfinished plan",
            found.scope_name
        ),
    ]);
    if !query.is_empty() {
        fzf_args.extend(["--query".into(), query]);
    }

    let Some(out) = fzf::run(&fzf_args, found.rows)? else { return Ok(ExitCode::from(1)) };
    // --print-query puts the typed query on line 1 and the --expect key on line 2; the row is line 3.
    if out.get(1).map(|s| s.trim()) == Some("ctrl-a") {
        if all {
            return Ok(ExitCode::from(1));
        }
        return search(agent, args, out[0].clone(), true);
    }
    let Some(row) = out.get(2).filter(|r| !r.trim().is_empty()) else { return Ok(ExitCode::from(1)) };
    let pane = row.rsplit('\t').next().unwrap_or_default();
    let Some((sid, cwd)) = found.targets.get(pane) else { bail!("could not resolve a session for that row") };
    resume(agent, sid, cwd)
}

fn row(label: String, haystack: &str, pane: &str) -> String {
    format!("{label}  ||  {haystack}\t{pane}")
}

fn claude_rows(args: &Args, all: bool) -> Result<Found> {
    let home = paths::claude_home()?;
    let root = home.join("projects");
    if !root.is_dir() {
        bail!("no Claude history at {}", root.display());
    }
    let scope = Scope::select(all)?;
    let all_dirs: Vec<PathBuf> =
        fs::read_dir(&root)?.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.is_dir()).collect();
    let name = |p: &PathBuf| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let under = |p: &PathBuf, key: &str| {
        let n = name(p);
        n == key || n.starts_with(&format!("{key}-"))
    };

    // Scope is this directory and everything ever under it: a folder since split up or deleted still
    // owns its sessions. The repo's parent folder is folded in too, since a session rooted there is
    // otherwise invisible from every checkout inside it.
    let (key, exclude): (String, Vec<String>) = match &scope.choice {
        Choice::All => (project_key(&scope.base), vec![]),
        Choice::Main => (project_key(&scope.base), scope.others.iter().map(|o| project_key(&o.path)).collect()),
        Choice::Worktree(p) => (project_key(p), vec![]),
    };
    let mut dirs: Vec<PathBuf> =
        all_dirs.iter().filter(|d| under(d, &key) && !exclude.iter().any(|x| under(d, x))).cloned().collect();
    if scope.is_repo {
        if let Some(parent) = Path::new(&scope.base).parent() {
            let pkey = project_key(&parent.to_string_lossy());
            dirs.extend(all_dirs.iter().filter(|d| name(d) == pkey).cloned());
        }
    }
    let mut widened = false;
    if all {
        dirs = all_dirs.clone();
    } else if dirs.is_empty() {
        eprintln!("\x1b[33mno conversations for {} - widening to every project\x1b[0m", scope.leaf());
        dirs = all_dirs.clone();
        widened = true;
    }
    dirs.sort();
    dirs.dedup();

    // Subagent transcripts live in per-session subfolders and are not resumable, hence not recursive.
    let files = newest(dirs.iter().flat_map(|d| jsonl_in(d, false)).collect(), args.count);
    let inflight: Vec<String> = WalkDir::new(home.join("plans"))
        .into_iter()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("_inflight-") && n.ends_with(".md"))
        .collect();

    let index = Index::open(paths::cache_dir()?.join("claude"))?;
    let entries = index.update(&files, args.rebuild, pane_width(), parse::claude)?;
    let mut targets = HashMap::new();
    let rows = files
        .iter()
        .filter_map(|f| {
            let e = entries.get(f.path.to_string_lossy().as_ref())?;
            let sid = paths::file_stem(&f.path);
            let plan = inflight.iter().any(|n| n.contains(text::truncate(&sid, 8)));
            let label = format!(
                "{}  {}  {:>7} KB  {:<9} {}",
                when(e.mtime),
                if plan { "PLAN" } else { "    " },
                e.size / 1024,
                e.story,
                e.preview
            );
            targets.insert(e.pane.clone(), (sid, e.cwd.clone()));
            Some(row(label, &e.haystack, &e.pane))
        })
        .collect::<Vec<_>>();

    let scope_name = if all || widened {
        "every project".to_string()
    } else {
        format!("{} ({} project folder(s), {} sessions)", scope.leaf(), dirs.len(), rows.len())
    };
    Ok(Found { rows, targets, scope_name, widened })
}

fn codex_rows(args: &Args, all: bool) -> Result<Found> {
    let root = paths::codex_home()?.join("sessions");
    if !root.is_dir() {
        bail!("no Codex history at {}", root.display());
    }
    let scope = Scope::select(all)?;
    let every = jsonl_in(&root, true);
    let index = Index::open(paths::cache_dir()?.join("codex"))?;
    let meta = index.update_meta(&every, args.rebuild, parse::codex_meta)?;
    let resumable =
        |f: &FileInfo| meta.get(f.path.to_string_lossy().as_ref()).filter(|m| !m.subagent && !m.session_id.is_empty());

    let mut widened = false;
    let mut in_scope: Vec<FileInfo> =
        every.iter().filter(|f| resumable(f).is_some_and(|m| all || scope.contains_cwd(&m.cwd))).cloned().collect();
    if in_scope.is_empty() && !all {
        eprintln!("\x1b[33mno Codex conversations for {} - widening to every session\x1b[0m", scope.leaf());
        in_scope = every.iter().filter(|f| resumable(f).is_some()).cloned().collect();
        widened = true;
    }
    let files = newest(in_scope, args.count);

    let entries = index.update(&files, args.rebuild, pane_width(), parse::codex)?;
    let mut targets = HashMap::new();
    let rows = files
        .iter()
        .filter_map(|f| {
            let e: &Entry = entries.get(f.path.to_string_lossy().as_ref())?;
            let m = resumable(f)?;
            targets.insert(e.pane.clone(), (m.session_id.clone(), m.cwd.clone()));
            let label = format!("{}  {:>7} KB  {}", when(e.mtime), e.size / 1024, e.preview);
            Some(row(label, &e.haystack, &e.pane))
        })
        .collect::<Vec<_>>();

    let scope_name = if all || widened {
        "every session".to_string()
    } else {
        format!("{} ({} sessions)", scope.leaf(), rows.len())
    };
    Ok(Found { rows, targets, scope_name, widened })
}

/// A session resumes only from the directory it was started in, so the agent is launched there.
fn resume(agent: Agent, sid: &str, cwd: &str) -> Result<ExitCode> {
    let exe = which::which(agent.name()).with_context(|| format!("{} is not on PATH", agent.name()))?;
    let mut cmd = Command::new(exe);
    match agent {
        Agent::Claude => cmd.args(["--resume", sid]),
        Agent::Codex => cmd.args(["resume", sid]),
    };
    let here = std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    if !cwd.is_empty() && Path::new(cwd).is_dir() && paths::norm(cwd) != paths::norm(&here) {
        eprintln!(
            "\x1b[90m-> cd {cwd}; {} {}\x1b[0m",
            agent.name(),
            if agent == Agent::Claude { "--resume" } else { "resume" }
        );
        cmd.current_dir(cwd);
    } else {
        eprintln!("\x1b[90m-> {} {sid}\x1b[0m", agent.name());
    }
    let status = cmd.status().with_context(|| format!("failed to start {}", agent.name()))?;
    Ok(ExitCode::from(status.code().unwrap_or(1).clamp(0, 255) as u8))
}

/// Prints the rendered conversation, then the haystack lines matching the live query, so the pane
/// (which opens scrolled to its end) lands on why this session matched.
pub fn preview(args: PreviewArgs) -> Result<ExitCode> {
    let dir = paths::cache_dir()?.join(args.agent.name()).join("panes");
    if args.pane.contains(['/', '\\']) || args.pane.contains("..") {
        bail!("invalid pane name");
    }
    let ends = fs::read_to_string(dir.join(format!("{}.ends.txt", args.pane))).unwrap_or_default();
    let mut out = ends;
    let emit = |out: String| {
        let _ = std::io::stdout().write_all(out.as_bytes());
        Ok(ExitCode::SUCCESS)
    };

    // fzf --exact syntax: space-separated AND terms; negations and anchors don't select lines here.
    let terms: Vec<String> = args
        .query
        .join(" ")
        .split_whitespace()
        .filter(|t| !t.starts_with('!') && *t != "|")
        .map(|t| t.trim_start_matches(['\'', '^']).trim_end_matches('$').to_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    if terms.is_empty() {
        return emit(out);
    }
    let hay = fs::read_to_string(dir.join(format!("{}.txt", args.pane))).unwrap_or_default();
    let hits: Vec<&str> = hay
        .lines()
        .filter(|l| {
            let low = l.to_lowercase();
            terms.iter().all(|t| low.contains(t))
        })
        .collect();
    let hits = if hits.is_empty() {
        hay.lines()
            .filter(|l| {
                let low = l.to_lowercase();
                terms.iter().any(|t| low.contains(t))
            })
            .collect()
    } else {
        hits
    };
    if hits.is_empty() {
        return emit(out);
    }
    let pattern = terms.iter().map(|t| regex::escape(t)).collect::<Vec<_>>().join("|");
    let mark = Regex::new(&format!("(?i){pattern}"))?;
    out += &format!("\n\x1b[38;5;240m\u{2500}\u{2500} said: {} \u{2500}\u{2500}\x1b[0m\n", terms.join(" "));
    for line in hits.iter().take(40) {
        out += &format!("  {}\n", mark.replace_all(line, "\x1b[1;33m$0\x1b[0m"));
    }
    if hits.len() > 40 {
        out += &format!("\x1b[38;5;240m  \u{2026} {} more\x1b[0m\n", hits.len() - 40);
    }
    emit(out)
}
