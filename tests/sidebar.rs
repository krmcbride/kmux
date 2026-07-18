pub mod support;

use std::fs;

use anyhow::Result;
use tempfile::TempDir;

use support::{
    TmuxFixture, kmux, raw_key_capture_command, wait_for_nonempty_file, wait_for_path, write_config,
};

const IDLE_PANE_COMMAND: &str = "sh -c 'while :; do sleep 60; done'";
const FIXED_WIDTH_CONFIG: &str = "sidebar: {width: {min: 30, percent: 20, max: 30}}\n";

#[test]
fn sidebar_on_creates_refreshes_and_off_removes_marked_panes() -> Result<()> {
    let cwd = TempDir::new()?;
    let Some(tmux) = TmuxFixture::new(cwd.path())? else {
        return Ok(());
    };
    let config_home = write_config(cwd.path(), FIXED_WIDTH_CONFIG)?;
    let window_id = tmux.current_window_id()?;
    tmux.resize_window_and_wait(&window_id, &tmux.pane_id, 100)?;

    kmux(cwd.path(), &config_home, &tmux)?
        .args(["sidebar", "on"])
        .assert()
        .success();
    assert_eq!(
        tmux.global_option("@kmux_sidebar_enabled")?.as_deref(),
        Some("1")
    );
    assert_eq!(tmux.sidebar_pane_count()?, 1);
    let sidebar_pane = tmux.sidebar_pane_for_window(&window_id)?;
    assert_eq!(tmux.pane_format(&sidebar_pane, "#{@kmux_role}")?, "sidebar");
    tmux.wait_for_pane_format(&sidebar_pane, "#{pane_width}", "30")?;
    tmux.wait_for_sidebar_title("kmux")?;
    assert!(
        tmux.global_hook("after-new-window[90]")?
            .contains("sidebar _refresh")
    );
    assert!(
        tmux.global_hook("window-resized[90]")?
            .contains("sidebar _refresh")
    );
    for hook_name in [
        "after-select-window[90]",
        "after-select-pane[90]",
        "client-session-changed[90]",
    ] {
        let wake_hook = tmux.global_hook(hook_name)?;
        assert!(wake_hook.contains("sidebar _wake"));
        assert!(wake_hook.contains("#{window_id}"));
    }
    assert!(tmux.has_one_sidebar_per_window()?);

    let new_window_id = tmux.tmux_output(&[
        "new-window",
        "-d",
        "-t",
        "project:",
        "-n",
        "secondary",
        "-P",
        "-F",
        "#{window_id}",
        IDLE_PANE_COMMAND,
    ])?;
    tmux.tmux_output(&["resize-window", "-t", &new_window_id, "-x", "100"])?;
    tmux.wait_for_one_sidebar_per_window()?;
    let new_sidebar_pane = tmux.sidebar_pane_for_window(&new_window_id)?;
    tmux.wait_for_pane_format(&new_sidebar_pane, "#{window_width}", "100")?;
    tmux.wait_for_pane_format(&new_sidebar_pane, "#{pane_width}", "30")?;

    kmux(cwd.path(), &config_home, &tmux)?
        .args(["sidebar", "off"])
        .assert()
        .success();
    assert_eq!(tmux.sidebar_pane_count()?, 0);
    assert_eq!(tmux.global_option("@kmux_sidebar_enabled")?, None);
    assert!(
        !tmux
            .tmux_output(&["show-hooks", "-g"])?
            .contains("sidebar _refresh")
    );
    assert!(
        !tmux
            .tmux_output(&["show-hooks", "-g"])?
            .contains("sidebar _wake")
    );
    Ok(())
}

#[test]
fn sidebar_toggle_enables_and_disables_sidebar() -> Result<()> {
    let cwd = TempDir::new()?;
    let Some(tmux) = TmuxFixture::new(cwd.path())? else {
        return Ok(());
    };
    let config_home = write_config(cwd.path(), FIXED_WIDTH_CONFIG)?;

    kmux(cwd.path(), &config_home, &tmux)?
        .args(["sidebar", "toggle"])
        .assert()
        .success();
    assert_eq!(
        tmux.global_option("@kmux_sidebar_enabled")?.as_deref(),
        Some("1")
    );
    assert_eq!(tmux.sidebar_pane_count()?, 1);

    kmux(cwd.path(), &config_home, &tmux)?
        .args(["sidebar", "toggle"])
        .assert()
        .success();
    assert_eq!(tmux.global_option("@kmux_sidebar_enabled")?, None);
    assert_eq!(tmux.sidebar_pane_count()?, 0);
    Ok(())
}

