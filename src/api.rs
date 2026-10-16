use crate::domain::{AccountKind, Money, TransactionData};
use chrono::NaiveDate;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
mod imports;
pub use imports::*;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContextRequest {}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AccountsList {
    pub ledger_id: Uuid,
    pub as_of: Option<NaiveDate>,
    #[serde(default)]
    pub include_system: bool,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AccountsUpsert {
    pub ledger_id: Uuid,
    pub idempotency_key: String,
    pub account_id: Option<Uuid>,
    pub expected_version: Option<i64>,
    pub name: String,
    pub kind: AccountKind,
    pub currency: String,
    pub parent_id: Option<Uuid>,
    #[serde(default)]
    pub external_identifiers: std::collections::BTreeMap<String, String>,
    #[serde(default = "enabled")]
    pub active: bool,
}
fn enabled() -> bool {
    true
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TransactionsApply {
    pub ledger_id: Uuid,
    pub idempotency_key: String,
    #[serde(default)]
    pub preview: bool,
    pub actions: Vec<Action>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Create {
        transaction: TransactionData,
    },
    /// Full replacement. Existing identity and version must be read first.
    Revise {
        transaction_id: Uuid,
        expected_version: i64,
        reason: String,
        transaction: TransactionData,
    },
    Void {
        transaction_id: Uuid,
        expected_version: i64,
        reason: String,
    },
    /// as_of is the day-end balance preceding the tracking period.
    SetOpening {
        account_id: Uuid,
        money: Money,
        as_of: NaiveDate,
        transaction_id: Option<Uuid>,
        expected_version: Option<i64>,
        reason: String,
    },
    LinkEvidence {
        transaction_id: Uuid,
        expected_version: i64,
        reason: String,
        evidence_refs: Vec<crate::domain::EvidenceRef>,
    },
    UnlinkEvidence {
        transaction_id: Uuid,
        expected_version: i64,
        reason: String,
        observation_ids: Vec<Uuid>,
    },
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TransactionsSearch {
    pub ledger_id: Uuid,
    pub occurred_from: Option<NaiveDate>,
    /// Exclusive upper bound.
    pub occurred_before: Option<NaiveDate>,
    pub account_id: Option<Uuid>,
    pub merchant: Option<String>,
    pub currency: Option<String>,
    pub amount_minor: Option<crate::domain::Amount>,
    pub category_id: Option<Uuid>,
    pub tag_id: Option<Uuid>,
    #[serde(default)]
    pub include_voided: bool,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TransactionsGet {
    pub ledger_id: Uuid,
    pub transaction_id: Uuid,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationsGet {
    pub ledger_id: Uuid,
    pub operation_id: Option<Uuid>,
    pub idempotency_key: Option<String>,
    pub tool: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReportsGet {
    pub ledger_id: Uuid,
    pub report: ReportKind,
    pub from: Option<NaiveDate>,
    /// Exclusive upper bound, using occurrence dates for income/expense.
    pub before: Option<NaiveDate>,
    pub account_id: Option<Uuid>,
    pub currency: Option<String>,
    pub merchant: Option<String>,
    pub category_id: Option<Uuid>,
    pub tag_id: Option<Uuid>,
    pub display_currency: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReportKind {
    IncomeExpense,
    AccountBalances,
    DataQuality,
    ByCategory,
    Export,
}
