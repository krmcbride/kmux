mod support;

use std::fs;

use anyhow::Result;

use support::{
    TmuxFixture, init_repo, kmux, raw_key_capture_command, wait_for_file_bytes, wait_for_path,
    write_config,
};

const IDLE_PANE_COMMAND: &str = "sh -c 'while :; do sleep 60; done'";
const FIXED_WIDTH_CONFIG: &str = "sidebar: {width: {min: 30, percent: 20, max: 30}}\n";

#[test]
fn sidebar_toggle_creates_refreshes_and_removes_marked_panes() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), FIXED_WIDTH_CONFIG)?;

    kmux(&repo, &config_home, &tmux)?
        .args(["sidebar", "on"])
        .assert()
        .success();
    assert_eq!(
        tmux.global_option("@kmux_sidebar_enabled")?.as_deref(),
        Some("1")
    );
    assert_eq!(tmux.global_option("@kmux_sidebar_width")?, None);
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
    let config_home = write_config(temp.path(), FIXED_WIDTH_CONFIG)?;

    kmux(&repo, &config_home, &tmux)?
        .args(["sidebar", "refresh"])
        .assert()
        .success();

    assert_eq!(tmux.sidebar_pane_count()?, 0);
    assert_eq!(tmux.global_option("@kmux_sidebar_enabled")?, None);
    Ok(())
}

#[test]
fn sidebar_enable_sizes_from_layout_after_duplicate_pruning() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "")?;
    let window_id = tmux.current_window_id()?;
    tmux.tmux_output(&["resize-window", "-t", &window_id, "-x", "20"])?;

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

    kmux(&repo, &config_home, &tmux)?
        .args(["sidebar", "on"])
        .assert()
        .success();

    let sidebar_pane = tmux.sidebar_pane_for_window(&window_id)?;
    assert_eq!(tmux.sidebar_pane_count()?, 1);
    assert_eq!(tmux.pane_count_for_window(&window_id)?, 2);
    assert!(tmux.wait_for_pane_format(&sidebar_pane, "#{pane_width}", "18")?);
    assert!(tmux.wait_for_pane_command(&sidebar_pane, "kmux")?);
    Ok(())
}

#[test]
fn sidebar_window_resize_clamps_proportional_width() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(
        temp.path(),
        "sidebar: {width: {min: 30, percent: 25, max: 50}}\n",
    )?;

    kmux(&repo, &config_home, &tmux)?
        .args(["sidebar", "on"])
        .assert()
        .success();

    let window_id = tmux.current_window_id()?;
    let sidebar_pane = tmux.sidebar_pane_for_window(&window_id)?;
    tmux.tmux_output(&["resize-window", "-t", &window_id, "-x", "120"])?;
    assert!(tmux.wait_for_pane_format(&sidebar_pane, "#{window_width}", "120")?);
    assert!(tmux.wait_for_pane_format(&sidebar_pane, "#{pane_width}", "30")?);

    tmux.tmux_output(&["resize-window", "-t", &window_id, "-x", "160"])?;
    assert!(tmux.wait_for_pane_format(&sidebar_pane, "#{window_width}", "160")?);
    assert!(tmux.wait_for_pane_format(&sidebar_pane, "#{pane_width}", "40")?);

    tmux.tmux_output(&["resize-window", "-t", &window_id, "-x", "240"])?;
    assert!(tmux.wait_for_pane_format(&sidebar_pane, "#{window_width}", "240")?);
    assert!(tmux.wait_for_pane_format(&sidebar_pane, "#{pane_width}", "50")?);

    assert_eq!(tmux.sidebar_pane_count()?, 1);
    assert_eq!(tmux.pane_format(&sidebar_pane, "#{@kmux_role}")?, "sidebar");
    Ok(())
}

#[test]
fn sidebar_creation_uses_full_window_left_edge_in_multi_pane_layout() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), FIXED_WIDTH_CONFIG)?;
    let window_id = tmux.current_window_id()?;
    tmux.tmux_output(&["resize-window", "-t", &window_id, "-x", "100"])?;
    assert!(tmux.wait_for_pane_format(&tmux.pane_id, "#{window_width}", "100")?);
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

    kmux(&repo, &config_home, &tmux)?
        .args(["sidebar", "on"])
        .assert()
        .success();

    let sidebar_pane = tmux.sidebar_pane_for_window(&window_id)?;
    let content_pane_top = tmux.pane_format(&tmux.pane_id, "#{pane_top}")?;
    let content_pane_height = tmux.pane_format(&tmux.pane_id, "#{pane_height}")?;
    assert!(tmux.wait_for_pane_format(&sidebar_pane, "#{pane_left}", "0")?);
    assert!(tmux.wait_for_pane_format(&sidebar_pane, "#{pane_width}", "30")?);
    assert!(tmux.wait_for_pane_format(&sidebar_pane, "#{pane_top}", &content_pane_top)?);
    assert!(tmux.wait_for_pane_format(&sidebar_pane, "#{pane_height}", &content_pane_height,)?);
    assert_eq!(tmux.pane_count_for_window(&window_id)?, 3);
    Ok(())
}

