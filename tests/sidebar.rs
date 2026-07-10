mod support;

use std::fs;

use anyhow::Result;

use support::{
    TmuxFixture, init_repo, kmux, raw_key_capture_command, wait_for_file_bytes, wait_for_path,
    write_config,
};

const IDLE_PANE_COMMAND: &str = "sh -c 'while :; do sleep 60; done'";

#[test]
fn sidebar_toggle_creates_refreshes_and_removes_marked_panes() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "sidebar: {width: 30}\n")?;

    kmux(&repo, &config_home, &tmux)?
        .args(["sidebar", "on"])
        .assert()
        .success();
    assert_eq!(
        tmux.global_option("@kmux_sidebar_enabled")?.as_deref(),
        Some("1")
    );
    assert_eq!(
        tmux.global_option("@kmux_sidebar_width")?.as_deref(),
        Some("30")
    );
    assert_eq!(tmux.sidebar_pane_count()?, 1);
    assert!(tmux.wait_for_sidebar_title("kmux")?);
    assert!(
        tmux.global_hook("after-new-window[90]")?
            .contains("sidebar refresh")
    );
    assert!(
        tmux.global_hook("window-resized[90]")?
            .contains("sidebar refresh")
    );
    let wake_hook = tmux.global_hook("after-select-window[90]")?;
    assert!(wake_hook.contains("sidebar wake"));
    assert!(wake_hook.contains("#{window_id}"));
    let pane_wake_hook = tmux.global_hook("after-select-pane[90]")?;
    assert!(pane_wake_hook.contains("sidebar wake"));
    assert!(pane_wake_hook.contains("#{window_id}"));
    let session_wake_hook = tmux.global_hook("client-session-changed[90]")?;
    assert!(session_wake_hook.contains("sidebar wake"));
    assert!(session_wake_hook.contains("#{window_id}"));
    assert!(tmux.has_one_sidebar_per_window()?);

    for index in 0..5 {
        tmux.tmux_output(&[
            "new-window",
            "-d",
            "-t",
            "project:",
            "-n",
            &format!("scratch-{index}"),
        ])?;
    }
    assert!(tmux.wait_for_one_sidebar_per_window()?);
    kmux(&repo, &config_home, &tmux)?
        .args(["sidebar", "refresh"])
        .assert()
        .success();
    assert!(tmux.has_one_sidebar_per_window()?);

    kmux(&repo, &config_home, &tmux)?
        .args(["sidebar", "off"])
        .assert()
        .success();
    assert_eq!(tmux.sidebar_pane_count()?, 0);
    assert_eq!(tmux.global_option("@kmux_sidebar_enabled")?, None);
    assert!(
        !tmux
            .tmux_output(&["show-hooks", "-g"])?
            .contains("sidebar refresh")
    );
    assert!(
        !tmux
            .tmux_output(&["show-hooks", "-g"])?
            .contains("sidebar wake")
    );
    Ok(())
}

#[test]
fn sidebar_refresh_is_noop_when_sidebar_is_disabled() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "sidebar: {width: 30}\n")?;

    kmux(&repo, &config_home, &tmux)?
        .args(["sidebar", "refresh"])
        .assert()
        .success();

    assert_eq!(tmux.sidebar_pane_count()?, 0);
    assert_eq!(tmux.global_option("@kmux_sidebar_enabled")?, None);
    Ok(())
}

#[test]
fn sidebar_window_resize_restores_configured_width() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "sidebar: {width: 30}\n")?;

    kmux(&repo, &config_home, &tmux)?
        .args(["sidebar", "on"])
        .assert()
        .success();

    let window_id = tmux.current_window_id()?;
    let sidebar_pane = tmux.tmux_output(&[
        "list-panes",
        "-t",
        &window_id,
        "-f",
        "#{==:#{@kmux_role},sidebar}",
        "-F",
        "#{pane_id}",
    ])?;
    assert!(!sidebar_pane.is_empty());

    tmux.tmux_output(&["resize-window", "-t", &window_id, "-x", "160"])?;

    assert!(tmux.wait_for_pane_format(&sidebar_pane, "#{pane_width}", "30")?);
    Ok(())
}

