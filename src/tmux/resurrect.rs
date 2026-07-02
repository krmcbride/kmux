use std::fs;
use std::path::PathBuf;

use anyhow::Result;

use super::Tmux;

const RESURRECT_DIR_OPTION: &str = "@resurrect-dir";

#[derive(Debug, Clone, PartialEq, Eq)]
/// Pane identity and process hints from tmux-resurrect's last saved environment.
pub struct TmuxResurrectPane {
    pub session_name: String,
    pub window_index: String,
    pub pane_index: String,
    pub title: String,
    pub current_command: String,
}

impl Tmux {
    /// Return pane records from tmux-resurrect's `last` save file when present.
    ///
    /// tmux-resurrect saves explicit tab-delimited pane/window fields rather than
    /// arbitrary tmux user options. This lets callers recover panes that can be
    /// recognized from saved title or command data after `@kmux_*` options are gone.
    pub fn resurrect_saved_panes(&self) -> Result<Vec<TmuxResurrectPane>> {
        let Some(last_file) = resurrect_last_file(self)? else {
            return Ok(Vec::new());
        };
        let contents = fs::read_to_string(last_file)?;
        Ok(parse_resurrect_panes(&contents))
    }
}

// Upstream keeps `last` as a symlink to the latest save; resolve relative
// symlink targets against the resurrect directory, and also accept a regular
// file for tests or hand-written state.
fn resurrect_last_file(tmux: &Tmux) -> Result<Option<PathBuf>> {
    let Some(dir) = resurrect_dir(tmux)? else {
        return Ok(None);
    };
    let last = dir.join("last");
    if !last.exists() {
        return Ok(None);
    }
    Ok(Some(fs::read_link(&last).map_or(last, |target| {
        if target.is_absolute() {
            target
        } else {
            dir.join(target)
        }
    })))
}

// Match tmux-resurrect's save directory lookup: explicit `@resurrect-dir`, then
// the legacy location, then the XDG data path used by current upstream versions.
fn resurrect_dir(tmux: &Tmux) -> Result<Option<PathBuf>> {
    let output = tmux.output(["show-option", "-gqv", RESURRECT_DIR_OPTION])?;
    if output.status.success() {
        let value = output.stdout.trim_end();
        if !value.is_empty() {
            return Ok(Some(expand_resurrect_path(value)));
        }
    }

    let Some(home) = std::env::var_os("HOME") else {
        return Ok(None);
    };
    let home = PathBuf::from(home);
    let legacy_dir = home.join(".tmux/resurrect");
    if legacy_dir.is_dir() {
        return Ok(Some(legacy_dir));
    }

    Ok(Some(
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"))
            .join("tmux/resurrect"),
    ))
}

// tmux-resurrect documents limited path expansion for `@resurrect-dir`; this
// mirrors the cases kmux needs when locating the plugin's latest save file.
fn expand_resurrect_path(value: &str) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    if value == "~" {
        return PathBuf::from(home);
    }
    if let Some(rest) = value.strip_prefix("~/") {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(value.replace("$HOME", &home))
}

// Column indexes mirror tmux-resurrect's `save.sh` pane rows, which are
// tab-delimited records beginning with the literal `pane`.
fn parse_resurrect_panes(contents: &str) -> Vec<TmuxResurrectPane> {
    contents.lines().filter_map(parse_resurrect_pane).collect()
}

fn parse_resurrect_pane(line: &str) -> Option<TmuxResurrectPane> {
    let fields = line.split('\t').collect::<Vec<_>>();
    if fields.first().copied() != Some("pane") || fields.len() < 10 {
        return None;
    }

    Some(TmuxResurrectPane {
        session_name: fields[1].to_owned(),
        window_index: fields[2].to_owned(),
        pane_index: fields[5].to_owned(),
        title: fields[6].to_owned(),
        current_command: fields[9].to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_resurrect_pane_records_from_saved_environment() {
        let panes = parse_resurrect_panes(
            "pane\tproject\t1\t1\t:*\t1\tkmux\t:/repo\t0\tkmux\t:\n\
             pane\tproject\t1\t1\t:*\t2\tfish\t:/repo\t1\tfish\t:\n\
             window\tproject\t1\t:main\t1\t:*\tlayout\t:\n",
        );

        assert_eq!(
            panes,
            vec![
                TmuxResurrectPane {
                    session_name: "project".to_owned(),
                    window_index: "1".to_owned(),
                    pane_index: "1".to_owned(),
                    title: "kmux".to_owned(),
                    current_command: "kmux".to_owned(),
                },
                TmuxResurrectPane {
                    session_name: "project".to_owned(),
                    window_index: "1".to_owned(),
                    pane_index: "2".to_owned(),
                    title: "fish".to_owned(),
                    current_command: "fish".to_owned(),
                },
            ]
        );
    }
}
