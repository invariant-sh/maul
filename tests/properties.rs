use maul::budget::{MicroUsd, Price};
use maul::openai::TokenUsage;
use maul::proxy::request_transform::include_stream_usage;
use maul::report::{RequestRecord, aggregate_sessions};
use proptest::prelude::*;
use serde_json::json;

fn event(
    sequence: u64,
    session: Option<String>,
    status: u16,
    fault: Option<String>,
) -> RequestRecord {
    RequestRecord {
        sequence,
        session_id: session,
        status,
        fault_injected: fault,
        ..RequestRecord::default()
    }
}

proptest! {
    #[test]
    fn cost_is_monotonic_in_token_counts(
        prompt_a in 0u64..100_000,
        completion_a in 0u64..100_000,
        prompt_delta in 0u64..100_000,
        completion_delta in 0u64..100_000,
    ) {
        let price = Price::new(
            MicroUsd::from_micro_usd(150_000),
            MicroUsd::from_micro_usd(600_000),
        );
        let first = TokenUsage {
            prompt_tokens: prompt_a,
            completion_tokens: completion_a,
            total_tokens: prompt_a.saturating_add(completion_a),
        };
        let second = TokenUsage {
            prompt_tokens: prompt_a.saturating_add(prompt_delta),
            completion_tokens: completion_a.saturating_add(completion_delta),
            total_tokens: prompt_a
                .saturating_add(prompt_delta)
                .saturating_add(completion_a)
                .saturating_add(completion_delta),
        };

        let first_cost = price.calculate(&first).unwrap();
        let second_cost = price.calculate(&second).unwrap();
        prop_assert!(second_cost >= first_cost);
    }

    #[test]
    fn request_transform_preserves_unrelated_fields(
        temperature in 0.0f64..2.0,
        request_id in "[a-z0-9]{1,20}",
    ) {
        let mut body = json!({
            "model": "gpt-4o-mini",
            "stream": true,
            "temperature": temperature,
            "metadata": {"request_id": request_id},
        });
        let original_metadata = body["metadata"].clone();

        include_stream_usage(&mut body).unwrap();

        prop_assert_eq!(&body["metadata"], &original_metadata);
        prop_assert_eq!(&body["stream_options"]["include_usage"], &json!(true));
    }

    #[test]
    fn recovery_events_never_exceed_fault_events_per_session(
        session_count in 1usize..5,
        steps in proptest::collection::vec(0u8..6, 1..12),
    ) {
        let sessions: Vec<String> = (0..session_count).map(|i| format!("s{i}")).collect();
        let mut requests = Vec::new();
        for (index, encoded) in steps.iter().enumerate() {
            let session = &sessions[usize::from(*encoded) % session_count];
            let kind = encoded % 3;
            let (status, fault) = match kind {
                0 => (500, Some("force_500".to_owned())),
                1 => (200, Some("malformed_tool_call_json".to_owned())),
                _ => (200, None),
            };
            requests.push(event(
                (index as u64) + 1,
                Some(session.clone()),
                status,
                fault,
            ));
        }

        let aggregation = aggregate_sessions(&requests);
        for session in &aggregation.sessions {
            prop_assert!(session.recovery_events <= session.fault_events);
        }
        prop_assert!(aggregation.recovery_events <= aggregation.fault_events);
        prop_assert!(aggregation.recovery_events <= aggregation.attributed_fault_events);
    }

    #[test]
    fn unrelated_sessions_cannot_affect_recovery(
        extra_successes in 0usize..5,
    ) {
        let mut requests = vec![event(
            1,
            Some("faulted".to_owned()),
            500,
            Some("force_500".to_owned()),
        )];
        for index in 0..extra_successes {
            requests.push(event(
                (index as u64) + 2,
                Some(format!("other-{index}")),
                200,
                None,
            ));
        }
        let aggregation = aggregate_sessions(&requests);
        prop_assert_eq!(aggregation.recovery_events, 0);
        prop_assert_eq!(aggregation.unrecovered_sessions, 1);
        prop_assert_eq!(aggregation.recovered_sessions, 0);
    }

    #[test]
    fn unattributed_requests_never_create_inferred_recovery(
        pair_count in 1usize..6,
    ) {
        let mut requests = Vec::new();
        for index in 0..pair_count {
            let sequence = (index as u64) * 2;
            requests.push(event(
                sequence + 1,
                None,
                500,
                Some("force_500".to_owned()),
            ));
            requests.push(event(sequence + 2, None, 200, None));
        }
        let aggregation = aggregate_sessions(&requests);
        prop_assert_eq!(aggregation.recovery_events, 0);
        prop_assert_eq!(aggregation.sessions_observed, 0);
        prop_assert_eq!(aggregation.unrecovered_sessions, 0);
        prop_assert_eq!(aggregation.unattributed_requests, (pair_count * 2) as u64);
    }
}
