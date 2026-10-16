use super::{Amount, Money, currency_precision};
use crate::error::{Result, require};
use chrono::NaiveDate;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AccountKind {
    Asset,
    Liability,
    Income,
    Expense,
    Equity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Account {
    pub id: Uuid,
    pub name: String,
    pub kind: AccountKind,
    pub currency: String,
    pub role: Option<String>,
    pub active: bool,
    pub version: i64,
    pub parent_id: Option<Uuid>,
    #[serde(default)]
    pub external_identifiers: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TransactionData {
    /// None means the occurrence date is unknown; the server never invents it.
    pub occurred_on: Option<NaiveDate>,
    pub occurred_at: Option<chrono::DateTime<chrono::FixedOffset>>,
    /// YYYY or YYYY-MM when the exact day is unknown.
    pub occurred_period: Option<String>,
    #[serde(default)]
    pub description: String,
    pub merchant: Option<String>,
    pub review_note: Option<String>,
    pub normalized_merchant: Option<String>,
    pub original_money: Option<Money>,
    pub settlement: Option<super::Settlement>,
    #[serde(default)]
    pub allocations: Vec<super::Allocation>,
    #[serde(default)]
    pub fees: Vec<super::Fee>,
    #[serde(default)]
    pub tags: Vec<Uuid>,
    #[serde(default)]
    pub identifiers: Vec<super::StrongIdentifier>,
    pub evidence_refs: Option<Vec<super::EvidenceRef>>,
    #[serde(default)]
    pub inferred_posting_accounts: Vec<Uuid>,
    pub movement: Movement,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Movement {
    Expense {
        payment_account_id: Option<Uuid>,
        money: Money,
        posted_on: NaiveDate,
    },
    Income {
        receipt_account_id: Uuid,
        money: Money,
        posted_on: NaiveDate,
    },
    Refund {
        original_transaction_id: Uuid,
        receipt_account_id: Uuid,
        money: Money,
        original_money: Money,
        refund_allocations: Vec<super::RefundAllocation>,
        posted_on: NaiveDate,
    },
    Transfer {
        payment_account_id: Uuid,
        receipt_account_id: Uuid,
        from_money: Money,
        to_money: Money,
        payment_posted_on: NaiveDate,
        receipt_posted_on: NaiveDate,
    },
    UnresolvedMovement {
        account_id: Uuid,
        money: Money,
        direction: Direction,
        posted_on: NaiveDate,
        #[serde(default)]
        transfer_in_transit: bool,
    },
    /// A confirmed balance at the close of as_of. Zero is a real baseline.
    Opening {
        account_id: Uuid,
        money: Money,
        as_of: NaiveDate,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    In,
    Out,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalLine {
    pub account_id: Uuid,
    pub currency: String,
    pub amount_minor: Amount,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedEntry {
    pub effective_on: NaiveDate,
    pub lines: Vec<JournalLine>,
}

pub fn system_account_id(ledger_id: Uuid, role: &str, currency: &str) -> Uuid {
    Uuid::new_v5(&ledger_id, format!("system:{role}:{currency}").as_bytes())
}

pub fn system_kind(role: &str) -> AccountKind {
    match role {
        "income" => AccountKind::Income,
        "expense" => AccountKind::Expense,
        "opening" => AccountKind::Equity,
        _ => AccountKind::Asset,
    }
}

pub const SYSTEM_ROLES: &[&str] = &[
    "income", "expense", "opening", "suspense", "transit", "exchange", "unknown",
];

impl PlannedEntry {
    pub fn validate(&self) -> Result<()> {
        require(
            self.lines.len() >= 2,
            "Each journal entry needs at least two nonzero lines",
        )?;
        let mut sums = BTreeMap::<&str, Amount>::new();
        for line in &self.lines {
            currency_precision(&line.currency)?;
            require(
                line.amount_minor != Amount::ZERO,
                "Zero journal lines are forbidden",
            )?;
            let total = sums.entry(&line.currency).or_default();
            *total = total.checked_add(line.amount_minor)?;
        }
        require(
            sums.values().all(|v| *v == Amount::ZERO),
            "Every currency must balance independently",
        )
    }
    pub fn reversed(&self) -> Result<Self> {
        Ok(Self {
            effective_on: self.effective_on,
            lines: self
                .lines
                .iter()
                .map(|l| {
                    Ok(JournalLine {
                        account_id: l.account_id,
                        currency: l.currency.clone(),
                        amount_minor: l.amount_minor.checked_neg()?,
                    })
                })
                .collect::<Result<_>>()?,
        })
    }
}

impl TransactionData {
    pub fn plan(
        &self,
        ledger_id: Uuid,
        accounts: &BTreeMap<Uuid, Account>,
    ) -> Result<Vec<PlannedEntry>> {
        require(
            self.description.len() <= 4096,
            "Description exceeds 4096 bytes",
        )?;
        require(
            self.review_note.as_ref().is_none_or(|s| s.len() <= 8192),
            "Review note exceeds 8192 bytes",
        )?;
        require(
            self.merchant.as_ref().is_none_or(|s| s.len() <= 512),
            "Merchant exceeds 512 bytes",
        )?;
        let actual = |id: Uuid, money: &Money| -> Result<()> {
            money.validate()?;
            let account = accounts.get(&id).ok_or(crate::error::Error::NotFound)?;
            require(
                account.role.is_none()
                    && matches!(account.kind, AccountKind::Asset | AccountKind::Liability),
                "Use a real asset or liability account",
            )?;
            require(account.active, "Account is inactive")?;
            require(
                account.currency == money.currency,
                "Account currency differs from transaction currency",
            )
        };
        let line = |id, money: &Money, negative: bool| -> Result<JournalLine> {
            Ok(JournalLine {
                account_id: id,
                currency: money.currency.clone(),
                amount_minor: if negative {
                    money.amount_minor.checked_neg()?
                } else {
                    money.amount_minor
                },
            })
        };
        let sys = |role, money: &Money| system_account_id(ledger_id, role, &money.currency);
        let pair = |date, a, b| PlannedEntry {
            effective_on: date,
            lines: vec![a, b],
        };
        let mut entries = match &self.movement {
            Movement::Expense {
                payment_account_id,
                money,
                posted_on,
            } => {
                if let Some(account_id) = payment_account_id {
                    actual(*account_id, money)?;
                }
                money.amount_minor.positive()?;
                vec![pair(
                    *posted_on,
                    line(sys("expense", money), money, false)?,
                    line(
                        payment_account_id.unwrap_or_else(|| sys("unknown", money)),
                        money,
                        true,
                    )?,
                )]
            }
            Movement::Income {
                receipt_account_id,
                money,
                posted_on,
            } => {
                actual(*receipt_account_id, money)?;
                money.amount_minor.positive()?;
                vec![pair(
                    *posted_on,
                    line(*receipt_account_id, money, false)?,
                    line(sys("income", money), money, true)?,
                )]
            }
            Movement::Opening {
                account_id,
                money,
                as_of,
            } => {
                actual(*account_id, money)?;
                if money.amount_minor == Amount::ZERO {
                    vec![]
                } else {
                    vec![pair(
                        *as_of,
                        line(*account_id, money, false)?,
                        line(sys("opening", money), money, true)?,
                    )]
                }
            }
            Movement::Refund {
                receipt_account_id,
                money,
                original_money,
                refund_allocations,
                posted_on,
                ..
            } => {
                actual(*receipt_account_id, money)?;
                original_money.validate()?;
                money.amount_minor.positive()?;
                original_money.amount_minor.positive()?;
                let mut total = Amount::ZERO;
                for allocation in refund_allocations {
                    allocation.amount_minor.positive()?;
                    total = total.checked_add(allocation.amount_minor)?;
                }
                require(
                    total == original_money.amount_minor,
                    "Refund allocations must equal refunded original principal",
                )?;
                if money.currency == original_money.currency {
                    require(
                        money.amount_minor == original_money.amount_minor,
                        "Same-currency refund principal must equal receipt",
                    )?;
                    vec![pair(
                        *posted_on,
                        line(*receipt_account_id, money, false)?,
                        line(sys("expense", original_money), original_money, true)?,
                    )]
                } else {
                    vec![PlannedEntry {
                        effective_on: *posted_on,
                        lines: vec![
                            line(*receipt_account_id, money, false)?,
                            line(sys("exchange", money), money, true)?,
                            line(sys("exchange", original_money), original_money, false)?,
                            line(sys("expense", original_money), original_money, true)?,
                        ],
                    }]
                }
            }
            Movement::UnresolvedMovement {
                account_id,
                money,
                direction,
                posted_on,
                transfer_in_transit,
            } => {
                actual(*account_id, money)?;
                money.amount_minor.positive()?;
                require(
                    !transfer_in_transit || *direction == Direction::Out,
                    "Transfer in transit requires a known outgoing movement",
                )?;
                let negative = *direction == Direction::Out;
                let role = if *transfer_in_transit {
                    "transit"
                } else {
                    "suspense"
                };
                vec![pair(
                    *posted_on,
                    line(*account_id, money, negative)?,
                    line(sys(role, money), money, !negative)?,
                )]
            }
            Movement::Transfer {
                payment_account_id,
                receipt_account_id,
                from_money,
                to_money,
                payment_posted_on,
                receipt_posted_on,
            } => {
                require(
                    payment_account_id != receipt_account_id,
                    "Transfer accounts must differ",
                )?;
                actual(*payment_account_id, from_money)?;
                actual(*receipt_account_id, to_money)?;
                from_money.amount_minor.positive()?;
                to_money.amount_minor.positive()?;
                let same_currency = from_money.currency == to_money.currency;
                require(
                    !same_currency || from_money.amount_minor == to_money.amount_minor,
                    "Same-currency principal must match; record fees as separate expenses",
                )?;
                require(
                    receipt_posted_on >= payment_posted_on,
                    "Receipt date cannot precede payment date",
                )?;
                let mut entries = vec![];
                let same_date = payment_posted_on == receipt_posted_on;
                if same_currency && same_date {
                    entries.push(pair(
                        *payment_posted_on,
                        line(*payment_account_id, from_money, true)?,
                        line(*receipt_account_id, to_money, false)?,
                    ));
                } else {
                    let outgoing_counterparty =
                        sys(if same_date { "exchange" } else { "transit" }, from_money);
                    entries.push(pair(
                        *payment_posted_on,
                        line(*payment_account_id, from_money, true)?,
                        line(outgoing_counterparty, from_money, false)?,
                    ));
                    if same_currency {
                        entries.push(pair(
                            *receipt_posted_on,
                            line(*receipt_account_id, to_money, false)?,
                            line(outgoing_counterparty, from_money, true)?,
                        ));
                    } else {
                        let mut incoming = vec![
                            line(*receipt_account_id, to_money, false)?,
                            line(sys("exchange", to_money), to_money, true)?,
                        ];
                        if !same_date {
                            incoming.push(line(sys("transit", from_money), from_money, true)?);
                            incoming.push(line(sys("exchange", from_money), from_money, false)?);
                        }
                        entries.push(PlannedEntry {
                            effective_on: *receipt_posted_on,
                            lines: incoming,
                        });
                    }
                }
                entries
            }
        };
        for fee in &self.fees {
            actual(fee.payment_account_id, &fee.money)?;
            fee.money.amount_minor.positive()?;
            entries.push(pair(
                fee.posted_on,
                line(sys("expense", &fee.money), &fee.money, false)?,
                line(fee.payment_account_id, &fee.money, true)?,
            ));
        }
        entries.sort_by_key(|p| (p.effective_on, p.lines.first().map(|l| l.account_id)));
        for entry in &entries {
            entry.validate()?;
        }
        Ok(entries)
    }

    pub fn opening_account(&self) -> Option<Uuid> {
        if let Movement::Opening { account_id, .. } = self.movement {
            Some(account_id)
        } else {
            None
        }
    }

    pub fn financial_accounts(&self) -> Vec<Uuid> {
        let mut ids = match self.movement {
            Movement::Expense {
                payment_account_id, ..
            } => payment_account_id.into_iter().collect(),
            Movement::Income {
                receipt_account_id, ..
            } => vec![receipt_account_id],
            Movement::Refund {
                receipt_account_id, ..
            } => vec![receipt_account_id],
            Movement::Transfer {
                payment_account_id,
                receipt_account_id,
                ..
            } => vec![payment_account_id, receipt_account_id],
            Movement::Opening { account_id, .. }
            | Movement::UnresolvedMovement { account_id, .. } => vec![account_id],
        };
        ids.extend(self.fees.iter().map(|f| f.payment_account_id));
        ids.sort();
        ids.dedup();
        ids
    }
}
