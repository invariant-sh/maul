use std::collections::HashMap;

use maul::budget::{MicroUsd, Price};
use maul::pricing::{PricingRegistry, REGISTRY_VERSION};

#[test]
fn bundled_registry_contains_supported_models() {
    let registry = PricingRegistry::with_overrides(&HashMap::new());
    assert!(registry.lookup("gpt-4o-mini").is_some());
    assert!(registry.lookup("gpt-4o").is_some());
    assert_eq!(registry.version(), REGISTRY_VERSION);
}

#[test]
fn explicit_override_wins_over_bundled_price() {
    let override_price = Price::new(MicroUsd::from_micro_usd(1), MicroUsd::from_micro_usd(2));
    let overrides = HashMap::from([("gpt-4o-mini".to_owned(), override_price)]);
    let registry = PricingRegistry::with_overrides(&overrides);

    assert_eq!(registry.lookup("gpt-4o-mini"), Some(override_price));
}

#[test]
fn unknown_models_are_not_priced_implicitly() {
    let registry = PricingRegistry::with_overrides(&HashMap::new());
    assert_eq!(registry.lookup("custom-gateway-model"), None);
}
