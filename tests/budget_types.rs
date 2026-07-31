use maul::budget::{MicroUsd, MicroUsdError};
use rust_decimal::Decimal;

#[test]
fn converts_decimal_dollars_to_exact_micro_usd() {
    let amount = MicroUsd::try_from(Decimal::new(150, 2)).unwrap();
    assert_eq!(amount, MicroUsd::from_micro_usd(1_500_000));
    assert_eq!(amount.to_string(), "$1.500000");
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
