use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result};
use rayon::prelude::*;

use super::parse::{CodexMeta, Session};
use super::render;
use super::text::wrap;
use crate::paths::file_stem;

/// Bump when parsing or rendering changes shape: a stale header forces a full re-parse instead of
/// serving rows built by the old logic.
const INDEX_VERSION: &str = "tj-cr-index-v1";
const META_VERSION: &str = "tj-cr-meta-v1";

#[derive(Clone)]
pub struct FileInfo {
    pub path: PathBuf,
    pub mtime: u128,
    pub size: u64,
}

impl FileInfo {
    pub fn read(path: PathBuf) -> Option<Self> {
        let md = fs::metadata(&path).ok()?;
        let mtime = md.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_nanos();
        Some(Self { path, mtime, size: md.len() })
    }

    fn key(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }
}

/// One indexed session. `pane` names its rendered preview files.
#[derive(Clone)]
pub struct Entry {
    pub mtime: u128,
    pub size: u64,
    pub pane: String,
    pub story: String,
    pub cwd: String,
    pub preview: String,
    pub haystack: String,
}

/// TABs and newlines would split a TSV record or an fzf row.
fn field(s: &str) -> String {
    s.replace(['\t', '\n', '\r'], " ")
}

fn read_table(path: &Path, version: &str, columns: usize) -> HashMap<String, Vec<String>> {
    let Ok(text) = fs::read_to_string(path) else { return HashMap::new() };
    let mut lines = text.lines();
    if lines.next() != Some(version) {
        return HashMap::new();
    }
    lines
        .map(|l| l.splitn(columns, '\t').map(str::to_string).collect::<Vec<_>>())
        .filter(|r| r.len() == columns)
        .map(|r| (r[0].clone(), r))
        .collect()
}

/// Rows whose transcript has since been deleted are dropped, so the cache never outgrows the history.
fn write_table(path: &Path, version: &str, rows: impl Iterator<Item = (String, Vec<String>)>) -> Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::io::BufWriter::new(fs::File::create(&tmp)?);
        writeln!(f, "{version}")?;
        for (key, row) in rows {
            if Path::new(&key).exists() {
                writeln!(f, "{}", row.join("\t"))?;
            }
        }
        f.flush()?;
    }
    fs::rename(&tmp, path).context("failed to replace the index")
}

pub struct Index {
    dir: PathBuf,
}

impl Index {
    pub fn open(dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(dir.join("panes")).with_context(|| format!("cannot create {}", dir.display()))?;
        Ok(Self { dir })
    }

    pub fn panes(&self) -> PathBuf {
        self.dir.join("panes")
    }

    /// Indexes `files`, re-parsing only new or modified transcripts (in parallel), and returns an entry
    /// per file keyed by path. The first run is the slow one; every run after reads the cache.
    pub fn update(
        &self,
        files: &[FileInfo],
        rebuild: bool,
        width: usize,
        parse: impl Fn(&Path) -> Session + Sync,
    ) -> Result<HashMap<String, Entry>> {
        let table = self.dir.join("index.tsv");
        let mut cache = if rebuild { HashMap::new() } else { read_table(&table, INDEX_VERSION, 9) };
        let panes = self.panes();

        let stale: Vec<&FileInfo> = files
            .iter()
            .filter(|f| {
                let fresh = cache.get(&f.key()).is_some_and(|r| r[1] == f.mtime.to_string());
                // A row with no pane on disk would leave TAB blank forever, so a missing pane re-parses too.
                !(fresh && panes.join(format!("{}.ends.txt", file_stem(&f.path))).exists())
            })
            .collect();

        let parsed: Vec<(String, Vec<String>)> = stale
            .par_iter()
            .map(|f| {
                let s = parse(&f.path);
                let pane = file_stem(&f.path);
                write_pane(&panes, &pane, &s, width);
                let row = vec![
                    f.key(),
                    f.mtime.to_string(),
                    f.size.to_string(),
                    pane,
                    field(&s.branch),
                    field(&s.story),
                    field(&s.cwd),
                    field(&s.preview),
                    field(&s.haystack),
                ];
                (f.key(), row)
            })
            .collect();

        if !parsed.is_empty() {
            if parsed.len() >= 5 {
                eprintln!("\x1b[90mcr: indexed {} new/changed session(s)\x1b[0m", parsed.len());
            }
            cache.extend(parsed);
            write_table(&table, INDEX_VERSION, cache.iter().map(|(k, v)| (k.clone(), v.clone())))?;
        }

        Ok(files
            .iter()
            .filter_map(|f| {
                let r = cache.get(&f.key())?;
                Some((
                    f.key(),
                    Entry {
                        mtime: r[1].parse().unwrap_or(f.mtime),
                        size: r[2].parse().unwrap_or(f.size),
                        pane: r[3].clone(),
                        story: r[5].clone(),
                        cwd: r[6].clone(),
                        preview: r[7].clone(),
                        haystack: r[8].clone(),
                    },
                ))
            })
            .collect())
    }

    /// Codex does not bucket sessions by project, so every rollout's first line is peeked (and cached)
    /// to learn its cwd before scoping; only the in-scope survivors are fully indexed.
    pub fn update_meta(
        &self,
        files: &[FileInfo],
        rebuild: bool,
        peek: impl Fn(&Path) -> CodexMeta + Sync,
    ) -> Result<HashMap<String, CodexMeta>> {
        let table = self.dir.join("meta.tsv");
        let mut cache = if rebuild { HashMap::new() } else { read_table(&table, META_VERSION, 5) };
        let parsed: Vec<(String, Vec<String>)> = files
            .par_iter()
            .filter(|f| !cache.get(&f.key()).is_some_and(|r| r[1] == f.mtime.to_string()))
            .map(|f| {
                let m = peek(&f.path);
                let row = vec![
                    f.key(),
                    f.mtime.to_string(),
                    field(&m.session_id),
                    field(&m.cwd),
                    (m.subagent as u8).to_string(),
                ];
                (f.key(), row)
            })
            .collect();
        if !parsed.is_empty() {
            cache.extend(parsed);
            write_table(&table, META_VERSION, cache.iter().map(|(k, v)| (k.clone(), v.clone())))?;
        }
        Ok(files
            .iter()
            .filter_map(|f| {
                let r = cache.get(&f.key())?;
                Some((f.key(), CodexMeta { session_id: r[2].clone(), cwd: r[3].clone(), subagent: r[4] == "1" }))
            })
            .collect())
    }
}

/// Two small files per session, so the preview command never re-reads a multi-MB transcript: the
/// wrapped haystack it greps for the live query, and the pre-rendered conversation it prints.
fn write_pane(dir: &Path, pane: &str, s: &Session, width: usize) {
    let mut hay = wrap(&s.haystack, 96);
    hay.retain(|l| !l.trim().is_empty());
    if hay.is_empty() {
        hay.push("(nothing said)".into());
    }
    let ends = render::pane(&s.head, &s.tail, &hay, width);
    let _ = fs::write(dir.join(format!("{pane}.txt")), hay.join("\n") + "\n");
    let _ = fs::write(dir.join(format!("{pane}.ends.txt")), ends.join("\n") + "\n");
}
