use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use directories::BaseDirs;
use serde::Deserialize;
use unicode_width::UnicodeWidthStr;

/// Default tmux window-name prefix for kmux workspaces.
pub const DEFAULT_WINDOW_PREFIX: &str = "kmux-";
/// Default age after which completed sidebar rows switch to the idle style.
pub const DEFAULT_SIDEBAR_IDLE_AFTER_SECONDS: u64 = 30 * 60;

const DEFAULT_SIDEBAR_MIN_WIDTH: u16 = 36;
const DEFAULT_SIDEBAR_WIDTH_PERCENT: u16 = 20;
const DEFAULT_SIDEBAR_MAX_WIDTH: u16 = 52;

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
/// User-facing kmux configuration loaded from YAML.
pub struct Config {
    pub window_prefix: Option<String>,
    pub panes: Option<Vec<PaneConfig>>,
    pub post_create: Vec<String>,
    pub files: FileConfig,
    pub status_icons: StatusIcons,
    pub sidebar: SidebarConfig,
}

impl Config {
    /// Load the global kmux config, returning defaults when the file does not exist.
    pub fn load() -> Result<Self> {
        Self::load_from_path(Self::global_path()?)
    }

    /// Return the configured tmux window prefix or the project default.
    pub fn window_prefix(&self) -> &str {
        if let Some(prefix) = &self.window_prefix {
            prefix
        } else {
            DEFAULT_WINDOW_PREFIX
        }
    }

    /// Build the tmux window name for a kmux workspace slug.
    pub fn workspace_window_name(&self, workspace_slug: &str) -> String {
        format!("{}{}", self.window_prefix(), workspace_slug)
    }

    /// Return the XDG config-file path used for the global kmux YAML config.
    fn global_path() -> Result<PathBuf> {
        let base_dirs = BaseDirs::new().context("could not determine config directory")?;
        Ok(base_dirs.config_dir().join("kmux/config.yaml"))
    }

