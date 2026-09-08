//! Launcher selection and caller-input resolution for workspace workflows.

use std::io::Read;

use anyhow::{Context, Result, bail};

use crate::cli;
use crate::config::Config;
use crate::launcher::ResolvedLauncher;

/// Resolve create's explicit/default launcher policy and caller-owned input.
pub(super) fn resolve_create(
    config: &Config,
    args: &cli::CreateArgs,
) -> Result<Option<ResolvedLauncher>> {
    resolve_launcher(
        config,
        args.launcher.as_deref(),
        args.launcher_input.as_deref(),
    )
}

/// Resolve an explicit/default launcher and validate transient caller input before effects.
pub(super) fn resolve_launcher(
    config: &Config,
    launcher: Option<&str>,
    input: Option<&str>,
) -> Result<Option<ResolvedLauncher>> {
    if input.is_some() && launcher.is_none() {
        bail!("--launcher-input requires --launcher");
    }
    let input = resolve_launcher_input(input)?;
    let selected = if let Some(name) = launcher {
        let launcher = config
            .launcher(name)
            .ok_or_else(|| anyhow::anyhow!("unknown launcher {name:?}"))?;
        Some((name, launcher))
    } else {
        config.default_launcher()
    };

    Ok(selected.map(|(name, launcher)| ResolvedLauncher::from_config(name, launcher, input)))
}

/// Resolve restore's current default launcher without one-shot input.
pub(super) fn resolve_default(config: &Config) -> Option<ResolvedLauncher> {
    config
        .default_launcher()
        .map(|(name, launcher)| ResolvedLauncher::from_config(name, launcher, None))
}

fn resolve_launcher_input(input: Option<&str>) -> Result<Option<String>> {
    let input = match input {
        Some("-") => {
            let mut bytes = Vec::new();
            std::io::stdin()
                .lock()
                .read_to_end(&mut bytes)
                .context("failed to read launcher input from stdin")?;
            Some(
                String::from_utf8(bytes)
                    .context("launcher input from stdin must be valid UTF-8")?,
            )
        }
        Some(input) => Some(input.to_owned()),
        None => None,
    };
    // OS process arguments cannot represent embedded NUL. Reject it during
    // preflight instead of discovering it at spawn after workspace mutation.
    if input.as_ref().is_some_and(|input| input.contains('\0')) {
        bail!("launcher input must not contain NUL");
    }
    Ok(input)
}
