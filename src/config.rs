use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use directories::BaseDirs;
use serde::Deserialize;

pub const DEFAULT_WINDOW_PREFIX: &str = "kmux-";

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub base_branch: Option<String>,
    pub window_prefix: Option<String>,
    pub panes: Option<Vec<PaneConfig>>,
    pub post_create: Vec<String>,
    pub files: FileConfig,
    pub status_icons: StatusIcons,
    pub sidebar: SidebarConfig,
}

impl Config {
    pub fn load() -> Result<Self> {
        Self::load_from_path(Self::global_path()?)
    }

    pub fn global_path() -> Result<PathBuf> {
        let base_dirs = BaseDirs::new().context("could not determine config directory")?;
        Ok(base_dirs.config_dir().join("kmux/config.yaml"))
    }

    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(Self::default());
        }

        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config file {}", path.display()))?;
        Self::from_yaml_str(&content)
            .with_context(|| format!("failed to parse config file {}", path.display()))
    }

    pub fn from_yaml_str(content: &str) -> Result<Self> {
        let config: Self = serde_yaml::from_str(content)?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        if let Some(layout) = &self.sidebar.layout {
            match layout.as_str() {
                "compact" | "tiles" => {}
                _ => bail!("sidebar.layout must be 'compact' or 'tiles', got '{layout}'"),
            }
        }

        Ok(())
    }

    pub fn window_prefix(&self) -> &str {
        if let Some(prefix) = &self.window_prefix {
            prefix
        } else {
            DEFAULT_WINDOW_PREFIX
        }
    }

    pub fn window_name(&self, handle: &str) -> String {
        format!("{}{}", self.window_prefix(), handle)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FileConfig {
    pub copy: Option<Vec<String>>,
    pub symlink: Option<Vec<String>>,
}

impl FileConfig {
    pub fn copy_entries(&self) -> &[String] {
        self.copy.as_deref().unwrap_or(&[])
    }

    pub fn symlink_entries(&self) -> &[String] {
        self.symlink.as_deref().unwrap_or(&[])
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StatusIcons {
    pub working: Option<String>,
    pub waiting: Option<String>,
    pub done: Option<String>,
}

impl StatusIcons {
    pub fn working(&self) -> &str {
        self.working.as_deref().unwrap_or("🤖")
    }

    pub fn waiting(&self) -> &str {
        self.waiting.as_deref().unwrap_or("💬")
    }

    pub fn done(&self) -> &str {
        self.done.as_deref().unwrap_or("✅")
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PaneConfig {
    pub command: Option<String>,
    pub focus: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SidebarConfig {
    pub width: Option<SidebarSize>,
    pub height: Option<SidebarSize>,
    pub layout: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarSize {
    Absolute(u16),
    Percent(u16),
}

impl SidebarSize {
    pub fn resolve(self, total: u16) -> u16 {
        match self {
            Self::Absolute(value) => value,
            Self::Percent(percent) => total * percent / 100,
        }
    }
}

impl<'de> Deserialize<'de> for SidebarSize {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de;

        struct Visitor;

        impl de::Visitor<'_> for Visitor {
            type Value = SidebarSize;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a number or percentage string like '15%'")
            }

            fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
                let value = u16::try_from(value)
                    .map_err(|_| de::Error::custom("value is too large for u16"))?;
                Ok(SidebarSize::Absolute(value))
            }

            fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
                if value < 0 {
                    return Err(de::Error::custom("value cannot be negative"));
                }
                self.visit_u64(value as u64)
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                if let Some(percent) = value.trim().strip_suffix('%') {
                    let percent = percent
                        .trim()
                        .parse::<u16>()
                        .map_err(|_| de::Error::custom("invalid percentage"))?;
                    if !(1..=100).contains(&percent) {
                        return Err(de::Error::custom("percentage must be 1-100"));
                    }
                    return Ok(SidebarSize::Percent(percent));
                }

                let value = value
                    .trim()
                    .parse::<u16>()
                    .map_err(|_| de::Error::custom("invalid numeric value"))?;
                Ok(SidebarSize::Absolute(value))
            }
        }

        deserializer.deserialize_any(Visitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_portable_kmux_window_prefix() {
        let config = Config::default();

        assert_eq!(config.window_prefix(), DEFAULT_WINDOW_PREFIX);
        assert_eq!(config.window_name("feature-auth"), "kmux-feature-auth");
    }

    #[test]
    fn explicit_window_prefix_wins_over_default() {
        let config = Config {
            window_prefix: Some("km-".to_string()),
            ..Config::default()
        };

        assert_eq!(config.window_name("feature-auth"), "km-feature-auth");
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
  waiting: wait
  done: done
post_create:
  - direnv allow
files:
  copy:
    - .envrc
    - .opencode
  symlink:
    - codebook.toml
sidebar: {width: 42}
"#,
        )
        .expect("active global config should parse");

        let panes = config.panes.as_ref().expect("panes should be parsed");

        assert_eq!(config.window_prefix(), "git-");
        assert_eq!(panes[0].command.as_deref(), Some("nvim"));
        assert!(panes[0].focus);
        assert_eq!(config.status_icons.working(), "spin");
        assert_eq!(config.post_create, ["direnv allow"]);
        assert_eq!(config.files.copy_entries(), [".envrc", ".opencode"]);
        assert_eq!(config.files.symlink_entries(), ["codebook.toml"]);
        assert_eq!(config.sidebar.width, Some(SidebarSize::Absolute(42)));
    }

    #[test]
    fn rejects_unsupported_config_fields() {
        let error = Config::from_yaml_str("sandbox: {}\n")
            .expect_err("unsupported config field should fail");

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
    fn parses_sidebar_percentage_size() {
        let config = Config::from_yaml_str("sidebar: {width: '15%'}\n")
            .expect("sidebar config should parse");
        let width = config
            .sidebar
            .width
            .expect("sidebar width should be parsed");

        assert_eq!(width.resolve(200), 30);
    }

    #[test]
    fn rejects_deferred_sidebar_position_field() {
        let error = Config::from_yaml_str("sidebar: {position: top}\n")
            .expect_err("unsupported sidebar position should fail");

        assert!(error.to_string().contains("unknown field"));
    }
}
