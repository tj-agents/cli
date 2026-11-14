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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Done {
    Waiting,
    Withdrawn,
    Due,
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

/// A `tj done` mark holds while the host stays idle after the turn that ran it ends; that turn began before the
/// mark, so any turn that began after it withdraws the mark.
pub(crate) fn judge(
    marked: SystemTime,
    now: SystemTime,
    turn: Option<Turn>,
    pinned: &mut Option<Turn>,
    background: u32,
    idle: Duration,
) -> Done {
    let Some(turn) = turn else { return Done::Waiting };
    match *pinned {
        None if turn.changed < marked => return Done::Waiting,
        None if turn.idle => *pinned = Some(turn),
        None => return Done::Withdrawn,
        Some(seen) if seen != turn => return Done::Withdrawn,
        Some(_) => {}
    }
    let quiet = now.duration_since(turn.changed).unwrap_or_default();
    if background == 0 && quiet >= idle { Done::Due } else { Done::Waiting }
}

/// When to ask an idle agent whether its work is finished: once it has stayed idle for the wait with no shell
/// running under it, at most once per turn. The agent's answer to an ask is itself a turn, so the idle record
/// that follows an ask is spent and only a later turn earns another ask.
#[derive(Debug, Default)]
pub(crate) struct Nudge {
    asked: Option<SystemTime>,
    spent: Option<SystemTime>,
}

impl Nudge {
    pub(crate) fn asked(&mut self, at: SystemTime) {
        self.asked = Some(at);
    }

    pub(crate) fn due(&mut self, now: SystemTime, turn: Option<Turn>, background: u32, wait: Duration) -> bool {
        let Some(turn) = turn.filter(|turn| turn.idle) else { return false };
        if let Some(asked) = self.asked {
            if turn.changed <= asked {
                return false;
            }
            self.asked = None;
            self.spent = Some(turn.changed);
        }
        let quiet = now.duration_since(turn.changed).unwrap_or_default();
        self.spent != Some(turn.changed) && background == 0 && quiet >= wait
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use super::{Done, Nudge, Turn, judge, record, task};

    const IDLE: Duration = Duration::from_secs(600);

    fn at(seconds: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(seconds)
    }

    fn turn(idle: bool, changed: u64) -> Option<Turn> {
        Some(Turn { idle, changed: at(changed) })
    }

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

    #[test]
    fn the_turn_that_ran_tj_done_may_finish_and_then_must_stay_idle() {
        let mut pinned = None;
        assert_eq!(judge(at(100), at(101), turn(false, 50), &mut pinned, 0, IDLE), Done::Waiting);
        assert_eq!(judge(at(100), at(130), turn(true, 120), &mut pinned, 0, IDLE), Done::Waiting);
        assert_eq!(judge(at(100), at(719), turn(true, 120), &mut pinned, 0, IDLE), Done::Waiting);
        assert_eq!(judge(at(100), at(720), turn(true, 120), &mut pinned, 0, IDLE), Done::Due);
    }

    #[test]
    fn a_turn_started_after_the_mark_withdraws_it() {
        assert_eq!(judge(at(100), at(130), turn(false, 120), &mut None, 0, IDLE), Done::Withdrawn);
        let mut pinned = turn(true, 120);
        assert_eq!(judge(at(100), at(900), turn(false, 800), &mut pinned, 0, IDLE), Done::Withdrawn);
        let mut pinned = turn(true, 120);
        assert_eq!(judge(at(100), at(900), turn(true, 800), &mut pinned, 0, IDLE), Done::Withdrawn);
    }

    #[test]
    fn an_unreadable_host_or_a_background_shell_never_closes() {
        let mut pinned = turn(true, 120);
        assert_eq!(judge(at(100), at(9000), None, &mut pinned, 0, IDLE), Done::Waiting);
        assert_eq!(judge(at(100), at(9000), turn(true, 120), &mut pinned, 1, IDLE), Done::Waiting);
        assert_eq!(judge(at(100), at(9000), turn(true, 120), &mut pinned, 0, IDLE), Done::Due);
    }

    #[test]
    fn an_idle_agent_is_asked_once_and_again_only_after_a_later_turn() {
        let mut nudge = Nudge::default();
        assert!(!nudge.due(at(100), turn(true, 50), 0, IDLE));
        assert!(nudge.due(at(650), turn(true, 50), 0, IDLE));
        nudge.asked(at(650));
        assert!(!nudge.due(at(9000), turn(true, 50), 0, IDLE));
        assert!(!nudge.due(at(9000), turn(false, 660), 0, IDLE));
        assert!(!nudge.due(at(9000), turn(true, 700), 0, IDLE));
        assert!(!nudge.due(at(9900), turn(true, 700), 0, IDLE));
        assert!(!nudge.due(at(9100), turn(true, 9000), 0, IDLE));
        assert!(nudge.due(at(9600), turn(true, 9000), 0, IDLE));
    }

    #[test]
    fn a_busy_agent_a_background_shell_or_an_unreadable_host_is_never_asked() {
        let mut nudge = Nudge::default();
        assert!(!nudge.due(at(9000), turn(false, 50), 0, IDLE));
        assert!(!nudge.due(at(9000), turn(true, 50), 1, IDLE));
        assert!(!nudge.due(at(9000), None, 0, IDLE));
        assert!(nudge.due(at(9000), turn(true, 50), 0, IDLE));
    }

    #[test]
    fn an_idle_record_older_than_the_mark_waits_for_the_marking_turn_to_end() {
        let mut pinned = None;
        assert_eq!(judge(at(100), at(9000), turn(true, 10), &mut pinned, 0, IDLE), Done::Waiting);
        assert_eq!(judge(at(100), at(8100), turn(true, 8000), &mut pinned, 0, IDLE), Done::Waiting);
        assert_eq!(judge(at(100), at(8600), turn(true, 8000), &mut pinned, 0, IDLE), Done::Due);
    }
}
