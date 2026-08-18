//! Workflow-resolved launcher data ready for one workspace window.

use crate::config::LauncherConfig;

/// A validated, in-memory launcher choice ready for one workspace window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLauncher {
    name: String,
    executable: String,
    static_args: Vec<String>,
    input: Option<String>,
}

impl ResolvedLauncher {
    /// Resolve one validated config record while preserving its exact argv data.
    pub fn from_config(name: &str, config: &LauncherConfig, input: Option<String>) -> Self {
        Self {
            name: name.to_owned(),
            executable: config.command().to_owned(),
            static_args: config.args().to_vec(),
            input,
        }
    }

    /// Return the user-facing launcher name used in sanitized workflow errors.
    pub fn name(&self) -> &str {
        &self.name
    }

    pub(super) fn executable(&self) -> &str {
        &self.executable
    }

    pub(super) fn static_args(&self) -> &[String] {
        &self.static_args
    }

    pub(super) fn input(&self) -> Option<&str> {
        self.input.as_deref()
    }

    #[cfg(any(test, feature = "internal-adapter-contract-tests"))]
    pub(super) fn for_test(
        executable: impl Into<String>,
        args: &[&str],
        input: Option<&str>,
    ) -> Self {
        Self {
            name: "example-launcher".to_owned(),
            executable: executable.into(),
            static_args: args.iter().map(|argument| (*argument).to_owned()).collect(),
            input: input.map(str::to_owned),
        }
    }
}
