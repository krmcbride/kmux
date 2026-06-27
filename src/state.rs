use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use directories::BaseDirs;
use serde::{Deserialize, Serialize};

pub const TMUX_PANE_SOURCE: &str = "tmux-pane";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    Working,
    Waiting,
    Done,
}

impl AgentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Waiting => "waiting",
            Self::Done => "done",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentReportKey {
    pub source: String,
    pub instance: String,
    pub id: String,
}

impl AgentReportKey {
    pub fn new(
        source: impl Into<String>,
        instance: impl Into<String>,
        id: impl Into<String>,
    ) -> Self {
        Self {
            source: source.into(),
            instance: instance.into(),
            id: id.into(),
        }
    }

    pub fn tmux_pane(instance: impl Into<String>, pane_id: impl Into<String>) -> Self {
        Self::new(TMUX_PANE_SOURCE, instance, pane_id)
    }

    fn filename(&self) -> String {
        format!(
            "{}__{}__{}.json",
            filename_component(&self.source),
            filename_component(&self.instance),
            filename_component(&self.id)
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentTargetHints {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux_instance: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_current_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_handle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentReportState {
    pub key: AgentReportKey,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub status: AgentStatus,
    pub status_changed_at: u64,
    pub working_elapsed_secs: u64,
    pub observed_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    #[serde(default)]
    pub target: AgentTargetHints,
}

impl AgentReportState {
    pub fn elapsed_secs(&self, now: u64) -> u64 {
        let status_age = now.saturating_sub(self.status_changed_at);
        match self.status {
            AgentStatus::Working => self.working_elapsed_secs.saturating_add(status_age),
            AgentStatus::Waiting | AgentStatus::Done => status_age,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentReportTiming {
    pub status_changed_at: u64,
    pub working_elapsed_secs: u64,
}

pub fn next_report_timing(
    previous: Option<&AgentReportState>,
    key: &AgentReportKey,
    session_id: Option<&str>,
    status: AgentStatus,
    now: u64,
) -> AgentReportTiming {
    let Some(previous) = previous.filter(|report| same_timing_identity(report, key, session_id))
    else {
        return fresh_report_timing(now);
    };

    if previous.status == status {
        return AgentReportTiming {
            status_changed_at: previous.status_changed_at,
            working_elapsed_secs: match status {
                AgentStatus::Done => 0,
                AgentStatus::Working | AgentStatus::Waiting => previous.working_elapsed_secs,
            },
        };
    }

    match (previous.status, status) {
        (AgentStatus::Working, AgentStatus::Waiting) => AgentReportTiming {
            status_changed_at: now,
            working_elapsed_secs: previous
                .working_elapsed_secs
                .saturating_add(now.saturating_sub(previous.status_changed_at)),
        },
        (AgentStatus::Waiting, AgentStatus::Working) => AgentReportTiming {
            status_changed_at: now,
            working_elapsed_secs: previous.working_elapsed_secs,
        },
        _ => fresh_report_timing(now),
    }
}

fn fresh_report_timing(now: u64) -> AgentReportTiming {
    AgentReportTiming {
        status_changed_at: now,
        working_elapsed_secs: 0,
    }
}

fn same_timing_identity(
    previous: &AgentReportState,
    key: &AgentReportKey,
    session_id: Option<&str>,
) -> bool {
    if &previous.key != key {
        return false;
    }

    if key.source == TMUX_PANE_SOURCE {
        return previous.session_id.as_deref() == session_id;
    }

    true
}

#[derive(Debug, Clone)]
pub struct StateStore {
    base_path: PathBuf,
}

impl StateStore {
    pub fn new() -> Result<Self> {
        let base_dirs = BaseDirs::new().context("could not determine state directory")?;
        let state_root = base_dirs
            .state_dir()
            .unwrap_or_else(|| base_dirs.data_local_dir());
        Self::with_path(state_root.join("kmux"))
    }

    pub fn upsert_report(&self, state: &AgentReportState) -> Result<()> {
        let path = self.report_path(&state.key);
        let content = serde_json::to_vec_pretty(state)?;
        write_atomic(&path, &content)
    }

    pub fn get_report(&self, key: &AgentReportKey) -> Result<Option<AgentReportState>> {
        read_report_file(&self.report_path(key))
    }

    pub fn list_reports(&self) -> Result<Vec<AgentReportState>> {
        let reports_dir = self.reports_dir();
        if !reports_dir.exists() {
            return Ok(Vec::new());
        }

        let mut reports = Vec::new();
        for entry in fs::read_dir(&reports_dir)
            .with_context(|| format!("failed to read state directory {}", reports_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                match read_report_file(&path)? {
                    Some(report) => reports.push(report),
                    None => delete_file_if_exists(&path)?,
                }
            }
        }

        reports.sort_by(|left, right| {
            left.target
                .worktree_handle
                .cmp(&right.target.worktree_handle)
                .then_with(|| left.key.source.cmp(&right.key.source))
                .then_with(|| left.key.instance.cmp(&right.key.instance))
                .then_with(|| left.key.id.cmp(&right.key.id))
        });
        Ok(reports)
    }

    pub fn delete_report(&self, key: &AgentReportKey) -> Result<()> {
        delete_file_if_exists(&self.report_path(key))
    }

    pub fn migrate_worktree(
        &self,
        old_handle: &str,
        new_handle: &str,
        old_path: &Path,
        new_path: &Path,
        old_window_name: &str,
        new_window_name: &str,
    ) -> Result<usize> {
        let mut migrated = 0;
        for mut report in self.list_reports()? {
            let matches_handle = report.target.worktree_handle.as_deref() == Some(old_handle);
            let matches_path = report
                .target
                .worktree_path
                .as_deref()
                .is_some_and(|path| Path::new(path) == old_path);
            let matches_window = report.target.window_name.as_deref() == Some(old_window_name);

            if matches_handle || matches_path || matches_window {
                report.target.worktree_handle = Some(new_handle.to_owned());
                report.target.worktree_path = Some(new_path.display().to_string());
                report.target.window_name = Some(new_window_name.to_owned());
                report.observed_at = now_unix_seconds();
                self.upsert_report(&report)?;
                migrated += 1;
            }
        }
        Ok(migrated)
    }

    #[cfg(test)]
    pub fn test_with_path(base_path: impl Into<PathBuf>) -> Result<Self> {
        Self::with_path(base_path)
    }

    fn with_path(base_path: impl Into<PathBuf>) -> Result<Self> {
        let base_path = base_path.into();
        fs::create_dir_all(base_path.join("agent-reports"))
            .with_context(|| format!("failed to create state directory {}", base_path.display()))?;
        Ok(Self { base_path })
    }

    fn reports_dir(&self) -> PathBuf {
        self.base_path.join("agent-reports")
    }

    fn report_path(&self, key: &AgentReportKey) -> PathBuf {
        self.reports_dir().join(key.filename())
    }
}

pub fn now_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn read_report_file(path: &Path) -> Result<Option<AgentReportState>> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()));
        }
    };

    Ok(serde_json::from_str::<AgentReportState>(&content).ok())
}

fn write_atomic(path: &Path, content: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let tmp_path = path.with_extension(format!("json.{}.{nanos}.tmp", std::process::id()));
    fs::write(&tmp_path, content).with_context(|| {
        format!(
            "failed to write temporary state file {}",
            tmp_path.display()
        )
    })?;
    fs::rename(&tmp_path, path)
        .with_context(|| format!("failed to replace state file {}", path.display()))?;
    Ok(())
}

fn delete_file_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to delete {}", path.display())),
    }
}

