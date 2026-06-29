use anyhow::{Result, bail};

pub fn slugify(input: &str) -> String {
    let mut output = String::new();
    let mut last_was_separator = false;

    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            output.push(ch.to_ascii_lowercase());
            last_was_separator = false;
        } else if !output.is_empty() && !last_was_separator {
            output.push('-');
            last_was_separator = true;
        }
    }

    output.trim_matches('-').to_string()
}

pub fn workspace_slug_from_branch(branch_name: &str) -> Result<String> {
    let slug = slugify(branch_name);

    if slug.is_empty() {
        bail!("workspace slug cannot be empty");
    }

    Ok(slug)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugifies_branch_names_for_filesystem_and_tmux() {
        assert_eq!(slugify("feature/auth/oauth"), "feature-auth-oauth");
        assert_eq!(slugify("Feature! @#$%"), "feature");
        assert_eq!(slugify("  My Cool Feature  "), "my-cool-feature");
        assert_eq!(slugify("../feature auth"), "feature-auth");
    }

    #[test]
    fn slugifies_to_ascii_only_workspace_slugs() {
        assert_eq!(slugify("fèature/åuth"), "f-ature-uth");
        assert_eq!(slugify("火花"), "");
    }

    #[test]
    fn derives_workspace_slug_from_full_branch() {
        let slug = workspace_slug_from_branch("prj-4120/create-new-tags")
            .expect("workspace slug should be derived");

        assert_eq!(slug, "prj-4120-create-new-tags");
    }

    #[test]
    fn rejects_empty_workspace_slugs() {
        let error = workspace_slug_from_branch("!!!").expect_err("empty slug should fail");

        assert!(error.to_string().contains("empty"));
    }
}