#[test]
fn sidebar_enable_sizes_from_layout_after_duplicate_pruning() -> Result<()> {
    let cwd = TempDir::new()?;
    let Some(tmux) = TmuxFixture::new(cwd.path())? else {
        return Ok(());
    };
    let config_home = write_config(cwd.path(), "")?;
    let window_id = tmux.current_window_id()?;
    tmux.resize_window_and_wait(&window_id, &tmux.pane_id, 20)?;

    for _ in 0..2 {
        let pane_id = tmux.tmux_output(&[
            "split-window",
            "-d",
            "-h",
            "-b",
            "-f",
            "-t",
            &window_id,
            "-l",
            "1",
            "-P",
            "-F",
            "#{pane_id}",
            IDLE_PANE_COMMAND,
        ])?;
        tmux.tmux_output(&["set-option", "-p", "-t", &pane_id, "@kmux_role", "sidebar"])?;
    }

    kmux(cwd.path(), &config_home, &tmux)?
        .args(["sidebar", "on"])
        .assert()
        .success();

    let sidebar_pane = tmux.sidebar_pane_for_window(&window_id)?;
    assert_eq!(tmux.sidebar_pane_count()?, 1);
    assert_eq!(tmux.pane_count_for_window(&window_id)?, 2);
    tmux.wait_for_pane_format(&sidebar_pane, "#{pane_width}", "18")?;
    tmux.wait_for_pane_command(&sidebar_pane, "kmux")?;
    Ok(())
}

#[test]
fn sidebar_window_resize_clamps_proportional_width() -> Result<()> {
    let cwd = TempDir::new()?;
    let Some(tmux) = TmuxFixture::new(cwd.path())? else {
        return Ok(());
    };
    let config_home = write_config(
        cwd.path(),
        "sidebar: {width: {min: 30, percent: 25, max: 50}}\n",
    )?;
    let window_id = tmux.current_window_id()?;
    tmux.resize_window_and_wait(&window_id, &tmux.pane_id, 120)?;

    kmux(cwd.path(), &config_home, &tmux)?
        .args(["sidebar", "on"])
        .assert()
        .success();

    let sidebar_pane = tmux.sidebar_pane_for_window(&window_id)?;
    tmux.wait_for_pane_format(&sidebar_pane, "#{pane_width}", "30")?;

    tmux.resize_window_and_wait(&window_id, &sidebar_pane, 160)?;
    tmux.wait_for_pane_format(&sidebar_pane, "#{pane_width}", "40")?;

    assert_eq!(tmux.sidebar_pane_count()?, 1);
    assert_eq!(tmux.pane_format(&sidebar_pane, "#{@kmux_role}")?, "sidebar");
    Ok(())
}

#[test]
fn sidebar_creation_uses_full_window_left_edge_in_multi_pane_layout() -> Result<()> {
    let cwd = TempDir::new()?;
    let Some(tmux) = TmuxFixture::new(cwd.path())? else {
        return Ok(());
    };
    let config_home = write_config(cwd.path(), FIXED_WIDTH_CONFIG)?;
    let window_id = tmux.current_window_id()?;
    tmux.resize_window_and_wait(&window_id, &tmux.pane_id, 100)?;
    let right_pane = tmux.tmux_output(&[
        "split-window",
        "-d",
        "-h",
        "-t",
        &window_id,
        "-l",
        "50",
        "-P",
        "-F",
        "#{pane_id}",
        IDLE_PANE_COMMAND,
    ])?;
    tmux.tmux_output(&["select-pane", "-t", &right_pane])?;

    kmux(cwd.path(), &config_home, &tmux)?
        .args(["sidebar", "on"])
        .assert()
        .success();

    let sidebar_pane = tmux.sidebar_pane_for_window(&window_id)?;
    let content_pane_top = tmux.pane_format(&tmux.pane_id, "#{pane_top}")?;
    let content_pane_height = tmux.pane_format(&tmux.pane_id, "#{pane_height}")?;
    tmux.wait_for_pane_format(&sidebar_pane, "#{pane_left}", "0")?;
    tmux.wait_for_pane_format(&sidebar_pane, "#{pane_width}", "30")?;
    tmux.wait_for_pane_format(&sidebar_pane, "#{pane_top}", &content_pane_top)?;
    tmux.wait_for_pane_format(&sidebar_pane, "#{pane_height}", &content_pane_height)?;
    assert_eq!(tmux.pane_count_for_window(&window_id)?, 3);
    Ok(())
}

#[test]
fn sidebar_on_reloads_width_policy_from_config() -> Result<()> {
    let cwd = TempDir::new()?;
    let Some(tmux) = TmuxFixture::new(cwd.path())? else {
        return Ok(());
    };
    let config_home = write_config(cwd.path(), FIXED_WIDTH_CONFIG)?;
    let window_id = tmux.current_window_id()?;
    tmux.resize_window_and_wait(&window_id, &tmux.pane_id, 100)?;

    kmux(cwd.path(), &config_home, &tmux)?
        .args(["sidebar", "on"])
        .assert()
        .success();

    let sidebar_pane = tmux.sidebar_pane_for_window(&window_id)?;
    tmux.wait_for_pane_format(&sidebar_pane, "#{pane_width}", "30")?;

    fs::write(
        config_home.join("kmux/config.yaml"),
        "sidebar: {width: {min: 45, percent: 20, max: 45}}\n",
    )?;
    kmux(cwd.path(), &config_home, &tmux)?
        .args(["sidebar", "on"])
        .assert()
        .success();

    tmux.wait_for_pane_format(&sidebar_pane, "#{pane_width}", "45")?;
    Ok(())
}

