use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use tj_cli::process::agent;

use crate::paths;

const SHELLS: &[&str] = &["bash", "sh", "dash", "zsh", "fish", "pwsh", "powershell"];
const TAIL: u64 = 64 * 1024;

/// The host's latest turn as the host itself records it: waiting for input or not, and since when.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Turn {
    pub idle: bool,
    pub changed: SystemTime,
}

/// Claude's own per-process record, `<claude home>/sessions/<pid>.json`; its format belongs to Claude Code.
pub(crate) fn claude(agent: u32) -> Option<Turn> {
    let home = paths::claude_home().ok()?;
    record(&std::fs::read_to_string(home.join("sessions").join(format!("{agent}.json"))).ok()?)
}

fn record(text: &str) -> Option<Turn> {
    let record: Value = serde_json::from_str(text).ok()?;
    let changed = UNIX_EPOCH + Duration::from_millis(record["statusUpdatedAt"].as_u64()?);
    Some(Turn { idle: record["status"].as_str()? == "idle", changed })
}

/// Shells the agent started and left running, such as background tasks.
pub(crate) fn shells(agent: u32) -> u32 {
    let Ok(images) = agent::descendants(agent) else { return u32::MAX };
    let shell = |image: &String| {
        let stem = image.rsplit_once('.').map_or(image.as_str(), |(stem, _)| stem);
        SHELLS.iter().any(|name| stem.eq_ignore_ascii_case(name))
    };
    u32::try_from(images.iter().filter(|image| shell(image)).count()).unwrap_or(u32::MAX)
}

/// The Codex thread's rollout, `<codex home>/sessions/YYYY/MM/DD/rollout-<time>-<thread>.jsonl`.
pub(crate) fn rollout(thread: &str) -> Option<PathBuf> {
    newest(&paths::codex_home().ok()?.join("sessions"), &format!("-{thread}.jsonl"))
}

fn newest(folder: &Path, suffix: &str) -> Option<PathBuf> {
    let mut entries: Vec<PathBuf> =
        std::fs::read_dir(folder).ok()?.filter_map(|entry| Some(entry.ok()?.path())).collect();
    entries.sort_unstable_by(|left, right| right.cmp(left));
    entries.into_iter().find_map(|path| {
        if path.is_dir() {
            return newest(&path, suffix);
        }
        path.file_name()?.to_str()?.ends_with(suffix).then_some(path)
    })
}

/// The last task event in a Codex rollout; its format belongs to Codex.
pub(crate) fn codex(rollout: &Path) -> Option<Turn> {
    let mut file = std::fs::File::open(rollout).ok()?;
    let length = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(length.saturating_sub(TAIL))).ok()?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail).ok()?;
    task(&String::from_utf8_lossy(&tail))
}

fn task(tail: &str) -> Option<Turn> {
    let line = tail.lines().rev().filter_map(|line| serde_json::from_str::<Value>(line).ok()).find(|line| {
        line["type"] == "event_msg"
            && matches!(line["payload"]["type"].as_str(), Some("task_started" | "task_complete"))
    })?;
    let changed = chrono::DateTime::parse_from_rfc3339(line["timestamp"].as_str()?).ok()?;
    Some(Turn { idle: line["payload"]["type"] == "task_complete", changed: changed.into() })
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::{Turn, record, task};

    #[test]
    fn claudes_record_gives_the_status_and_when_it_last_changed() {
        let record_of = |status: &str| format!(r#"{{"pid":1,"status":"{status}","statusUpdatedAt":7000}}"#);
        let changed = UNIX_EPOCH + Duration::from_millis(7000);
        assert_eq!(record(&record_of("idle")), Some(Turn { idle: true, changed }));
        assert_eq!(record(&record_of("busy")), Some(Turn { idle: false, changed }));
        assert_eq!(record(r#"{"status":"idle"}"#), None);
        assert_eq!(record(r#"{"status":"idle","statusUpdat"#), None);
    }

    #[test]
    fn the_last_codex_task_event_is_the_turn_even_after_a_torn_line() {
        let fixture = include_str!("../../tests/fixtures/codex-turns.jsonl");
        let ended = chrono::DateTime::parse_from_rfc3339("2026-11-11T17:27:40.500Z").unwrap().into();
        assert_eq!(task(fixture), Some(Turn { idle: true, changed: ended }));
        let next = r#"{"timestamp":"2026-11-11T17:30:00Z","type":"event_msg","payload":{"type":"task_started"}}"#;
        assert_eq!(task(&format!("{fixture}\n{next}")).map(|turn| turn.idle), Some(false));
        assert_eq!(
            task(r#"{"timestamp":"2026-11-11T17:30:00Z","type":"event_msg","payload":{"type":"token_count"}}"#),
            None
        );
    }
}
