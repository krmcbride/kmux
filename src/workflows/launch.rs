//! Launcher selection and caller-input resolution for workspace workflows.

use std::io::Read;

use anyhow::{Context, Result, bail};

use crate::cli;
use crate::config::Config;
use crate::launcher::ResolvedLauncher;

/// Resolve add's explicit/default launcher policy and caller-owned input.
pub(super) fn resolve_add(
    config: &Config,
    args: &cli::AddArgs,
) -> Result<Option<ResolvedLauncher>> {
    let input = resolve_input(args)?;
    let selected = if let Some(name) = args.launch.as_deref() {
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

fn resolve_input(args: &cli::AddArgs) -> Result<Option<String>> {
    if args.input.is_some() && args.launch.is_none() {
        bail!("--input requires --launch");
    }

    let input = match args.input.as_deref() {
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
