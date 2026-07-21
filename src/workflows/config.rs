use std::collections::BTreeMap;

use anyhow::Result;
use serde::Serialize;

use crate::cli;
use crate::config::{Config, LauncherConfig};

/// Print the active configuration using a stable, fully-resolved output shape.
pub(super) fn run(args: cli::ConfigArgs) -> Result<()> {
    let config = Config::load()?;
    let output = ActiveConfig::from(&config);
    if args.json {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        print!("{}", yaml_serde::to_string(&output)?);
    }
    Ok(())
}

#[derive(Serialize)]
struct ActiveConfig<'a> {
    window_prefix: &'a str,
    window: ActiveWindow<'a>,
    launchers: BTreeMap<&'a str, ActiveLauncher<'a>>,
    post_create: &'a [String],
    files: ActiveFiles<'a>,
    status_icons: ActiveStatusIcons<'a>,
    sidebar: ActiveSidebar,
}

impl<'a> From<&'a Config> for ActiveConfig<'a> {
    fn from(config: &'a Config) -> Self {
        Self {
            window_prefix: config.window_prefix(),
            window: ActiveWindow {
                default_launcher: config.window.default_launcher(),
            },
            launchers: config
                .launchers()
                .map(|(name, launcher)| (name, ActiveLauncher::from(launcher)))
                .collect(),
            post_create: &config.post_create,
            files: ActiveFiles {
                copy: config.files.copy_entries(),
                symlink: config.files.symlink_entries(),
            },
            status_icons: ActiveStatusIcons {
                working: config.status_icons.working(),
                working_frames: config.status_icons.working_frames(),
                waiting: config.status_icons.waiting(),
                done: config.status_icons.done(),
                sleeping: config.status_icons.sleeping(),
            },
            sidebar: ActiveSidebar {
                width: ActiveSidebarWidth {
                    min: config.sidebar.width.min,
                    percent: config.sidebar.width.percent,
                    max: config.sidebar.width.max,
                },
                idle_after_seconds: config.sidebar.idle_after_seconds(),
            },
        }
    }
}

#[derive(Serialize)]
struct ActiveWindow<'a> {
    default_launcher: Option<&'a str>,
}

#[derive(Serialize)]
struct ActiveLauncher<'a> {
    description: Option<&'a str>,
    command: &'a str,
    args: &'a [String],
}

impl<'a> From<&'a LauncherConfig> for ActiveLauncher<'a> {
    fn from(launcher: &'a LauncherConfig) -> Self {
        Self {
            description: launcher.description(),
            command: launcher.command(),
            args: launcher.args(),
        }
    }
}

#[derive(Serialize)]
struct ActiveFiles<'a> {
    copy: &'a [String],
    symlink: &'a [String],
}

#[derive(Serialize)]
struct ActiveStatusIcons<'a> {
    working: &'a str,
    working_frames: Option<&'a [String]>,
    waiting: &'a str,
    done: &'a str,
    sleeping: &'a str,
}

#[derive(Serialize)]
struct ActiveSidebar {
    width: ActiveSidebarWidth,
    idle_after_seconds: u64,
}

#[derive(Serialize)]
struct ActiveSidebarWidth {
    min: u16,
    percent: u16,
    max: u16,
}