#[test]
fn sidebar_on_reuses_unmarked_sidebar_in_linked_window() -> Result<()> {
    let cwd = TempDir::new()?;
    let Some(tmux) = TmuxFixture::new(cwd.path())? else {
        return Ok(());
    };
    let config_home = write_config(cwd.path(), FIXED_WIDTH_CONFIG)?;
    let window_id = tmux.current_window_id()?;
    tmux.resize_window_and_wait(&window_id, &tmux.pane_id, 100)?;
    let restored_pane = tmux.tmux_output(&[
        "split-window",
        "-d",
        "-h",
        "-b",
        "-f",
        "-t",
        &window_id,
        "-l",
        "30",
        "-P",
        "-F",
        "#{pane_id}",
        IDLE_PANE_COMMAND,
    ])?;
    let cwd_path = cwd.path().to_string_lossy().into_owned();
    tmux.tmux_output(&[
        "new-session",
        "-d",
        "-s",
        "linked-project",
        "-c",
        &cwd_path,
        IDLE_PANE_COMMAND,
    ])?;
    tmux.tmux_output(&[
        "link-window",
        "-d",
        "-s",
        &window_id,
        "-t",
        "linked-project:",
    ])?;
    let listed_panes = tmux.tmux_output(&["list-panes", "-a", "-F", "#{pane_id}"])?;
    assert_eq!(
        listed_panes
            .lines()
            .filter(|pane_id| *pane_id == restored_pane)
            .count(),
        2
    );

    kmux(cwd.path(), &config_home, &tmux)?
        .args(["sidebar", "on"])
        .assert()
        .success();

    tmux.wait_for_one_sidebar_per_window()?;
    assert_eq!(tmux.pane_count_for_window(&window_id)?, 2);
    assert_eq!(tmux.sidebar_pane_for_window(&window_id)?, restored_pane);
    tmux.wait_for_pane_format(&restored_pane, "#{pane_width}", "30")?;
    tmux.wait_for_pane_format(&restored_pane, "#{@kmux_role}", "sidebar")?;
    tmux.wait_for_pane_command(&restored_pane, "kmux")?;
    Ok(())
}

#[test]
fn sidebar_wake_sends_key_only_to_target_window_sidebar() -> Result<()> {
    let cwd = TempDir::new()?;
    let Some(tmux) = TmuxFixture::new(cwd.path())? else {
        return Ok(());
    };
    let config_home = write_config(cwd.path(), "")?;

    let source_window_id = tmux.tmux_output(&["display-message", "-p", "#{window_id}"])?;
    tmux.resize_window_and_wait(&source_window_id, &tmux.pane_id, 100)?;
    let source_capture = cwd.path().join("source-wake.bin");
    let source_ready = cwd.path().join("source-wake.ready");
    let source_command = raw_key_capture_command(&source_capture, &source_ready);
    let source_sidebar = tmux.tmux_output(&[
        "split-window",
        "-d",
        "-h",
        "-b",
        "-f",
        "-t",
        &source_window_id,
        "-l",
        "30",
        "-P",
        "-F",
        "#{pane_id}",
        &source_command,
    ])?;
    tmux.tmux_output(&[
        "set-option",
        "-p",
        "-t",
        &source_sidebar,
        "@kmux_role",
        "sidebar",
    ])?;

    let target_window_id = tmux.tmux_output(&[
        "new-window",
        "-d",
        "-t",
        "project:",
        "-n",
        "wake-target",
        "-P",
        "-F",
        "#{window_id}",
        IDLE_PANE_COMMAND,
    ])?;
    let target_content_pane = tmux.pane_for_window("wake-target")?;
    tmux.resize_window_and_wait(&target_window_id, &target_content_pane, 100)?;
    let target_capture = cwd.path().join("target-wake.bin");
    let target_ready = cwd.path().join("target-wake.ready");
    let target_command = raw_key_capture_command(&target_capture, &target_ready);
    let target_sidebar = tmux.tmux_output(&[
        "split-window",
        "-d",
        "-h",
        "-b",
        "-f",
        "-t",
        &target_window_id,
        "-l",
        "30",
        "-P",
        "-F",
        "#{pane_id}",
        &target_command,
    ])?;
    tmux.tmux_output(&[
        "set-option",
        "-p",
        "-t",
        &target_sidebar,
        "@kmux_role",
        "sidebar",
    ])?;

    wait_for_path(&source_ready)?;
    wait_for_path(&target_ready)?;

    kmux(cwd.path(), &config_home, &tmux)?
        .args(["sidebar", "_wake", &target_window_id])
        .assert()
        .success();

    wait_for_nonempty_file(&target_capture)?;
    assert_eq!(fs::read(&source_capture).map_or(0, |bytes| bytes.len()), 0);

    Ok(())
}
