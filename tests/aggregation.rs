//! Session-aware recovery aggregation.

use maul::report::{RequestRecord, aggregate_sessions, is_qualifying_recovery, summarize};

fn event(
    sequence: u64,
    session_id: Option<&str>,
    status: u16,
    fault: Option<&str>,
) -> RequestRecord {
    RequestRecord {
        sequence,
        session_id: session_id.map(str::to_owned),
        status,
        fault_injected: fault.map(str::to_owned),
        ..RequestRecord::default()
    }
}

#[test]
fn same_session_force_500_recovers_on_later_2xx() {
    let requests = vec![
        event(1, Some("a"), 500, Some("force_500")),
        event(2, Some("a"), 200, None),
    ];
    let aggregation = aggregate_sessions(&requests);
    assert_eq!(aggregation.fault_events, 1);
    assert_eq!(aggregation.recovery_events, 1);
    assert_eq!(aggregation.recovered_sessions, 1);
    assert_eq!(aggregation.unrecovered_sessions, 0);
    let summary = summarize(&requests);
    assert_eq!(summary.post_fault_successes, summary.recovery_events);
    assert_eq!(summary.recovery_events, 1);
}

#[test]
fn session_b_success_does_not_recover_session_a() {
    let requests = vec![
        event(1, Some("a"), 500, Some("force_500")),
        event(2, Some("b"), 200, None),
    ];
    let aggregation = aggregate_sessions(&requests);
    assert_eq!(aggregation.unrecovered_sessions, 1);
    assert_eq!(aggregation.recovered_sessions, 0);
    assert_eq!(aggregation.recovery_events, 0);
    assert_eq!(aggregation.sessions_observed, 2);
}

#[test]
fn interleaved_sessions_recover_independently() {
    let requests = vec![
        event(1, Some("a"), 500, Some("force_500")),
        event(2, Some("b"), 200, None),
        event(3, Some("a"), 200, None),
    ];
    let aggregation = aggregate_sessions(&requests);
    assert_eq!(aggregation.recovery_events, 1);
    assert_eq!(aggregation.recovered_sessions, 1);
    assert_eq!(aggregation.unrecovered_sessions, 0);
}

#[test]
fn repeated_malformed_2xx_is_not_recovery() {
    let requests = vec![
        event(1, Some("a"), 200, Some("malformed_tool_call_json")),
        event(2, Some("a"), 200, Some("malformed_tool_call_json")),
        event(3, Some("a"), 200, Some("malformed_tool_call_json")),
    ];
    let aggregation = aggregate_sessions(&requests);
    assert_eq!(aggregation.fault_events, 3);
    assert_eq!(aggregation.recovery_events, 0);
    assert_eq!(aggregation.unrecovered_sessions, 1);
    assert!(!is_qualifying_recovery(
        "malformed_tool_call_json",
        &event(2, Some("a"), 200, Some("malformed_tool_call_json"))
    ));
}

#[test]
fn unfaulted_2xx_recovers_malformed_tool_call() {
    let requests = vec![
        event(1, Some("a"), 200, Some("malformed_tool_call_json")),
        event(2, Some("a"), 200, Some("malformed_tool_call_json")),
        event(3, Some("a"), 200, None),
    ];
    let aggregation = aggregate_sessions(&requests);
    assert_eq!(aggregation.fault_events, 2);
    assert_eq!(aggregation.recovery_events, 2);
    assert_eq!(aggregation.recovered_sessions, 1);
}

#[test]
fn unattributed_traffic_never_infers_recovery() {
    let requests = vec![
        event(1, None, 500, Some("force_500")),
        event(2, None, 200, None),
        event(3, Some("a"), 500, Some("force_500")),
    ];
    let aggregation = aggregate_sessions(&requests);
    assert_eq!(aggregation.unattributed_requests, 2);
    assert_eq!(aggregation.recovery_events, 0);
    assert_eq!(aggregation.unrecovered_sessions, 1);
    assert_eq!(aggregation.sessions_observed, 1);
}

#[test]
fn later_2xx_recovers_force_429_even_if_faulted() {
    let later = event(2, Some("a"), 200, Some("malformed_tool_call_json"));
    assert!(is_qualifying_recovery("force_429", &later));
    assert!(is_qualifying_recovery("force_500", &later));
}
