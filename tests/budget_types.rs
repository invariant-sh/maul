use maul::budget::{MicroUsd, MicroUsdError};
use rust_decimal::Decimal;

#[test]
fn converts_decimal_dollars_to_exact_micro_usd() {
    let amount = MicroUsd::try_from(Decimal::new(150, 2)).unwrap();
    assert_eq!(amount, MicroUsd::from_micro_usd(1_500_000));
    assert_eq!(amount.to_string(), "$1.500000");
}

#[test]
fn serializes_micro_usd_with_display_string() {
    let amount = MicroUsd::from_micro_usd(1_500_000);
    let value = serde_json::to_value(amount).unwrap();
    assert_eq!(value["micro_usd"], 1_500_000);
    assert_eq!(value["display"], "$1.500000");
}

#[test]
fn deserializes_legacy_integer_and_object_forms() {
    let legacy: MicroUsd = serde_json::from_str("42").unwrap();
    assert_eq!(legacy, MicroUsd::from_micro_usd(42));

    let object: MicroUsd =
        serde_json::from_str(r#"{"micro_usd":1500000,"display":"$1.500000"}"#).unwrap();
    assert_eq!(object, MicroUsd::from_micro_usd(1_500_000));
}

#[test]
fn rejects_negative_money() {
    let error = MicroUsd::try_from(Decimal::new(-1, 0)).unwrap_err();
    assert_eq!(error, MicroUsdError::Negative);
}

#[test]
fn rejects_sub_micro_precision() {
    let amount = Decimal::new(1, 7);
    let error = MicroUsd::try_from(amount).unwrap_err();
    assert_eq!(error, MicroUsdError::TooPrecise(amount));
}

#[test]
fn preserves_zero() {
    assert_eq!(MicroUsd::try_from(Decimal::ZERO).unwrap(), MicroUsd::ZERO);
}
