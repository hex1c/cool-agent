use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentConfig {
    pub schema_version: String,
    pub environment: Environment,
    pub company: CompanyConfig,
    pub quotation: QuotationConfig,
    pub drive: DriveConfig,
    pub calendar: CalendarConfig,
    pub email: EmailConfig,
    pub ai: AiConfig,
    pub attachments: AttachmentConfig,
    pub normalization: NormalizationConfig,
    pub timeouts: TimeoutConfig,
    pub retry: RetryConfig,
    pub links: LinkConfig,
    pub budget: BudgetConfig,
    pub telegram: TelegramConfig,
    pub google: GoogleConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Environment {
    Development,
    Staging,
    Production,
    /// Local real-provider development. Not deployed via SAM; secrets are
    /// resolved from the process environment (`.env`) rather than SSM.
    Local,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct CompanyConfig {
    pub legal_name: String,
    pub display_name: String,
    pub contact_email: String,
    pub contact_phone: String,
    pub address: String,
    pub logo_ref: String,
    pub signature_ref: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct QuotationConfig {
    pub template_version: String,
    pub reference_pdf: String,
    pub currency: String,
    pub tax_rate_bps: u32,
    pub validity_days: u32,
    pub number_prefix: String,
    pub number_padding: u8,
    pub terms: Vec<String>,
    pub field_mapping: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct DriveConfig {
    pub default_folder_id: String,
    pub shared_drive_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct CalendarConfig {
    pub timezone: String,
    pub default_calendar_id: String,
    pub default_reminder_minutes: u32,
    pub send_invitations_by_default: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct EmailConfig {
    pub host: String,
    pub port: u16,
    pub sender_address: String,
    pub secret_ref: String,
    pub link_expiry_days: u32,
    pub template_ref: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct AiConfig {
    pub provider: String,
    pub model_id: String,
    pub reasoning: String,
    pub vision: bool,
    pub secret_ref: String,
    pub prompt_version: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentConfig {
    pub max_count: u8,
    pub max_bytes: u64,
    pub allowed_mime_types: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct NormalizationConfig {
    pub max_image_pixels: u64,
    pub max_pdf_pages: u32,
    pub max_decompressed_bytes: u64,
    pub max_normalized_text_bytes: u64,
    #[serde(rename = "maxCsvRows")]
    pub max_csv_rows: u32,
    #[serde(rename = "maxCsvCells")]
    pub max_csv_cells: u64,
    #[serde(rename = "maxOfficeUncompressedBytes")]
    pub max_office_uncompressed_bytes: u64,
    #[serde(rename = "maxCompressionRatio")]
    pub max_compression_ratio: u64,
}

impl Default for NormalizationConfig {
    /// Conservative non-zero defaults used by unit tests. Production config
    /// is always loaded and semantically validated by the config loader.
    fn default() -> Self {
        Self {
            max_image_pixels: 16_777_216,
            max_pdf_pages: 50,
            max_decompressed_bytes: 52_428_800,
            max_normalized_text_bytes: 65_536,
            max_csv_rows: 10_000,
            max_csv_cells: 100_000,
            max_office_uncompressed_bytes: 52_428_800,
            max_compression_ratio: 100,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct TimeoutConfig {
    pub collection_seconds: u32,
    pub clarification_seconds: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct RetryConfig {
    pub max_attempts: u8,
    pub backoff_base_ms: u32,
    pub backoff_max_ms: u32,
    pub jitter_bps: u16,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct LinkConfig {
    pub s3_expiry_seconds: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct BudgetConfig {
    pub monthly_cap_micro_inr: u64,
    pub warning_threshold_micro_inr: u64,
    pub suspension_threshold_micro_inr: u64,
    pub operational_reserve_micro_inr: u64,
    pub safety_margin_bps: u16,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct TelegramConfig {
    pub bot_username: String,
    pub forum_chat_id: i64,
    pub bot_token_ref: String,
    pub webhook_secret_ref: String,
    pub privacy_mode: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct GoogleConfig {
    pub client_id: String,
    pub client_secret_ref: String,
    pub callback_uri: String,
    pub scopes: Vec<String>,
}