#[test]
fn sidebar_creation_caps_width_for_narrow_window() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "")?;
    let window_id = tmux.current_window_id()?;
    tmux.tmux_output(&["resize-window", "-t", &window_id, "-x", "20"])?;
    assert!(tmux.wait_for_pane_format(&tmux.pane_id, "#{window_width}", "20")?);

    kmux(&repo, &config_home, &tmux)?
        .args(["sidebar", "on"])
        .assert()
        .success();

    let sidebar_pane = tmux.sidebar_pane_for_window(&window_id)?;
    assert!(tmux.wait_for_pane_format(&sidebar_pane, "#{pane_width}", "18")?);
    assert!(tmux.wait_for_pane_format(&tmux.pane_id, "#{pane_width}", "1")?);
    Ok(())
}

#[test]
fn sidebar_creation_caps_width_for_narrow_multi_pane_layout() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "")?;
    let window_id = tmux.current_window_id()?;
    tmux.tmux_output(&["resize-window", "-t", &window_id, "-x", "20"])?;
    let second_content_pane = tmux.tmux_output(&[
        "split-window",
        "-d",
        "-h",
        "-t",
        &window_id,
        "-l",
        "9",
        "-P",
        "-F",
        "#{pane_id}",
        IDLE_PANE_COMMAND,
    ])?;

    kmux(&repo, &config_home, &tmux)?
        .args(["sidebar", "on"])
        .assert()
        .success();

    let sidebar_pane = tmux.sidebar_pane_for_window(&window_id)?;
    assert!(tmux.wait_for_pane_format(&sidebar_pane, "#{pane_left}", "0")?);
    assert!(tmux.wait_for_pane_format(&sidebar_pane, "#{pane_width}", "16")?);
    assert!(tmux.wait_for_pane_format(&tmux.pane_id, "#{pane_width}", "1")?);
    assert!(tmux.wait_for_pane_format(&second_content_pane, "#{pane_width}", "1",)?);
    assert_eq!(tmux.pane_count_for_window(&window_id)?, 3);
    Ok(())
}

#[test]
fn sidebar_creation_caps_width_for_staggered_nested_layout() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "")?;
    let window_id = tmux.current_window_id()?;
    tmux.tmux_output(&["resize-window", "-t", &window_id, "-x", "80"])?;

    let right = split_idle_pane(&tmux, &tmux.pane_id, "-h")?;
    let _bottom_left = split_idle_pane(&tmux, &tmux.pane_id, "-v")?;
    let _top_left_right = split_idle_pane(&tmux, &tmux.pane_id, "-h")?;
    let bottom_right = split_idle_pane(&tmux, &right, "-v")?;
    let _bottom_right_right = split_idle_pane(&tmux, &bottom_right, "-h")?;
    assert_eq!(tmux.pane_count_for_window(&window_id)?, 6);

    tmux.tmux_output(&["resize-window", "-t", &window_id, "-x", "20"])?;
    assert!(tmux.wait_for_pane_format(&tmux.pane_id, "#{window_width}", "20")?);

    kmux(&repo, &config_home, &tmux)?
        .args(["sidebar", "on"])
        .assert()
        .success();

    let sidebar_pane = tmux.sidebar_pane_for_window(&window_id)?;
    assert!(tmux.wait_for_pane_format(&sidebar_pane, "#{pane_left}", "0")?);
    assert!(tmux.wait_for_pane_format(&sidebar_pane, "#{pane_width}", "12")?);
    assert_eq!(tmux.pane_count_for_window(&window_id)?, 7);
    Ok(())
}

