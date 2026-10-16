use super::{Amount, Direction, Money};
use chrono::NaiveDate;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Allocation {
    pub allocation_id: Option<Uuid>,
    pub category_id: Option<Uuid>,
    pub amount_minor: Amount,
    #[serde(default)]
    pub tags: Vec<Uuid>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Fee {
    pub fee_id: Option<Uuid>,
    pub payment_account_id: Uuid,
    pub money: Money,
    pub posted_on: NaiveDate,
    pub occurred_on: Option<NaiveDate>,
    #[serde(default)]
    pub allocations: Vec<Allocation>,
    pub settlement: Option<Settlement>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RefundAllocation {
    pub allocation_id: Uuid,
    pub amount_minor: Amount,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Settlement {
    pub money: Money,
    pub source_version_id: Uuid,
    pub record_locator: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StrongIdentifier {
    pub namespace: String,
    pub account_id: Uuid,
    pub event_type: String,
    pub external_id: String,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceRole {
    #[default]
    Movement,
    Context,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EvidenceRef {
    pub observation_id: Uuid,
    pub version: i64,
    pub account_id: Option<Uuid>,
    pub amount_minor: Option<Amount>,
    #[serde(default)]
    pub role: EvidenceRole,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CaptureKind {
    Original,
    Excerpt,
    AiNormalized,
    UserStatement,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Coverage {
    Complete,
    Partial,
    Unknown,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BalanceKind {
    Booked,
    Available,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ControlCount {
    pub value: u32,
    pub locator: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ControlAmount {
    pub value: Amount,
    pub locator: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Statement {
    pub account_id: Uuid,
    pub currency: String,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub opening_balance: Option<Amount>,
    pub closing_balance: Option<Amount>,
    pub expected_count: Option<ControlCount>,
    pub expected_debit_total: Option<ControlAmount>,
    pub expected_credit_total: Option<ControlAmount>,
    pub source_coverage: Coverage,
    /// Explicit complete movement locator set, backed by a locator in the archived source.
    pub record_locators: Option<Vec<String>>,
    pub range_locator: Option<String>,
    #[serde(default)]
    pub extraction_complete: bool,
    #[serde(default)]
    pub known_gaps: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ObservationPayload {
    Movement {
        account_id: Option<Uuid>,
        money: Option<Money>,
        direction: Option<Direction>,
        posted_on: Option<NaiveDate>,
        #[serde(default)]
        posting_date_inferred: bool,
    },
    Order {
        money: Option<Money>,
        description: String,
    },
    Authorization {
        money: Option<Money>,
        description: String,
    },
    Balance {
        account_id: Uuid,
        currency: String,
        amount_minor: Amount,
        as_of: NaiveDate,
        balance_kind: BalanceKind,
    },
    Statement {
        #[serde(flatten)]
        statement: Box<Statement>,
    },
    FxQuote {
        from_currency: String,
        to_currency: String,
        on: NaiveDate,
        rate: String,
    },
}
impl ObservationPayload {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Movement { .. } => "movement",
            Self::Order { .. } => "order",
            Self::Authorization { .. } => "authorization",
            Self::Balance { .. } => "balance",
            Self::Statement { .. } => "statement",
            Self::FxQuote { .. } => "fx_quote",
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CategoryKind {
    Income,
    Expense,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Category {
    pub id: Uuid,
    pub name: String,
    pub kind: CategoryKind,
    pub parent_id: Option<Uuid>,
    pub active: bool,
    pub version: i64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Tag {
    pub id: Uuid,
    pub name: String,
    pub active: bool,
    pub version: i64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClassificationRule {
    pub merchant_contains: String,
    pub kind: CategoryKind,
    pub category_id: Uuid,
    #[serde(default)]
    pub tags: Vec<Uuid>,
}
