//! Fault engine behavior (seed + probability + scenarios).

use maul::config::{Budget, Config};
use maul::fault::{Action, FORCE_500, FaultEngine};

fn config_with(scenarios: Vec<&str>, probability: f64, seed: u64) -> Config {
    Config {
        proxy_listen: "127.0.0.1:7777".into(),
        upstream_base_url: "https://api.openai.com".into(),
        scenarios: scenarios.into_iter().map(str::to_owned).collect(),
        probability,
        seed,
        budget: Budget {
            max_llm_calls: 100,
            max_cost_usd: 5.0,
        },
    }
}

#[test]
fn disabled_when_scenario_not_listed() {
    let engine = FaultEngine::from_config(&config_with(vec![], 1.0, 1));
    for _ in 0..20 {
        assert!(matches!(engine.decide(), Action::Forward));
    }
}

#[test]
fn disabled_when_probability_zero() {
    let engine = FaultEngine::from_config(&config_with(vec![FORCE_500], 0.0, 1));
    for _ in 0..20 {
        assert!(matches!(engine.decide(), Action::Forward));
    }
}

#[test]
fn always_injects_when_probability_one() {
    let engine = FaultEngine::from_config(&config_with(vec![FORCE_500], 1.0, 7));
    for _ in 0..10 {
        match engine.decide() {
            Action::ShortCircuit(response) => {
                assert_eq!(
                    response.status(),
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR
                );
            }
            Action::Forward => panic!("expected ShortCircuit"),
        }
    }
}

#[test]
fn same_seed_same_sequence() {
    let a = FaultEngine::from_config(&config_with(vec![FORCE_500], 0.5, 99));
    let b = FaultEngine::from_config(&config_with(vec![FORCE_500], 0.5, 99));

    let seq_a: Vec<bool> = (0..32)
        .map(|_| matches!(a.decide(), Action::ShortCircuit(_)))
        .collect();
    let seq_b: Vec<bool> = (0..32)
        .map(|_| matches!(b.decide(), Action::ShortCircuit(_)))
        .collect();

    assert_eq!(seq_a, seq_b);
    assert!(seq_a.iter().any(|injected| *injected));
    assert!(seq_a.iter().any(|injected| !*injected));
}
