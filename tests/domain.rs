use chrono::NaiveDate;
use std::collections::BTreeMap;
use sumi::{domain::*, error::Error};
use uuid::Uuid;

fn date(value: &str) -> NaiveDate {
    value.parse().unwrap()
}
fn money(currency: &str, amount: &str) -> Money {
    Money {
        currency: currency.into(),
        amount_minor: amount.parse().unwrap(),
    }
}
fn account(currency: &str, kind: AccountKind) -> Account {
    Account {
        id: Uuid::new_v4(),
        name: "test".into(),
        kind,
        currency: currency.into(),
        role: None,
        active: true,
        version: 1,
        parent_id: None,
        external_identifiers: BTreeMap::new(),
    }
}
fn transaction(movement: Movement) -> TransactionData {
    serde_json::from_value(serde_json::json!({"occurred_on":"2026-01-30","movement":movement}))
        .unwrap()
}

#[test]
fn money_rejects_fraction_float_and_overflow() {
    for value in ["", "1.0", "1e2", "+1", " 1", "NaN", "--1"] {
        assert!(value.parse::<Amount>().is_err(), "{value}");
    }
    for value in [
        "100000000000000000000000000000000000000",
        "-100000000000000000000000000000000000000",
    ] {
        assert!(matches!(value.parse::<Amount>(), Err(Error::Overflow)));
    }
    let max = Amount::new(Amount::MAX).unwrap();
    assert!(max.checked_add("1".parse().unwrap()).is_err());
    assert_eq!(
        serde_json::to_string(&max).unwrap(),
        format!("\"{}\"", Amount::MAX)
    );
    assert!(serde_json::from_str::<Amount>("100").is_err());
    assert_eq!(currency_precision("JPY").unwrap(), 0);
    assert_eq!(currency_precision("KWD").unwrap(), 3);
    assert!(currency_precision("ZZZ").is_err());
}

#[test]
fn expenses_credit_card_and_reversal_balance() {
    let ledger = Uuid::new_v4();
    let cc = account("CNY", AccountKind::Liability);
    let accounts = BTreeMap::from([(cc.id, cc.clone())]);
    let t = transaction(Movement::Expense {
        payment_account_id: Some(cc.id),
        money: money("CNY", "10000"),
        posted_on: date("2026-01-31"),
    });
    let plan = t.plan(ledger, &accounts).unwrap();
    assert_eq!(plan[0].lines[1].amount_minor.value(), -10000);
    assert_eq!(plan[0].effective_on, date("2026-01-31"));
    let reverse = plan[0].reversed().unwrap();
    assert_eq!(reverse.effective_on, plan[0].effective_on);
    assert_eq!(reverse.lines[1].amount_minor.value(), 10000);
    reverse.validate().unwrap();
}

#[test]
fn cross_month_fx_preserves_transit_and_each_day_currency_balance() {
    let ledger = Uuid::new_v4();
    let cny = account("CNY", AccountKind::Asset);
    let usd = account("USD", AccountKind::Asset);
    let accounts = BTreeMap::from([(cny.id, cny.clone()), (usd.id, usd.clone())]);
    let t = transaction(Movement::Transfer {
        payment_account_id: cny.id,
        receipt_account_id: usd.id,
        from_money: money("CNY", "72000"),
        to_money: money("USD", "10000"),
        payment_posted_on: date("2026-01-31"),
        receipt_posted_on: date("2026-02-02"),
    });
    let plan = t.plan(ledger, &accounts).unwrap();
    assert_eq!(plan.len(), 2);
    let transit = system_account_id(ledger, "transit", "CNY");
    let jan = plan
        .iter()
        .filter(|p| p.effective_on < date("2026-02-01"))
        .flat_map(|p| &p.lines)
        .filter(|l| l.account_id == transit)
        .map(|l| l.amount_minor.value())
        .sum::<i128>();
    let total = plan
        .iter()
        .flat_map(|p| &p.lines)
        .filter(|l| l.account_id == transit)
        .map(|l| l.amount_minor.value())
        .sum::<i128>();
    assert_eq!(jan, 72000);
    assert_eq!(total, 0);
    for p in plan {
        p.validate().unwrap();
    }
}

#[test]
fn same_currency_transfer_rejects_hidden_fees() {
    let ledger = Uuid::new_v4();
    let a = account("CNY", AccountKind::Asset);
    let b = account("CNY", AccountKind::Asset);
    let accounts = BTreeMap::from([(a.id, a.clone()), (b.id, b.clone())]);
    let t = transaction(Movement::Transfer {
        payment_account_id: a.id,
        receipt_account_id: b.id,
        from_money: money("CNY", "10000"),
        to_money: money("CNY", "9900"),
        payment_posted_on: date("2026-01-31"),
        receipt_posted_on: date("2026-01-31"),
    });
    assert!(t.plan(ledger, &accounts).is_err());
}

