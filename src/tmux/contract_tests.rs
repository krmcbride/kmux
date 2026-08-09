//! Process-backed contracts for the concrete tmux adapter.

use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use tempfile::TempDir;

use super::contract_support::{TmuxFixture, create_test_session};

pub fn creates_selects_lists_and_kills_windows_on_isolated_socket() -> Result<()> {
    let fixture = TmuxFixture::new()?;
    let temp = TempDir::new()?;
    let tmux = &fixture.tmux;

    create_test_session(tmux, "project", temp.path())?;
    create_test_session(tmux, "project-copy", temp.path())?;
    assert!(
        tmux.output(["has-session", "-t", "project"])?
            .status
            .success()
    );

    let project_session_id = tmux
        .list_panes()?
        .into_iter()
        .find(|pane| pane.placement.session_name == "project")
        .map(|pane| pane.identity.session_id)
        .ok_or_else(|| anyhow::anyhow!("expected project session"))?;
    let pane_id = tmux.create_window_by_id(&project_session_id, "feature-auth", temp.path())?;
    let context = tmux.pane_context(&pane_id)?;

    assert_eq!(context.session_name, "project");
    assert_eq!(context.window_name, "feature-auth");
    assert_eq!(context.pane_id, pane_id);
    assert!(tmux.window_exists_by_name_by_id(&project_session_id, "feature-auth")?);
    assert!(
        tmux.list_windows(Some("project"))?
            .iter()
            .all(|window| window.session_name == "project")
    );
    let snapshot = tmux
        .list_pane_snapshots()?
        .into_iter()
        .find(|pane| pane.identity.pane_id == pane_id)
        .ok_or_else(|| anyhow::anyhow!("expected created pane in tmux snapshot"))?;
    assert_eq!(snapshot.placement.session_name, "project");
    assert_eq!(snapshot.identity.window_id, context.window_id);
    assert_eq!(snapshot.placement.window_name, "feature-auth");

    tmux.select_window_id_in_session(&context.session_name, &context.window_id)?;
    tmux.select_pane(&pane_id)?;
    tmux.set_pane_title(&pane_id, "kmux")?;
    let updated_snapshot = tmux
        .list_pane_snapshots()?
        .into_iter()
        .find(|pane| pane.identity.pane_id == pane_id)
        .ok_or_else(|| anyhow::anyhow!("expected updated pane in tmux snapshot"))?;
    assert_eq!(updated_snapshot.title.as_deref(), Some("kmux"));
    assert!(!tmux.pane_visibility(&pane_id)?.pane_has_focus);

    tmux.select_window_by_id(&project_session_id, "feature-auth")?;
    let selected = tmux
        .list_windows(Some("project"))?
        .into_iter()
        .find(|window| window.window_name == "feature-auth")
        .ok_or_else(|| anyhow::anyhow!("expected feature-auth window"))?;
    assert!(selected.active);

    tmux.kill_window_id_in_session(&project_session_id, &selected.window_id)?;
    assert!(!tmux.window_exists_by_name_by_id(&project_session_id, "feature-auth")?);
    Ok(())
}

pub fn lightweight_pane_listing_treats_missing_server_as_empty() -> Result<()> {
    let fixture = TmuxFixture::new()?;
    assert!(fixture.tmux.list_panes()?.is_empty());
    Ok(())
}

pub fn project_session_window_commands_use_opaque_session_ids() -> Result<()> {
    let fixture = TmuxFixture::new()?;
    let temp = TempDir::new()?;
    let tmux = &fixture.tmux;

    create_test_session(tmux, "project alpha", temp.path())?;
    let panes = tmux.list_panes()?;
    let session_id = panes
        .iter()
        .find(|pane| pane.placement.session_name == "project alpha")
        .map(|pane| pane.identity.session_id.clone())
        .ok_or_else(|| anyhow::anyhow!("expected hostile-name test session: {panes:?}"))?;

    let pane_id = tmux.create_window_by_id(&session_id, "feature-auth", temp.path())?;
    assert!(tmux.window_exists_by_name_by_id(&session_id, "feature-auth")?);
    let window_id = tmux.pane_context(&pane_id)?.window_id;
    tmux.kill_window_id_in_session(&session_id, &window_id)?;
    Ok(())
}

pub fn physical_window_id_disambiguates_duplicate_names() -> Result<()> {
    let fixture = TmuxFixture::new()?;
    let temp = TempDir::new()?;
    let tmux = &fixture.tmux;

    create_test_session(tmux, "project", temp.path())?;
    let session_id = tmux
        .list_panes()?
        .into_iter()
        .find(|pane| pane.placement.session_name == "project")
        .map(|pane| pane.identity.session_id)
        .ok_or_else(|| anyhow::anyhow!("expected project session"))?;
    let first_pane = tmux.create_window_by_id(&session_id, "duplicate", temp.path())?;
    let second_pane = tmux.create_window_by_id(&session_id, "duplicate", temp.path())?;
    let first_window = tmux.pane_context(&first_pane)?.window_id;
    let second_window = tmux.pane_context(&second_pane)?.window_id;

    tmux.kill_window_id_in_session(&session_id, &first_window)?;

    let remaining = tmux.list_windows_by_id(&session_id)?;
    assert!(
        !remaining
            .iter()
            .any(|window| window.window_id == first_window)
    );
    assert!(
        remaining
            .iter()
            .any(|window| window.window_id == second_window)
    );
    Ok(())
}

pub fn literal_command_runs_inside_shell_and_window_survives_exit() -> Result<()> {
    let fixture = TmuxFixture::new()?;
    let temp = TempDir::new()?;
    let tmux = &fixture.tmux;
    let marker = temp.path().join("startup-ran");

    create_test_session(tmux, "project", temp.path())?;
    let session_id = tmux
        .list_panes()?
        .into_iter()
        .find(|pane| pane.placement.session_name == "project")
        .map(|pane| pane.identity.session_id)
        .ok_or_else(|| anyhow::anyhow!("expected project session"))?;
    let pane_id = tmux.create_window_by_id(&session_id, "feature-auth", temp.path())?;
    tmux.send_literal_command(&pane_id, "touch startup-ran")?;

    assert!(wait_for_path(&marker));
    assert!(tmux.window_exists_by_name_by_id(&session_id, "feature-auth")?);
    Ok(())
}

fn wait_for_path(path: &Path) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        thread::sleep(Duration::from_millis(25));
    }
    false
}
