use super::*;
use crate::domain::*;
use std::collections::BTreeMap;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourcesIngest {
    pub ledger_id: Uuid,
    pub idempotency_key: String,
    pub sources: Vec<SourceInput>,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceInput {
    pub provider: String,
    pub source_scope: String,
    pub external_id: String,
    pub origin_uri: Option<String>,
    pub body: String,
    pub capture_kind: CaptureKind,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourcesGet {
    pub ledger_id: Uuid,
    pub source_version_id: Option<Uuid>,
    pub source_id: Option<Uuid>,
    pub record_locator: Option<String>,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ObservationsIngest {
    pub ledger_id: Uuid,
    pub idempotency_key: String,
    pub observations: Vec<ObservationInput>,
    pub batch: Option<BatchInput>,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BatchInput {
    pub id: Option<Uuid>,
    pub expected_version: Option<i64>,
    pub name: String,
    pub source_version_ids: Vec<Uuid>,
    pub complete: bool,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ObservationInput {
    pub observation_id: Option<Uuid>,
    pub expected_version: Option<i64>,
    pub source_version_id: Option<Uuid>,
    pub record_locator: Option<String>,
    pub payload: Option<ObservationPayload>,
    pub references: Option<Vec<String>>,
    pub extractor: Option<String>,
    pub reason: String,
    pub review_note: Option<String>,
    pub ignored_reason: Option<String>,
    #[serde(default)]
    pub clear_review_note: bool,
    #[serde(default)]
    pub clear_ignored_reason: bool,
    /// Confirm a genuinely new row when importing a newer version of an existing source.
    #[serde(default)]
    pub new_record_confirmed: bool,
    #[serde(default)]
    pub clear_controls: Vec<String>,
    pub control_correction_locator: Option<String>,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ObservationsSearch {
    pub ledger_id: Uuid,
    pub source_id: Option<Uuid>,
    pub observation_id: Option<Uuid>,
    #[serde(default)]
    pub include_history: bool,
    pub source_version_id: Option<Uuid>,
    pub account_id: Option<Uuid>,
    pub observation_type: Option<String>,
    #[serde(default)]
    pub pending_only: bool,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FxRatesIngest {
    pub ledger_id: Uuid,
    pub idempotency_key: String,
    pub rates: Vec<FxRateInput>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FxRateInput {
    pub from_currency: String,
    pub to_currency: String,
    pub on: NaiveDate,
    pub rate: String,
    pub source_version_id: Uuid,
    pub record_locator: String,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FxRatesQuery {
    pub ledger_id: Uuid,
    pub from_currency: Option<String>,
    pub to_currency: Option<String>,
    pub on: Option<NaiveDate>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PreferencesUpdate {
    pub ledger_id: Uuid,
    pub idempotency_key: String,
    pub expected_version: i64,
    pub reason: String,
    pub reporting_currency: Option<String>,
    pub timezone: Option<String>,
    pub categories: Option<Vec<CategoryInput>>,
    pub tags: Option<Vec<TagInput>>,
    pub merchant_aliases: Option<BTreeMap<String, String>>,
    pub classification_rules: Option<Vec<ClassificationRule>>,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CategoryInput {
    pub id: Option<Uuid>,
    pub expected_version: Option<i64>,
    pub name: String,
    pub kind: CategoryKind,
    pub parent_id: Option<Uuid>,
    #[serde(default = "enabled")]
    pub active: bool,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TagInput {
    pub id: Option<Uuid>,
    pub expected_version: Option<i64>,
    pub name: String,
    #[serde(default = "enabled")]
    pub active: bool,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReconcileRequest {
    pub ledger_id: Uuid,
    pub account_id: Uuid,
    pub currency: String,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub statement_observation_id: Uuid,
    pub statement_version: i64,
}
