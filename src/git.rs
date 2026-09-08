//! Git subprocess adapter for repository and worktree operations.
//!
//! This module is kmux's boundary to the Git CLI. It keeps porcelain parsing,
//! ref checks, branch/worktree mutations, and Git common-dir discovery out of
//! workflow code so command use cases can reason in kmux terms.

mod branches;
mod process;
mod registration;
mod repository;
mod worktrees;

// Preserve crate-visible consumer imports even when a compilation target does
// not name every operation result type directly.
#[allow(unused_imports)]
pub use branches::{BranchAction, RemoteBranch};
pub use process::Git;
#[allow(unused_imports)]
pub use repository::RepoInfo;
pub use worktrees::WorktreeInfo;

#[cfg(feature = "internal-adapter-contract-tests")]
/// Crate-wide exception: path and agent adapter contracts need the same owned
/// Git environment, and no narrower Rust visibility spans those sibling modules.
pub(crate) mod contract_support;

#[cfg(feature = "internal-adapter-contract-tests")]
pub mod contract_tests;