#[test]
fn same_currency_cross_day_transfer_closes_transit_on_receipt_date() {
    let ledger = Uuid::new_v4();
    let from = account("CNY", AccountKind::Asset);
    let to = account("CNY", AccountKind::Asset);
    let accounts = BTreeMap::from([(from.id, from.clone()), (to.id, to.clone())]);
    let plan = transaction(Movement::Transfer {
        payment_account_id: from.id,
        receipt_account_id: to.id,
        from_money: money("CNY", "100"),
        to_money: money("CNY", "100"),
        payment_posted_on: date("2026-01-31"),
        receipt_posted_on: date("2026-02-02"),
    })
    .plan(ledger, &accounts)
    .unwrap();
    assert_eq!(plan.len(), 2);
    let transit = system_account_id(ledger, "transit", "CNY");
    assert_eq!(
        plan[0]
            .lines
            .iter()
            .find(|l| l.account_id == transit)
            .unwrap()
            .amount_minor
            .value(),
        100
    );
    assert_eq!(
        plan[1]
            .lines
            .iter()
            .find(|l| l.account_id == transit)
            .unwrap()
            .amount_minor
            .value(),
        -100
    );
    assert_eq!(plan[1].effective_on, date("2026-02-02"));
}

#[test]
fn zero_opening_is_a_confirmed_baseline_without_zero_lines() {
    let a = account("JPY", AccountKind::Asset);
    let accounts = BTreeMap::from([(a.id, a.clone())]);
    let t = transaction(Movement::Opening {
        account_id: a.id,
        money: money("JPY", "0"),
        as_of: date("2026-01-01"),
    });
    assert!(t.plan(Uuid::new_v4(), &accounts).unwrap().is_empty());
}

#[test]
fn raw_unbalanced_multi_currency_journal_is_rejected() {
    let entry = PlannedEntry {
        effective_on: date("2026-01-01"),
        lines: vec![
            JournalLine {
                account_id: Uuid::new_v4(),
                currency: "CNY".into(),
                amount_minor: "100".parse().unwrap(),
            },
            JournalLine {
                account_id: Uuid::new_v4(),
                currency: "USD".into(),
                amount_minor: "-100".parse().unwrap(),
            },
        ],
    };
    assert!(entry.validate().is_err());
}

#[test]
fn unknown_fields_cannot_be_silently_ignored() {
    let request = serde_json::json!({"ledger_id":Uuid::new_v4(),"idempotency_key":"test","actions":[{"op":"create","transaction":{"occurred_on":"2026-01-01","description":"","merchant":null,"review_note":null,"unexpected_field":true,"movement":{"kind":"expense","payment_account_id":Uuid::new_v4(),"money":{"currency":"CNY","amount_minor":"100"},"posted_on":"2026-01-01"}}}]});
    assert!(serde_json::from_value::<sumi::api::TransactionsApply>(request).is_err());
}

#[test]
fn fx_uses_half_even_without_float_and_checks_final_range() {
    for (amount, expected) in [("1", "0"), ("3", "2"), ("5", "2"), ("-3", "-2")] {
        assert_eq!(
            convert(amount.parse().unwrap(), "JPY", "USD", "0.005")
                .unwrap()
                .to_string(),
            expected
        );
    }
    let max = Amount::new(Amount::MAX).unwrap();
    assert_eq!(convert(max, "USD", "CNY", "1").unwrap(), max);
    assert!(matches!(
        convert(max, "USD", "CNY", "2"),
        Err(Error::Overflow)
    ));
    for rate in ["0", "-1", "NaN", "1e2", ".5", "1."] {
        assert!(parse_rate(rate).is_err(), "{rate}");
    }
}

#[test]
fn apportionment_is_order_independent_and_nonnegative_at_large_values() {
    let mut allocations = (1..=4)
        .map(|n| (Uuid::from_u128(n), "1".parse::<Amount>().unwrap()))
        .collect::<Vec<_>>();
    let small = apportion("2".parse().unwrap(), &allocations).unwrap();
    allocations.reverse();
    assert_eq!(
        apportion("2".parse().unwrap(), &allocations).unwrap(),
        small
    );
    assert_eq!(small[&Uuid::from_u128(1)].value(), 1);
    assert_eq!(small[&Uuid::from_u128(2)].value(), 1);
    assert_eq!(small[&Uuid::from_u128(3)], Amount::ZERO);
    let large = apportion(Amount::new(Amount::MAX).unwrap(), &allocations).unwrap();
    assert!(large.values().all(|a| *a >= Amount::ZERO));
    assert_eq!(
        large
            .values()
            .try_fold(Amount::ZERO, |a, b| a.checked_add(*b))
            .unwrap()
            .value(),
        Amount::MAX
    );
}