#[test]
fn sidebar_on_reuses_unmarked_sidebar_shaped_pane() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "sidebar: {width: 30}\n")?;
    let window_id = tmux.tmux_output(&["display-message", "-p", "#{window_id}"])?;
    let restored_pane = tmux.tmux_output(&[
        "split-window",
        "-d",
        "-h",
        "-b",
        "-t",
        &window_id,
        "-l",
        "30",
        "-P",
        "-F",
        "#{pane_id}",
        IDLE_PANE_COMMAND,
    ])?;
    assert!(tmux.wait_for_pane_format(&restored_pane, "#{pane_left}", "0")?);
    assert!(tmux.wait_for_pane_format(&restored_pane, "#{pane_width}", "30")?);

    assert_eq!(tmux.sidebar_pane_count()?, 0);
    assert_eq!(tmux.pane_count_for_window(&window_id)?, 2);

    kmux(&repo, &config_home, &tmux)?
        .args(["sidebar", "on"])
        .assert()
        .success();

    assert_eq!(tmux.pane_count_for_window(&window_id)?, 2);
    assert_eq!(tmux.sidebar_pane_count()?, 1);
    assert!(tmux.wait_for_pane_format(&restored_pane, "#{@kmux_role}", "sidebar")?);
    assert!(tmux.wait_for_pane_format(&restored_pane, "#{pane_width}", "30")?);
    assert!(tmux.wait_for_pane_command(&restored_pane, "kmux")?);

    Ok(())
}

#[test]
fn sidebar_on_ignores_unmarked_left_pane_with_different_width() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "sidebar: {width: 30}\n")?;
    let window_id = tmux.tmux_output(&["display-message", "-p", "#{window_id}"])?;
    let restored_pane = tmux.tmux_output(&[
        "split-window",
        "-d",
        "-h",
        "-b",
        "-t",
        &window_id,
        "-l",
        "10",
        "-P",
        "-F",
        "#{pane_id}",
        IDLE_PANE_COMMAND,
    ])?;
    assert!(tmux.wait_for_pane_format(&restored_pane, "#{pane_left}", "0")?);
    assert!(tmux.wait_for_pane_format(&restored_pane, "#{pane_width}", "10")?);

    assert_eq!(tmux.pane_format(&restored_pane, "#{@kmux_role}")?, "");
    assert_eq!(tmux.pane_count_for_window(&window_id)?, 2);

    kmux(&repo, &config_home, &tmux)?
        .args(["sidebar", "on"])
        .assert()
        .success();

    assert_eq!(tmux.pane_count_for_window(&window_id)?, 3);
    assert_eq!(tmux.sidebar_pane_count()?, 1);
    assert_eq!(tmux.pane_format(&restored_pane, "#{@kmux_role}")?, "");

    Ok(())
}

#[test]
fn sidebar_off_preserves_unmarked_sidebar_shaped_pane() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "sidebar: {width: 30}\n")?;
    let window_id = tmux.tmux_output(&["display-message", "-p", "#{window_id}"])?;
    let restored_pane = tmux.tmux_output(&[
        "split-window",
        "-d",
        "-h",
        "-b",
        "-t",
        &window_id,
        "-l",
        "30",
        "-P",
        "-F",
        "#{pane_id}",
        IDLE_PANE_COMMAND,
    ])?;
    assert!(tmux.wait_for_pane_format(&restored_pane, "#{pane_left}", "0")?);
    assert!(tmux.wait_for_pane_format(&restored_pane, "#{pane_width}", "30")?);

    assert_eq!(tmux.pane_count_for_window(&window_id)?, 2);

    kmux(&repo, &config_home, &tmux)?
        .args(["sidebar", "off"])
        .assert()
        .success();

    assert_eq!(tmux.pane_count_for_window(&window_id)?, 2);
    assert_eq!(tmux.pane_format(&restored_pane, "#{@kmux_role}")?, "");

    Ok(())
}

#[test]
fn sidebar_wake_sends_key_only_to_target_window_sidebar() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "")?;

    let source_window_id = tmux.tmux_output(&["display-message", "-p", "#{window_id}"])?;
    let source_capture = temp.path().join("source-wake.bin");
    let source_ready = temp.path().join("source-wake.ready");
    let source_command = raw_key_capture_command(&source_capture, &source_ready);
    let source_sidebar = tmux.tmux_output(&[
        "split-window",
        "-d",
        "-t",
        &source_window_id,
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
    ])?;
    let target_capture = temp.path().join("target-wake.bin");
    let target_ready = temp.path().join("target-wake.ready");
    let target_command = raw_key_capture_command(&target_capture, &target_ready);
    let target_sidebar = tmux.tmux_output(&[
        "split-window",
        "-d",
        "-t",
        &target_window_id,
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

    assert!(wait_for_path(&source_ready)?);
    assert!(wait_for_path(&target_ready)?);

    kmux(&repo, &config_home, &tmux)?
        .args(["sidebar", "wake", &target_window_id])
        .assert()
        .success();

    assert!(wait_for_file_bytes(&target_capture)?);
    assert_eq!(fs::read(&source_capture).map_or(0, |bytes| bytes.len()), 0);

    Ok(())
}
