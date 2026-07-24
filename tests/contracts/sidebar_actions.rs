use anyhow::Result;

#[test]
fn selection_options_round_trip_restore_and_cleanup() -> Result<()> {
    kmux::contract_tests::sidebar::selection_options_round_trip_restore_and_cleanup()
}

#[test]
fn stale_jump_candidate_falls_back_and_focuses_content_pane() -> Result<()> {
    kmux::contract_tests::sidebar::stale_jump_candidate_falls_back_and_focuses_content_pane()
}

#[test]
fn failed_detached_jump_restores_previous_selection() -> Result<()> {
    kmux::contract_tests::sidebar::failed_detached_jump_restores_previous_selection()
}

#[test]
fn delete_refreshes_badge_and_sidebar_surfaces_on_private_server() -> Result<()> {
    kmux::contract_tests::sidebar::delete_refreshes_badge_and_sidebar_surfaces_on_private_server()
}
