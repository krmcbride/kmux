use anyhow::Result;

#[test]
fn creates_selects_lists_and_kills_windows_on_isolated_socket() -> Result<()> {
    kmux::contract_tests::tmux::creates_selects_lists_and_kills_windows_on_isolated_socket()
}

#[test]
fn lightweight_pane_listing_treats_missing_server_as_empty() -> Result<()> {
    kmux::contract_tests::tmux::lightweight_pane_listing_treats_missing_server_as_empty()
}

#[test]
fn project_session_window_commands_use_opaque_session_ids() -> Result<()> {
    kmux::contract_tests::tmux::project_session_window_commands_use_opaque_session_ids()
}

#[test]
fn physical_window_id_disambiguates_duplicate_names() -> Result<()> {
    kmux::contract_tests::tmux::physical_window_id_disambiguates_duplicate_names()
}

#[test]
fn literal_command_runs_inside_shell_and_window_survives_exit() -> Result<()> {
    kmux::contract_tests::tmux::literal_command_runs_inside_shell_and_window_survives_exit()
}
