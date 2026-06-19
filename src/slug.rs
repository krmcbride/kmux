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

pub fn derive_handle(branch_name: &str, explicit_name: Option<&str>) -> Result<String> {
    let handle = if let Some(name) = explicit_name {
        slugify(name)
    } else {
        slugify(branch_name)
    };

    if handle.is_empty() {
        bail!("handle cannot be empty");
    }

    Ok(handle)
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
    fn slugifies_to_ascii_only_handles() {
        assert_eq!(slugify("fèature/åuth"), "f-ature-uth");
        assert_eq!(slugify("火花"), "");
    }

    #[test]
    fn derives_handle_from_full_branch() {
        let handle =
            derive_handle("prj-4120/create-new-tags", None).expect("handle should be derived");

        assert_eq!(handle, "prj-4120-create-new-tags");
    }

    #[test]
    fn explicit_name_overrides_branch_name() {
        let handle = derive_handle("feature/auth", Some("Custom Name"))
            .expect("explicit handle should be derived");

        assert_eq!(handle, "custom-name");
    }

    #[test]
    fn rejects_empty_handles() {
        let error = derive_handle("feature", Some("!!!")).expect_err("empty handle should fail");

        assert!(error.to_string().contains("empty"));
    }
}
