use std::collections::BTreeMap;
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
    pub window: WindowConfig,
    pub post_create: Vec<String>,
    pub files: FileConfig,
    pub status_icons: StatusIcons,
    pub sidebar: SidebarConfig,
    launchers: BTreeMap<String, LauncherConfig>,
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

    /// Return a configured launcher by its exact user-facing name.
    pub fn launcher(&self, name: &str) -> Option<&LauncherConfig> {
        self.launchers.get(name)
    }

    /// Return the configured default launcher name and record, when selected.
    pub fn default_launcher(&self) -> Option<(&str, &LauncherConfig)> {
        let name = self.window.default_launcher()?;
        self.launcher(name).map(|launcher| (name, launcher))
    }

    /// Iterate configured launcher names in deterministic order.
    pub fn launcher_names(&self) -> impl Iterator<Item = &str> {
        self.launchers.keys().map(String::as_str)
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
        self.validate_launchers()?;

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

    fn validate_launchers(&self) -> Result<()> {
        for (name, launcher) in &self.launchers {
            if !valid_launcher_name(name) {
                bail!(
                    "launcher name {name:?} must contain lowercase alphanumeric segments separated by one '-' or '_'"
                );
            }
            launcher.validate(name)?;
        }

        if let Some(default_name) = self.window.default_launcher() {
            if !valid_launcher_name(default_name) {
                bail!(
                    "window.default_launcher {default_name:?} must contain lowercase alphanumeric segments separated by one '-' or '_'"
                );
            }
            if !self.launchers.contains_key(default_name) {
                bail!("window.default_launcher references unknown launcher {default_name:?}");
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
/// Startup policy for newly-created workspace windows.
pub struct WindowConfig {
    default_launcher: Option<String>,
}

impl WindowConfig {
    /// Return the configured default launcher name, when one is selected.
    pub fn default_launcher(&self) -> Option<&str> {
        self.default_launcher.as_deref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
/// Exact executable and static arguments for one named workspace launcher.
pub struct LauncherConfig {
    command: String,
    #[serde(default)]
    args: Vec<String>,
}

impl LauncherConfig {
    /// Return the launcher executable exactly as configured.
    pub fn command(&self) -> &str {
        &self.command
    }

    /// Return the launcher's ordered static arguments exactly as configured.
    pub fn args(&self) -> &[String] {
        &self.args
    }

    fn validate(&self, name: &str) -> Result<()> {
        if self.command.trim().is_empty() {
            bail!("launchers.{name}.command must not be blank");
        }
        if self.command.contains('\0') {
            bail!("launchers.{name}.command must not contain NUL");
        }
        if let Some(index) = self
            .args
            .iter()
            .position(|argument| argument.contains('\0'))
        {
            bail!("launchers.{name}.args[{index}] must not contain NUL");
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

// Launcher names are deliberately ASCII and segment-based so they remain
// predictable in config, CLI input, and future shell completion candidates.
fn valid_launcher_name(name: &str) -> bool {
    let mut segment_has_character = false;
    for character in name.chars() {
        if character.is_ascii_lowercase() || character.is_ascii_digit() {
            segment_has_character = true;
        } else if matches!(character, '-' | '_') && segment_has_character {
            segment_has_character = false;
        } else {
            return false;
        }
    }
    segment_has_character
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
window:
  default_launcher: editor
launchers:
  editor:
    command: nvim
    args: ["--clean", ""]
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

        assert_eq!(config.window_prefix(), "git-");
        let (name, launcher) = config.default_launcher().expect("default launcher");
        assert_eq!(name, "editor");
        assert_eq!(launcher.command(), "nvim");
        assert_eq!(launcher.args(), ["--clean", ""]);
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
    fn parses_multiple_launchers_with_exact_argv() {
        let config = Config::from_yaml_str(
            r#"
launchers:
  example-launcher:
    command: "  executable with spaces  "
    args: ["", " two words ", "'quoted'", "--leading", "*.rs", ">file"]
  agent_2:
    command: /opt/example/bin/agent
"#,
        )
        .expect("valid launchers should parse");

        let launcher = config.launcher("example-launcher").expect("launcher");
        assert_eq!(launcher.command(), "  executable with spaces  ");
        assert_eq!(
            launcher.args(),
            ["", " two words ", "'quoted'", "--leading", "*.rs", ">file"]
        );
        assert!(
            config
                .launcher("agent_2")
                .expect("launcher")
                .args()
                .is_empty()
        );
        assert!(config.default_launcher().is_none());
        assert_eq!(
            config.launcher_names().collect::<Vec<_>>(),
            ["agent_2", "example-launcher"]
        );
    }

    #[test]
    fn launcher_name_grammar_accepts_only_lowercase_segments() {
        for valid in ["a", "agent2", "example-launcher", "agent_2", "a-b_c3"] {
            let yaml = format!("launchers: {{{valid}: {{command: example}}}}\n");
            Config::from_yaml_str(&yaml).expect("valid launcher name should parse");
        }

        for invalid in [
            "",
            "-agent",
            "agent-",
            "_agent",
            "agent_",
            "agent--two",
            "agent__two",
            "agent-_two",
            "Agent",
            "agent.two",
            "agent two",
            "é",
        ] {
            let yaml = format!("launchers: {{'{invalid}': {{command: example}}}}\n");
            let error = Config::from_yaml_str(&yaml).expect_err("invalid name should fail");
            assert!(error.to_string().contains("launcher name"));
        }
    }

    #[test]
    fn validates_launcher_fields_and_default_reference() {
        for (yaml, field) in [
            (
                "launchers: {editor: {command: '  '}}\n",
                "launchers.editor.command",
            ),
            (
                "launchers: {editor: {command: \"example\\0command\"}}\n",
                "launchers.editor.command",
            ),
            (
                "launchers: {editor: {command: example, args: [\"arg\\0value\"]}}\n",
                "launchers.editor.args[0]",
            ),
            (
                "window: {default_launcher: missing}\nlaunchers: {editor: {command: example}}\n",
                "window.default_launcher",
            ),
        ] {
            let error = Config::from_yaml_str(yaml).expect_err("invalid launcher should fail");
            assert!(error.to_string().contains(field), "{error:#}");
        }
    }

    #[test]
    fn missing_launcher_configuration_means_shell_startup() {
        for yaml in ["", "window: {}\n", "launchers: {}\n"] {
            let config = Config::from_yaml_str(yaml).expect("shell-only config should parse");
            assert!(config.default_launcher().is_none());
            assert!(config.launcher("missing").is_none());
        }
    }

    #[test]
    fn rejects_unknown_launcher_fields() {
        let nested = Config::from_yaml_str("launchers: {editor: {command: nvim, focus: true}}\n")
            .expect_err("unknown launcher field should fail");
        assert!(nested.to_string().contains("unknown field"));
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
}
