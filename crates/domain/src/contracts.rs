use serde::{Deserialize, Serialize};

pub const CONTRACT_VERSION: &str = "novus.ai.v2";

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct AiRequest {
    pub contract_version: String,
    pub request_id: String,
    pub workflow_id: String,
    pub operation: Operation,
    pub model: ModelVersion,
    pub history: HistoryCheckpoint,
    pub input: AiInput,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Extraction,
    QuotationCalculation,
    Draft,
    Calendar,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct ModelVersion {
    pub provider: String,
    pub model_id: String,
    pub prompt_version: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct HistoryCheckpoint {
    pub sequence: u64,
    pub history_digest: String,
    pub sanitized_object_ref: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct AiInput {
    pub instruction: String,
    pub attachments: Vec<AttachmentInput>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentInput {
    pub object_ref: String,
    pub media_type: String,
    pub checksum: String,
    pub extracted_text: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct AiResponse {
    pub contract_version: String,
    pub request_id: String,
    pub outcome: Outcome,
    pub model: ModelVersion,
    pub checkpoint: HistoryCheckpoint,
    pub extraction: Option<ExtractionResult>,
    pub quotation: Option<QuotationResult>,
    pub draft: Option<DraftResult>,
    pub calendar: Option<CalendarResult>,
    pub error: Option<TypedError>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Success,
    Invalid,
    RetryableError,
    TerminalError,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct ExtractionResult {
    pub customer: CustomerData,
    pub items: Vec<LineItem>,
    pub currency: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct CustomerData {
    pub name: String,
    pub address: Option<String>,
    pub contact: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct LineItem {
    pub description: String,
    pub quantity: String,
    pub unit_price_micro_inr: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct QuotationResult {
    pub currency: String,
    pub subtotal_micro_inr: i64,
    pub tax_micro_inr: i64,
    pub total_micro_inr: i64,
    pub tax_rate_bps: u32,
    pub assumptions: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct DraftResult {
    pub subject: String,
    pub body: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct CalendarResult {
    pub title: String,
    pub start: String,
    pub end: String,
    pub timezone: Option<String>,
    pub calendar_id: Option<String>,
    pub description: Option<String>,
    pub attendees: Vec<String>,
    pub reminders: Option<CalendarReminderResult>,
    pub send_invitations: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct CalendarReminderResult {
    pub push_minutes: Option<u16>,
    pub email_minutes: Option<u16>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct TypedError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::{AiRequest, AiResponse, CONTRACT_VERSION};
    use serde_json::Value;
    #[test]
    fn request_fixture_round_trips_through_rust_types() {
        let fixture: Value =
            serde_json::from_str(include_str!("../../../tests/fixtures/contracts/v2.json"))
                .expect("contract fixture must be JSON");
        let request: AiRequest = serde_json::from_value(fixture["request"].clone())
            .expect("request fixture must match Rust contract");
        let response: AiResponse = serde_json::from_value(fixture["response"].clone())
            .expect("response fixture must match Rust contract");

        assert_eq!(request.contract_version, CONTRACT_VERSION);
        assert_eq!(response.contract_version, CONTRACT_VERSION);

        let request_schema: Value = serde_json::from_str(include_str!(
            "../../../config/schema/ai-request.schema.json"
        ))
        .expect("request schema must be JSON");
        let response_schema: Value = serde_json::from_str(include_str!(
            "../../../config/schema/ai-response.schema.json"
        ))
        .expect("response schema must be JSON");
        let request_validator = jsonschema::options()
            .with_draft(jsonschema::Draft::Draft202012)
            .build(&request_schema)
            .expect("request schema must compile");
        let response_validator = jsonschema::options()
            .with_draft(jsonschema::Draft::Draft202012)
            .build(&response_schema)
            .expect("response schema must compile");

        assert!(request_validator.is_valid(&fixture["request"]));
        assert!(response_validator.is_valid(&fixture["response"]));
    }
}
