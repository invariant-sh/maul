use std::sync::Arc;
use std::thread;

use maul::budget::{BudgetAdmission, BudgetLimits, BudgetTracker, MicroUsd, Price};
use maul::openai::TokenUsage;

fn tracker(max_calls: u64, max_cost: u64) -> BudgetTracker {
    BudgetTracker::new(BudgetLimits {
        max_llm_calls: max_calls,
        max_cost_usd: MicroUsd::from_micro_usd(max_cost),
    })
}

#[test]
fn admits_exactly_the_call_limit() {
    let budget = tracker(2, 0);
    assert!(matches!(
        budget.admit(),
        BudgetAdmission::Allowed(permit) if permit.call_number == 1
    ));
    assert!(matches!(
        budget.admit(),
        BudgetAdmission::Allowed(permit) if permit.call_number == 2
    ));
    assert!(matches!(
        budget.admit(),
        BudgetAdmission::CallCapExceeded {
            calls_reserved: 2,
            calls_limit: 2
        }
    ));
}

#[test]
fn cost_cap_blocks_after_observed_cost_reaches_limit() {
    let budget = tracker(10, 100);
    assert!(matches!(budget.admit(), BudgetAdmission::Allowed(_)));
    budget.commit_cost(MicroUsd::from_micro_usd(100));
    assert!(matches!(
        budget.admit(),
        BudgetAdmission::CostCapExceeded {
            observed_cost_usd,
            cost_limit_usd
        } if observed_cost_usd == cost_limit_usd
    ));
    assert_eq!(budget.snapshot().calls_reserved, 1);
}

#[test]
fn zero_cost_limit_disables_cost_admission() {
    let budget = tracker(2, 0);
    budget.commit_cost(MicroUsd::from_micro_usd(10_000_000));
    assert!(matches!(budget.admit(), BudgetAdmission::Allowed(_)));
}

#[test]
fn concurrent_admission_never_exceeds_call_limit() {
    let budget = Arc::new(tracker(10, 0));
    let results = thread::scope(|scope| {
        let handles = (0..50)
            .map(|_| {
                let budget = Arc::clone(&budget);
                scope.spawn(move || budget.admit())
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("worker"))
            .collect::<Vec<_>>()
    });

    let allowed = results
        .iter()
        .filter(|result| matches!(result, BudgetAdmission::Allowed(_)))
        .count();
    assert_eq!(allowed, 10);
    assert_eq!(budget.snapshot().calls_reserved, 10);
}

#[test]
fn price_calculation_is_deterministic_and_rounded() {
    let price = Price::new(
        MicroUsd::from_micro_usd(150_000),
        MicroUsd::from_micro_usd(600_000),
    );
    let usage = TokenUsage {
        prompt_tokens: 1_000,
        completion_tokens: 500,
        total_tokens: 1_500,
    };

    assert_eq!(
        price.calculate(&usage).unwrap(),
        MicroUsd::from_micro_usd(450)
    );
}

#[test]
fn budget_clone_shares_atomic_state() {
    let original = tracker(2, 1_000);
    let clone = original.clone();
    clone.commit_cost(MicroUsd::from_micro_usd(100));
    assert_eq!(
        original.snapshot().observed_cost_usd,
        MicroUsd::from_micro_usd(100)
    );
}

#[test]
fn committed_cost_saturates_instead_of_wrapping() {
    let budget = tracker(2, 0);
    budget.commit_cost(MicroUsd::from_micro_usd(u64::MAX));
    budget.commit_cost(MicroUsd::from_micro_usd(1));
    assert_eq!(
        budget.snapshot().observed_cost_usd,
        MicroUsd::from_micro_usd(u64::MAX)
    );
}

#[test]
fn price_calculation_reports_overflow() {
    let price = Price::new(
        MicroUsd::from_micro_usd(u64::MAX),
        MicroUsd::from_micro_usd(u64::MAX),
    );
    let usage = TokenUsage {
        prompt_tokens: u64::MAX,
        completion_tokens: u64::MAX,
        total_tokens: u64::MAX,
    };

    assert!(price.calculate(&usage).is_err());
}
