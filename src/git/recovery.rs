//! Verified recovery refs outside the branch namespace, with compare-and-create semantics.

use anyhow::{Context, Result, bail};

use super::Git;
use super::process::bail_git;

impl Git {
    /// Protect a commit under a deterministic workspace/commit name without overwriting refs.
    /// Existing direct refs to the same commit are reusable; conflicting or symbolic refs
    /// receive an increasing suffix. Any create or verification failure preserves the caller's worktree.
    pub fn create_recovery_ref(&self, workspace_id: &str, commit: &str) -> Result<String> {
        validate_recovery_identity(workspace_id, commit)?;
        let zero = "0".repeat(commit.len());
        for collision in 0..100 {
            let reference = recovery_name(workspace_id, commit, collision);
            match self.direct_ref(&reference)? {
                Some((existing, false)) if existing == commit => {
                    self.verify_direct_ref(&reference, commit)?;
                    return Ok(reference);
                }
                Some(_) => continue,
                None => {}
            }
            let output = self.output(["update-ref", "--no-deref", &reference, commit, &zero])?;
            if !output.status.success() {
                // A racing creator can occupy this exact name; inspect it on the next attempt.
                if self.direct_ref(&reference)?.is_some() {
                    continue;
                }
                return bail_git(output)
                    .context("failed to create recovery ref; workspace remains intact");
            }
            self.verify_direct_ref(&reference, commit)
                .with_context(|| {
                    format!(
                        "recovery ref '{reference}' could not be verified; workspace remains intact"
                    )
                })?;
            return Ok(reference);
        }
        bail!("recovery ref names are occupied; workspace remains intact")
    }

    /// Establish committed HEAD reachability through a ref that removal will preserve.
    pub fn verify_preserved_branch(&self, branch: &str, commit: &str) -> Result<()> {
        self.verify_direct_ref(&format!("refs/heads/{branch}"), commit)
            .with_context(|| {
                format!("branch '{branch}' no longer protects the observed HEAD; retry removal")
            })
    }

    fn verify_direct_ref(&self, reference: &str, commit: &str) -> Result<()> {
        if self.direct_ref(reference)? != Some((commit.to_owned(), false)) {
            bail!("ref '{reference}' does not directly point to expected commit '{commit}'");
        }
        // Verify that the object is still readable as a commit, not merely a ref value.
        if self.resolve_commit(reference)? != commit {
            bail!("ref '{reference}' resolved to a different commit");
        }
        Ok(())
    }

    // Ref names cannot contain whitespace or NUL. Exact matching excludes child refs
    // from for-each-ref's prefix matching, and symref evidence prevents false durability.
    fn direct_ref(&self, reference: &str) -> Result<Option<(String, bool)>> {
        let output = self.stdout([
            "for-each-ref",
            "--format=%(refname)%00%(objectname)%00%(symref)",
            reference,
        ])?;
        for line in output.lines() {
            let fields = line.split('\0').collect::<Vec<_>>();
            if fields.len() != 3 {
                bail!("malformed Git ref inventory");
            }
            if fields[0] == reference {
                return Ok(Some((fields[1].to_owned(), !fields[2].is_empty())));
            }
        }
        Ok(None)
    }
}

fn validate_recovery_identity(workspace_id: &str, commit: &str) -> Result<()> {
    if !workspace_id.starts_with("ws-")
        || workspace_id.len() <= 3
        || !workspace_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        || !matches!(commit.len(), 40 | 64)
        || !commit.bytes().all(|b| b.is_ascii_hexdigit())
    {
        bail!("recovery requires a stable workspace ID and full Git object ID");
    }
    Ok(())
}

fn recovery_name(workspace_id: &str, commit: &str, collision: usize) -> String {
    let base = format!("refs/kmux/recovery/{workspace_id}/{commit}");
    if collision == 0 {
        base
    } else {
        format!("{base}-{collision}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_names_are_deterministic_outside_branch_space_and_reject_ref_injection() -> Result<()>
    {
        let commit = "a".repeat(40);
        validate_recovery_identity("ws-example", &commit)?;
        assert_eq!(
            recovery_name("ws-example", &commit, 0),
            format!("refs/kmux/recovery/ws-example/{commit}")
        );
        assert_eq!(
            recovery_name("ws-example", &commit, 1),
            format!("refs/kmux/recovery/ws-example/{commit}-1")
        );
        assert!(validate_recovery_identity("ws-example/other", &commit).is_err());
        assert!(validate_recovery_identity("ws-example", "HEAD").is_err());
        Ok(())
    }
}