    /// Load and validate config from a specific path, treating a missing file as defaults.
    fn load_from_path(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(Self::default());
        }

        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config file {}", path.display()))?;
        Self::from_yaml_str(&content)
            .with_context(|| format!("failed to parse config file {}", path.display()))
    }

    /// Parse and validate config from YAML content.
    fn from_yaml_str(content: &str) -> Result<Self> {
        let config: Self = yaml_serde::from_str(content)?;
        config.validate()?;
        Ok(config)
    }

    /// Validate cross-field config rules that serde cannot express.
    fn validate(&self) -> Result<()> {
        self.status_icons.validate()?;
        self.sidebar.validate()?;

        for entry in self
            .files
            .copy_entries()
            .iter()
            .chain(self.files.symlink_entries())
        {
            file_entry_relative_path(entry)?;
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
/// Configured file operations applied to new worktrees.
pub struct FileConfig {
    pub copy: Option<Vec<String>>,
    pub symlink: Option<Vec<String>>,
}

impl FileConfig {
    /// Return configured files to copy into new worktrees.
    pub fn copy_entries(&self) -> &[String] {
        self.copy.as_deref().unwrap_or(&[])
    }

    /// Return configured files to symlink into new worktrees.
    pub fn symlink_entries(&self) -> &[String] {
        self.symlink.as_deref().unwrap_or(&[])
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
/// Icons used by status, list, and sidebar presentation.
pub struct StatusIcons {
    pub working: Option<String>,
    pub working_frames: Option<Vec<String>>,
    pub waiting: Option<String>,
    pub done: Option<String>,
    pub sleeping: Option<String>,
}

impl StatusIcons {
    /// Return the icon used for active agent work.
    pub fn working(&self) -> &str {
        self.working.as_deref().unwrap_or("🤖")
    }

    /// Return optional animation frames used while an agent is working.
    pub fn working_frames(&self) -> Option<&[String]> {
        self.working_frames.as_deref()
    }

    /// Return the icon used when an agent is waiting for input.
    pub fn waiting(&self) -> &str {
        self.waiting.as_deref().unwrap_or("💬")
    }

    /// Return the icon used when an agent reports finished work.
    pub fn done(&self) -> &str {
        self.done.as_deref().unwrap_or("✅")
    }

    /// Return the icon used when a sidebar row is idle long enough to sleep.
    pub fn sleeping(&self) -> &str {
        self.sleeping.as_deref().unwrap_or("💤")
    }

    fn validate(&self) -> Result<()> {
        let Some(frames) = &self.working_frames else {
            return Ok(());
        };
        if frames.is_empty() {
            bail!("status_icons.working_frames must not be empty");
        }

        let mut expected_width = None;
        for frame in frames {
            let width = UnicodeWidthStr::width(frame.as_str());
            if frame.trim().is_empty() || width == 0 {
                bail!("status_icons.working_frames must not contain blank frames");
            }
            if width > 2 {
                bail!("status_icons.working_frames frames must fit in the two-cell icon column");
            }
            if let Some(expected_width) = expected_width {
                if width != expected_width {
                    bail!("status_icons.working_frames frames must have equal display width");
                }
            } else {
                expected_width = Some(width);
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
/// Startup pane command for newly-created workspace windows.
pub struct PaneConfig {
    pub command: Option<String>,
    pub focus: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
/// Bounded sidebar sizing and idle-row behavior.
pub struct SidebarConfig {
    pub width: SidebarWidth,
    pub idle_after_seconds: Option<u64>,
}

impl SidebarConfig {
    /// Return the idle threshold for sidebar rows, falling back to the project default.
    pub fn idle_after_seconds(&self) -> u64 {
        self.idle_after_seconds
            .unwrap_or(DEFAULT_SIDEBAR_IDLE_AFTER_SECONDS)
    }

    fn validate(&self) -> Result<()> {
        self.width.validate()?;
        if self.idle_after_seconds == Some(0) {
            bail!("sidebar.idle_after_seconds must be greater than zero");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
/// Bounded proportional width policy for sidebar panes.
pub struct SidebarWidth {
    /// Minimum preferred width in terminal cells when the window can fit it.
    pub min: u16,
    /// Preferred width as a percentage of the total tmux window width.
    pub percent: u16,
    /// Maximum width in terminal cells.
    pub max: u16,
}

impl Default for SidebarWidth {
    fn default() -> Self {
        Self {
            min: DEFAULT_SIDEBAR_MIN_WIDTH,
            percent: DEFAULT_SIDEBAR_WIDTH_PERCENT,
            max: DEFAULT_SIDEBAR_MAX_WIDTH,
        }
    }
}

impl SidebarWidth {
    fn validate(&self) -> Result<()> {
        if self.min == 0 {
            bail!("sidebar.width.min must be greater than zero");
        }
        if !(1..=100).contains(&self.percent) {
            bail!("sidebar.width.percent must be between 1 and 100");
        }
        if self.min > self.max {
            bail!("sidebar.width.min must not exceed sidebar.width.max");
        }
        Ok(())
    }
}

/// Validate and normalize a configured file operation path as repo-relative.
pub fn file_entry_relative_path(entry: &str) -> Result<PathBuf> {
    let path = Path::new(entry);
    if entry.trim().is_empty()
        || path.is_absolute()
        || contains_current_dir_segment(entry)
        || path.components().any(|component| {
            matches!(
                component,
                Component::CurDir | Component::ParentDir | Component::Prefix(_)
            )
        })
    {
        bail!("configured file path must be relative and stay inside the repo: {entry}");
    }
    Ok(path.to_path_buf())
}

// `Path::components` does not preserve every explicit `.` on all platforms, so
// reject current-directory segments at the string level before normalizing.
fn contains_current_dir_segment(entry: &str) -> bool {
    entry.split('/').any(|segment| segment == ".")
        || (cfg!(windows) && entry.split('\\').any(|segment| segment == "."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_portable_kmux_window_prefix() {
        let config = Config::default();

        assert_eq!(config.window_prefix(), DEFAULT_WINDOW_PREFIX);
        assert_eq!(
            config.workspace_window_name("feature-auth"),
            "kmux-feature-auth"
        );
        assert_eq!(
            config.sidebar.idle_after_seconds(),
            DEFAULT_SIDEBAR_IDLE_AFTER_SECONDS
        );
    }

    #[test]
    fn explicit_window_prefix_wins_over_default() {
        let config = Config {
            window_prefix: Some("km-".to_string()),
            ..Config::default()
        };

        assert_eq!(
            config.workspace_window_name("feature-auth"),
            "km-feature-auth"
        );
    }

    #[test]
    fn parses_active_global_kmux_config_shape() {
        let config = Config::from_yaml_str(
            r#"
window_prefix: "git-"
panes:
  - command: nvim
    focus: true
status_icons:
  working: spin
  working_frames: ["⠋", "⠙", "⠹", "⠸"]
  waiting: wait
  done: done
  sleeping: sleep
post_create:
  - direnv allow
files:
  copy:
    - .envrc
    - .opencode
  symlink:
    - codebook.toml
sidebar:
  width: {min: 36, percent: 20, max: 52}
  idle_after_seconds: 900
"#,
        )
        .expect("active global config should parse");

        let panes = config.panes.as_ref().expect("panes should be parsed");

        assert_eq!(config.window_prefix(), "git-");
        assert_eq!(panes[0].command.as_deref(), Some("nvim"));
        assert!(panes[0].focus);
        assert_eq!(config.status_icons.working(), "spin");
        assert_eq!(
            config.status_icons.working_frames().expect("frames"),
            ["⠋", "⠙", "⠹", "⠸"]
        );
        assert_eq!(config.status_icons.sleeping(), "sleep");
        assert_eq!(config.post_create, ["direnv allow"]);
        assert_eq!(config.files.copy_entries(), [".envrc", ".opencode"]);
        assert_eq!(config.files.symlink_entries(), ["codebook.toml"]);
        assert_eq!(
            config.sidebar.width,
            SidebarWidth {
                min: 36,
                percent: 20,
                max: 52,
            }
        );
        assert_eq!(config.sidebar.idle_after_seconds(), 900);
    }

    #[test]
    fn rejects_unsupported_config_fields() {
        let error = Config::from_yaml_str("sandbox: {}\n")
            .expect_err("unsupported config field should fail");

        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn rejects_removed_base_branch_config_key() {
        let error = Config::from_yaml_str("base_branch: main\n")
            .expect_err("removed config field should fail");

        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn rejects_deferred_parent_branch_config_key() {
        let error = Config::from_yaml_str("parent_branch: main\n")
            .expect_err("deferred config field should fail");

        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn rejects_deferred_multi_pane_layout_fields() {
        let error = Config::from_yaml_str(
            r#"
panes:
  - split: horizontal
"#,
        )
        .expect_err("unsupported multi-pane field should fail");

        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn rejects_file_entries_that_target_repo_or_escape() {
        for entry in ["", ".", "./.envrc", "foo/./bar", "../secret", "/tmp/secret"] {
            let yaml = format!("files: {{copy: ['{entry}']}}\n");
            let error = Config::from_yaml_str(&yaml).expect_err("file entry should fail");

            assert!(error.to_string().contains("configured file path"));
        }
    }

    #[test]
    fn rejects_invalid_working_spinner_frames() {
        for yaml in [
            "status_icons: {working_frames: []}\n",
            "status_icons: {working_frames: ['']}\n",
            "status_icons: {working_frames: ['a', '🤖']}\n",
            "status_icons: {working_frames: ['abc']}\n",
        ] {
            let error = Config::from_yaml_str(yaml).expect_err("invalid frames should fail");
            assert!(error.to_string().contains("status_icons.working_frames"));
        }
    }

    #[test]
    fn rejects_invalid_sidebar_idle_threshold() {
        let error = Config::from_yaml_str("sidebar: {idle_after_seconds: 0}\n")
            .expect_err("zero idle threshold should fail");

        assert!(error.to_string().contains("sidebar.idle_after_seconds"));
    }

    #[test]
    fn rejects_removed_sidebar_selection_hooks() {
        let error = Config::from_yaml_str("sidebar: {selection_hooks: [{command: notify-send}]}\n")
            .expect_err("removed selection hooks should fail");

        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn omitted_sidebar_width_uses_complete_default_policy() {
        let config = Config::from_yaml_str("sidebar: {idle_after_seconds: 900}\n")
            .expect("sidebar config should parse");

        assert_eq!(config.sidebar.width, SidebarWidth::default());
    }

    #[test]
    fn rejects_invalid_sidebar_width_policy() {
        for (yaml, field) in [
            (
                "sidebar: {width: {min: 0, percent: 20, max: 52}}\n",
                "sidebar.width.min",
            ),
            (
                "sidebar: {width: {min: 36, percent: 0, max: 52}}\n",
                "sidebar.width.percent",
            ),
            (
                "sidebar: {width: {min: 36, percent: 101, max: 52}}\n",
                "sidebar.width.percent",
            ),
            (
                "sidebar: {width: {min: 53, percent: 20, max: 52}}\n",
                "sidebar.width.min",
            ),
        ] {
            let error = Config::from_yaml_str(yaml).expect_err("invalid width should fail");
            assert!(error.to_string().contains(field));
        }
    }

    #[test]
    fn rejects_legacy_sidebar_width_forms() {
        for yaml in [
            "sidebar: {width: 42}\n",
            "sidebar: {width: '42'}\n",
            "sidebar: {width: '15%'}\n",
        ] {
            Config::from_yaml_str(yaml).expect_err("legacy sidebar width should fail");
        }
    }

    #[test]
    fn rejects_partial_or_unknown_sidebar_width_fields() {
        for yaml in [
            "sidebar: {width: {min: 36, percent: 20}}\n",
            "sidebar: {width: {min: 36, percent: 20, max: 52, preferred: 40}}\n",
        ] {
            let error = Config::from_yaml_str(yaml).expect_err("invalid width shape should fail");
            assert!(
                error.to_string().contains("sidebar.width") || error.to_string().contains("field")
            );
        }
    }

    #[test]
    fn rejects_deferred_sidebar_fields() {
        for yaml in [
            "sidebar: {position: top}\n",
            "sidebar: {height: 10}\n",
            "sidebar: {layout: compact}\n",
        ] {
            let error =
                Config::from_yaml_str(yaml).expect_err("unsupported sidebar field should fail");

            assert!(error.to_string().contains("unknown field"));
        }
    }
}