fn filename_component(value: &str) -> String {
    let mut component = String::new();
    for byte in value.bytes() {
        let ch = byte as char;
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            component.push(ch);
        } else {
            component.push_str(&format!("_x{byte:02X}"));
        }
    }
    component
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn state_store_round_trips_agent_report_state() -> Result<()> {
        let temp = TempDir::new()?;
        let store = StateStore::with_path(temp.path().join("state"))?;
        let key = AgentReportKey::tmux_pane("test", "%1");
        let state = AgentReportState {
            key: key.clone(),
            session_id: Some("ses_123".to_owned()),
            status: AgentStatus::Working,
            status_changed_at: 42,
            working_elapsed_secs: 5,
            observed_at: 43,
            title: Some("OpenCode session".to_owned()),
            context: Some("163.2K (41%)".to_owned()),
            target: AgentTargetHints {
                tmux_instance: Some("test".to_owned()),
                pane_id: Some("%1".to_owned()),
                window_id: Some("@1".to_owned()),
                session_name: Some("project".to_owned()),
                window_name: Some("kmux-feature-auth".to_owned()),
                pane_title: Some("Agent title".to_owned()),
                pane_current_command: Some("nvim".to_owned()),
                worktree_handle: Some("feature-auth".to_owned()),
                worktree_path: Some("/repo__worktrees/feature-auth".to_owned()),
                branch: Some("feature/auth".to_owned()),
                directory: Some("/repo__worktrees/feature-auth".to_owned()),
            },
        };

        store.upsert_report(&state)?;

        assert_eq!(store.list_reports()?, vec![state]);
        store.delete_report(&key)?;
        assert!(store.list_reports()?.is_empty());
        Ok(())
    }

    #[test]
    fn corrupt_agent_report_state_is_pruned() -> Result<()> {
        let temp = TempDir::new()?;
        let store = StateStore::with_path(temp.path().join("state"))?;
        let corrupt_path = store.reports_dir().join("bad.json");
        fs::write(&corrupt_path, "not json")?;

        assert!(store.list_reports()?.is_empty());
        assert!(!corrupt_path.exists());
        Ok(())
    }

    #[test]
    fn migrates_matching_worktree_report_state() -> Result<()> {
        let temp = TempDir::new()?;
        let store = StateStore::with_path(temp.path().join("state"))?;
        let state = AgentReportState {
            key: AgentReportKey::tmux_pane("test", "%1"),
            session_id: None,
            status: AgentStatus::Done,
            status_changed_at: 42,
            working_elapsed_secs: 0,
            observed_at: 43,
            title: None,
            context: None,
            target: AgentTargetHints {
                tmux_instance: Some("test".to_owned()),
                pane_id: Some("%1".to_owned()),
                window_id: Some("@1".to_owned()),
                session_name: Some("project".to_owned()),
                window_name: Some("kmux-old".to_owned()),
                worktree_handle: Some("old".to_owned()),
                worktree_path: Some("/repo__worktrees/old".to_owned()),
                branch: Some("feature/original".to_owned()),
                ..AgentTargetHints::default()
            },
        };
        store.upsert_report(&state)?;
        let before = now_unix_seconds();

        assert_eq!(
            store.migrate_worktree(
                "old",
                "new",
                Path::new("/repo__worktrees/old"),
                Path::new("/repo__worktrees/new"),
                "kmux-old",
                "kmux-new"
            )?,
            1
        );

        let reports = store.list_reports()?;
        assert_eq!(reports[0].target.worktree_handle.as_deref(), Some("new"));
        assert_eq!(
            reports[0].target.worktree_path.as_deref(),
            Some("/repo__worktrees/new")
        );
        assert_eq!(reports[0].target.window_name.as_deref(), Some("kmux-new"));
        assert_eq!(
            reports[0].target.branch.as_deref(),
            Some("feature/original")
        );
        assert_eq!(reports[0].status_changed_at, 42);
        assert!(reports[0].observed_at >= before);
        Ok(())
    }

    #[test]
    fn elapsed_secs_uses_accumulator_only_for_working_reports() {
        let mut report = test_report_state(AgentStatus::Working, 100);
        report.working_elapsed_secs = 1_200;
        assert_eq!(report.elapsed_secs(700), 1_800);

        report.status = AgentStatus::Waiting;
        assert_eq!(report.elapsed_secs(700), 600);

        report.status = AgentStatus::Done;
        assert_eq!(report.elapsed_secs(700), 600);
    }

    #[test]
    fn timing_accumulates_working_across_waiting_pause() {
        let key = AgentReportKey::tmux_pane("test", "%1");
        let mut report = test_report_state(AgentStatus::Working, 0);
        report.key = key.clone();
        report.session_id = Some("ses_root".to_owned());

        let waiting = next_report_timing(
            Some(&report),
            &key,
            Some("ses_root"),
            AgentStatus::Waiting,
            20 * 60,
        );
        assert_eq!(waiting.status_changed_at, 20 * 60);
        assert_eq!(waiting.working_elapsed_secs, 20 * 60);

        report.status = AgentStatus::Waiting;
        report.status_changed_at = waiting.status_changed_at;
        report.working_elapsed_secs = waiting.working_elapsed_secs;

        let resumed = next_report_timing(
            Some(&report),
            &key,
            Some("ses_root"),
            AgentStatus::Working,
            25 * 60,
        );
        assert_eq!(resumed.status_changed_at, 25 * 60);
        assert_eq!(resumed.working_elapsed_secs, 20 * 60);

        report.status = AgentStatus::Working;
        report.status_changed_at = resumed.status_changed_at;
        report.working_elapsed_secs = resumed.working_elapsed_secs;

        assert_eq!(report.elapsed_secs(35 * 60), 30 * 60);
    }

    #[test]
    fn timing_preserves_same_non_pane_report_identity() {
        let key = AgentReportKey::new("opencode-server", "server", "ses_root");
        let mut report = test_report_state(AgentStatus::Working, 100);
        report.key = key.clone();
        report.session_id = Some("ses_root".to_owned());
        report.working_elapsed_secs = 240;
        report.target.window_id = Some("@old".to_owned());

        let timing = next_report_timing(
            Some(&report),
            &key,
            Some("ses_root"),
            AgentStatus::Working,
            300,
        );

        assert_eq!(timing.status_changed_at, 100);
        assert_eq!(timing.working_elapsed_secs, 240);
    }

    #[test]
    fn timing_resets_when_pane_report_root_session_changes() {
        let key = AgentReportKey::tmux_pane("test", "%1");
        let mut report = test_report_state(AgentStatus::Working, 100);
        report.key = key.clone();
        report.session_id = Some("ses_old".to_owned());
        report.working_elapsed_secs = 240;

        let timing = next_report_timing(
            Some(&report),
            &key,
            Some("ses_new"),
            AgentStatus::Working,
            300,
        );

        assert_eq!(timing.status_changed_at, 300);
        assert_eq!(timing.working_elapsed_secs, 0);
    }

    #[test]
    fn timing_starts_and_ends_runs_cleanly() {
        let key = AgentReportKey::tmux_pane("test", "%1");
        let mut done = test_report_state(AgentStatus::Done, 100);
        done.key = key.clone();
        done.working_elapsed_secs = 240;

        let started = next_report_timing(Some(&done), &key, None, AgentStatus::Working, 300);
        assert_eq!(started.status_changed_at, 300);
        assert_eq!(started.working_elapsed_secs, 0);

        let mut working = test_report_state(AgentStatus::Working, 300);
        working.key = key.clone();
        working.working_elapsed_secs = 240;

        let finished = next_report_timing(Some(&working), &key, None, AgentStatus::Done, 500);
        assert_eq!(finished.status_changed_at, 500);
        assert_eq!(finished.working_elapsed_secs, 0);

        let repeated_done = next_report_timing(Some(&done), &key, None, AgentStatus::Done, 500);
        assert_eq!(repeated_done.status_changed_at, 100);
        assert_eq!(repeated_done.working_elapsed_secs, 0);
    }

    #[test]
    fn timing_saturates_when_clock_moves_backwards() {
        let key = AgentReportKey::tmux_pane("test", "%1");
        let mut report = test_report_state(AgentStatus::Working, 300);
        report.key = key.clone();
        report.working_elapsed_secs = 60;

        let waiting = next_report_timing(Some(&report), &key, None, AgentStatus::Waiting, 200);
        assert_eq!(waiting.status_changed_at, 200);
        assert_eq!(waiting.working_elapsed_secs, 60);

        assert_eq!(report.elapsed_secs(200), 60);
    }

    fn test_report_state(status: AgentStatus, status_changed_at: u64) -> AgentReportState {
        AgentReportState {
            key: AgentReportKey::tmux_pane("test", "%1"),
            session_id: None,
            status,
            status_changed_at,
            working_elapsed_secs: 0,
            observed_at: status_changed_at,
            title: None,
            context: None,
            target: AgentTargetHints::default(),
        }
    }
}
