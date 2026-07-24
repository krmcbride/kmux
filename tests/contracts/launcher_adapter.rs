use anyhow::Result;

#[cfg(unix)]
#[test]
fn request_round_trip_preserves_exact_argv_and_cleans_up() -> Result<()> {
    kmux::contract_tests::launcher::request_round_trip_preserves_exact_argv_and_cleans_up()
}

#[cfg(unix)]
#[test]
fn absent_and_empty_input_remain_distinct() -> Result<()> {
    kmux::contract_tests::launcher::absent_and_empty_input_remain_distinct()
}

#[cfg(unix)]
#[test]
fn concurrent_requests_do_not_collide_or_cross_acknowledgments() -> Result<()> {
    kmux::contract_tests::launcher::concurrent_requests_do_not_collide_or_cross_acknowledgments()
}

#[test]
fn spawn_failure_acknowledgment_and_diagnostics_are_sanitized() -> Result<()> {
    kmux::contract_tests::launcher::spawn_failure_acknowledgment_and_diagnostics_are_sanitized()
}

#[cfg(unix)]
#[test]
fn acknowledgment_delivery_failure_keeps_waiting_and_leaves_spawn_state_unknown() -> Result<()> {
    kmux::contract_tests::launcher::acknowledgment_delivery_failure_keeps_waiting_and_leaves_spawn_state_unknown()
}

#[cfg(unix)]
#[test]
fn relative_executable_paths_resolve_from_launcher_cwd() -> Result<()> {
    kmux::contract_tests::launcher::relative_executable_paths_resolve_from_launcher_cwd()
}