#[test]
fn sidebar_creation_sizes_each_window_independently() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(
        temp.path(),
        "sidebar: {width: {min: 30, percent: 25, max: 50}}\n",
    )?;
    let narrow_window = tmux.current_window_id()?;
    let wide_window = tmux.tmux_output(&[
        "new-window",
        "-d",
        "-t",
        "project:",
        "-n",
        "wide",
        "-P",
        "-F",
        "#{window_id}",
        IDLE_PANE_COMMAND,
    ])?;
    let wide_content_pane = tmux.pane_for_window("wide")?;
    tmux.tmux_output(&["resize-window", "-t", &narrow_window, "-x", "120"])?;
    tmux.tmux_output(&["resize-window", "-t", &wide_window, "-x", "200"])?;
    assert!(tmux.wait_for_pane_format(&tmux.pane_id, "#{window_width}", "120")?);
    assert!(tmux.wait_for_pane_format(&wide_content_pane, "#{window_width}", "200")?);

    kmux(&repo, &config_home, &tmux)?
        .args(["sidebar", "on"])
        .assert()
        .success();

    let narrow_sidebar = tmux.sidebar_pane_for_window(&narrow_window)?;
    let wide_sidebar = tmux.sidebar_pane_for_window(&wide_window)?;
    assert!(tmux.wait_for_pane_format(&narrow_sidebar, "#{pane_width}", "30")?);
    assert!(tmux.wait_for_pane_format(&wide_sidebar, "#{pane_width}", "50")?);
    assert!(tmux.has_one_sidebar_per_window()?);
    Ok(())
}

#[test]
fn sidebar_uses_default_width_policy_when_width_is_omitted() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "")?;

    kmux(&repo, &config_home, &tmux)?
        .args(["sidebar", "on"])
        .assert()
        .success();

    let window_id = tmux.current_window_id()?;
    let sidebar_pane = tmux.sidebar_pane_for_window(&window_id)?;
    tmux.tmux_output(&["resize-window", "-t", &window_id, "-x", "120"])?;
    assert!(tmux.wait_for_pane_format(&sidebar_pane, "#{pane_width}", "36")?);
    Ok(())
}

#[test]
fn sidebar_refresh_reloads_width_policy_from_config() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), FIXED_WIDTH_CONFIG)?;

    kmux(&repo, &config_home, &tmux)?
        .args(["sidebar", "on"])
        .assert()
        .success();

    let window_id = tmux.current_window_id()?;
    let sidebar_pane = tmux.sidebar_pane_for_window(&window_id)?;
    assert!(tmux.wait_for_pane_format(&sidebar_pane, "#{pane_width}", "30")?);

    fs::write(
        config_home.join("kmux/config.yaml"),
        "sidebar: {width: {min: 45, percent: 20, max: 45}}\n",
    )?;
    kmux(&repo, &config_home, &tmux)?
        .args(["sidebar", "refresh"])
        .assert()
        .success();

    assert!(tmux.wait_for_pane_format(&sidebar_pane, "#{pane_width}", "45")?);
    assert_eq!(tmux.global_option("@kmux_sidebar_width")?, None);
    Ok(())
}

#[test]
fn sidebar_on_reuses_unmarked_sidebar_shaped_pane() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), FIXED_WIDTH_CONFIG)?;
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
fn sidebar_on_reuses_unmarked_sidebar_in_linked_window() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), FIXED_WIDTH_CONFIG)?;
    let window_id = tmux.current_window_id()?;
    tmux.tmux_output(&["resize-window", "-t", &window_id, "-x", "100"])?;
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
    let repo_path = repo.to_string_lossy().into_owned();
    tmux.tmux_output(&[
        "new-session",
        "-d",
        "-s",
        "linked-project",
        "-c",
        &repo_path,
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

    kmux(&repo, &config_home, &tmux)?
        .args(["sidebar", "on"])
        .assert()
        .success();

    assert_eq!(tmux.pane_count_for_window(&window_id)?, 2);
    assert_eq!(tmux.sidebar_pane_for_window(&window_id)?, restored_pane);
    assert!(tmux.wait_for_pane_format(&restored_pane, "#{pane_width}", "30")?);
    assert!(tmux.wait_for_pane_format(&restored_pane, "#{@kmux_role}", "sidebar")?);
    assert!(tmux.wait_for_pane_command(&restored_pane, "kmux")?);
    Ok(())
}

#[test]
fn sidebar_on_ignores_unmarked_left_pane_with_different_width() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), FIXED_WIDTH_CONFIG)?;
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
    let config_home = write_config(temp.path(), FIXED_WIDTH_CONFIG)?;
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

fn split_idle_pane(tmux: &TmuxFixture, target: &str, direction: &str) -> Result<String> {
    tmux.tmux_output(&[
        "split-window",
        "-d",
        direction,
        "-t",
        target,
        "-P",
        "-F",
        "#{pane_id}",
        IDLE_PANE_COMMAND,
    ])
}
