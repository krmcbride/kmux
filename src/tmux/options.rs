//! Namespaced tmux user-option and global-hook operations.

use anyhow::{Result, bail};

use super::process::Tmux;

impl Tmux {
    /// Set a namespaced tmux window user option on a target.
    pub fn set_window_option(&self, target: &str, option_name: &str, value: &str) -> Result<()> {
        validate_user_option(option_name)?;
        self.stdout(["set-option", "-w", "-t", target, option_name, value])?;
        Ok(())
    }

    /// Read a namespaced tmux window user option, returning `None` when unset or blank.
    pub fn show_window_option(&self, target: &str, option_name: &str) -> Result<Option<String>> {
        validate_user_option(option_name)?;
        let output = self.output(["show-option", "-wqv", "-t", target, option_name])?;
        if !output.status.success() {
            return Ok(None);
        }

        Ok(Some(output.stdout.trim_end().to_owned()).filter(|value| !value.is_empty()))
    }

    /// Unset a namespaced tmux window user option on a target.
    pub fn unset_window_option(&self, target: &str, option_name: &str) -> Result<()> {
        validate_user_option(option_name)?;
        self.stdout(["set-option", "-uw", "-t", target, option_name])?;
        Ok(())
    }

    /// Set a namespaced tmux pane user option on a target.
    pub fn set_pane_option(&self, target: &str, option_name: &str, value: &str) -> Result<()> {
        validate_user_option(option_name)?;
        self.stdout(["set-option", "-p", "-t", target, option_name, value])?;
        Ok(())
    }

    /// Set a namespaced global tmux user option.
    pub fn set_global_option(&self, option_name: &str, value: &str) -> Result<()> {
        validate_user_option(option_name)?;
        self.stdout(["set-option", "-g", option_name, value])?;
        Ok(())
    }

    /// Read a namespaced global tmux user option, returning `None` when unset or blank.
    pub fn show_global_option(&self, option_name: &str) -> Result<Option<String>> {
        validate_user_option(option_name)?;
        let output = self.output(["show-option", "-gqv", option_name])?;
        if !output.status.success() {
            return Ok(None);
        }

        Ok(Some(output.stdout.trim_end().to_owned()).filter(|value| !value.is_empty()))
    }

    /// Unset a namespaced global tmux user option.
    pub fn unset_global_option(&self, option_name: &str) -> Result<()> {
        validate_user_option(option_name)?;
        self.stdout(["set-option", "-gu", option_name])?;
        Ok(())
    }

    /// Set a global tmux hook command.
    pub fn set_hook(&self, hook: &str, command: &str) -> Result<()> {
        self.stdout(["set-hook", "-g", hook, command])?;
        Ok(())
    }

    /// Unset a global tmux hook command.
    pub fn unset_hook(&self, hook: &str) -> Result<()> {
        self.stdout(["set-hook", "-gu", hook])?;
        Ok(())
    }
}

// Restrict user options to kmux-owned names so generic tmux options cannot be
// mutated through this adapter by accident.
fn validate_user_option(option_name: &str) -> Result<()> {
    if !option_name.starts_with("@kmux") {
        bail!("tmux user option must be namespaced under @kmux, got '{option_name}'");
    }
    if !option_name.chars().all(is_user_option_char) {
        bail!("tmux user option contains unsupported characters: '{option_name}'");
    }
    Ok(())
}

fn is_user_option_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '@' | '.' | '_' | '-')
}
