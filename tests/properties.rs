use maul::budget::{MicroUsd, Price};
use maul::openai::TokenUsage;
use maul::proxy::request_transform::include_stream_usage;
use proptest::prelude::*;
use serde_json::json;

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
}
