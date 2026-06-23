use anyhow::Result;

use crate::cli;

use super::context::load_repo_context;
use super::resolve::list_items;

pub(super) fn run(args: cli::JsonArgs) -> Result<()> {
    let repo = load_repo_context()?;
    let items = list_items(&repo)?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&items)?);
    } else {
        for item in items {
            let branch = item.branch.as_deref().unwrap_or("-");
            println!("{}\t{}\t{}", item.handle, branch, item.path);
        }
    }
    Ok(())
}
