use std::fmt::{Display, Formatter};

use serde::{Deserialize, Serialize};

use crate::identity::AttachmentId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    Image,
    Pdf,
    Csv,
    Word,
    Excel,
}

impl AttachmentKind {
    pub fn from_media_type(media_type: &str) -> Result<Self, AttachmentError> {
        match normalized_media_type(media_type).as_str() {
            "image/jpeg" | "image/png" => Ok(Self::Image),
            "application/pdf" => Ok(Self::Pdf),
            "text/csv" | "application/csv" => Ok(Self::Csv),
            "application/msword"
            | "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => {
                Ok(Self::Word)
            }
            "application/vnd.ms-excel"
            | "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => {
                Ok(Self::Excel)
            }
            _ => Err(AttachmentError::UnsupportedMediaType {
                media_type: media_type.to_owned(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AttachmentDescriptor {
    id: AttachmentId,
    kind: AttachmentKind,
    media_type: String,
}

impl AttachmentDescriptor {
    pub fn new(id: AttachmentId, media_type: &str) -> Result<Self, AttachmentError> {
        let kind = AttachmentKind::from_media_type(media_type)?;
        Ok(Self {
            id,
            kind,
            media_type: normalized_media_type(media_type),
        })
    }

    pub fn id(&self) -> &AttachmentId {
        &self.id
    }

    pub fn kind(&self) -> AttachmentKind {
        self.kind
    }

    pub fn media_type(&self) -> &str {
        &self.media_type
    }
}

impl<'de> serde::Deserialize<'de> for AttachmentDescriptor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct RawAttachmentDescriptor {
            id: AttachmentId,
            kind: AttachmentKind,
            media_type: String,
        }

        let raw = RawAttachmentDescriptor::deserialize(deserializer)?;
        let descriptor =
            AttachmentDescriptor::new(raw.id, &raw.media_type).map_err(serde::de::Error::custom)?;
        if raw.kind != descriptor.kind() {
            return Err(serde::de::Error::custom(format!(
                "attachment kind {raw_kind:?} does not match media_type {raw_media_type}",
                raw_kind = raw.kind,
                raw_media_type = raw.media_type,
            )));
        }
        Ok(descriptor)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachmentError {
    UnsupportedMediaType { media_type: String },
}

impl Display for AttachmentError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedMediaType { media_type } => {
                write!(formatter, "unsupported attachment media type: {media_type}")
            }
        }
    }
}

impl std::error::Error for AttachmentError {}

fn normalized_media_type(media_type: &str) -> String {
    media_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::{AttachmentDescriptor, AttachmentError, AttachmentKind};
    use crate::identity::AttachmentId;

    #[test]
    fn attachment_kinds_cover_supported_inputs() {
        let cases = [
            ("image/jpeg", AttachmentKind::Image),
            ("image/png", AttachmentKind::Image),
            ("application/pdf", AttachmentKind::Pdf),
            ("text/csv; charset=utf-8", AttachmentKind::Csv),
            ("application/msword", AttachmentKind::Word),
            (
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                AttachmentKind::Word,
            ),
            ("application/vnd.ms-excel", AttachmentKind::Excel),
            (
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                AttachmentKind::Excel,
            ),
        ];

        for (media_type, expected) in cases {
            assert_eq!(AttachmentKind::from_media_type(media_type), Ok(expected));
        }
    }

    #[test]
    fn attachment_descriptor_preserves_typed_identity_and_normalizes_media_type() {
        let result = AttachmentId::new("attachment-1")
            .map(|id| AttachmentDescriptor::new(id, " Image/JPEG "));
        assert!(
            matches!(result, Ok(Ok(ref d)) if d.kind() == AttachmentKind::Image
                && d.media_type() == "image/jpeg"
                && d.id().as_str() == "attachment-1")
        );
    }

    #[test]
    fn attachment_kind_rejects_unsupported_media_types() {
        assert!(matches!(
            AttachmentKind::from_media_type("application/zip"),
            Err(AttachmentError::UnsupportedMediaType { .. })
        ));
    }

    #[test]
    fn attachment_descriptor_deserialization_rejects_mismatched_kind() {
        let json = r#"{"id":"att-1","kind":"image","media_type":"application/pdf"}"#;
        let result: Result<AttachmentDescriptor, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn attachment_descriptor_deserialization_rejects_unsupported_media_type() {
        let json = r#"{"id":"att-1","kind":"pdf","media_type":"application/zip"}"#;
        let result: Result<AttachmentDescriptor, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn attachment_descriptor_deserialization_accepts_valid() {
        let json = r#"{"id":"att-1","kind":"image","media_type":"image/jpeg"}"#;
        let deser: Result<AttachmentDescriptor, String> =
            serde_json::from_str(json).map_err(|e| e.to_string());
        let ctor: Result<AttachmentDescriptor, String> = AttachmentId::new("att-1")
            .map_err(|e| e.to_string())
            .and_then(|id| AttachmentDescriptor::new(id, "image/jpeg").map_err(|e| e.to_string()));
        assert!(matches!((&deser, &ctor), (Ok(d), Ok(c)) if d == c));
    }

    #[test]
    fn attachment_descriptor_deserialization_normalizes_media_type_like_new() {
        let messy_mime = " Image/JPEG ; charset=utf-8 ";
        let json = format!(r#"{{"id":"att-norm","kind":"image","media_type":"{messy_mime}"}}"#);
        let deser: Result<AttachmentDescriptor, String> =
            serde_json::from_str(&json).map_err(|e| e.to_string());
        let ctor: Result<AttachmentDescriptor, String> = AttachmentId::new("att-norm")
            .map_err(|e| e.to_string())
            .and_then(|id| AttachmentDescriptor::new(id, messy_mime).map_err(|e| e.to_string()));
        assert!(
            matches!((&deser, &ctor), (Ok(d), Ok(c)) if d == c),
            "deserialization must normalize media_type identically to new()"
        );
        // Confirm normalized value is "image/jpeg" (trimmed, lowered, de-parameterized)
        assert!(
            matches!(&deser, Ok(d) if d.kind() == AttachmentKind::Image && d.media_type() == "image/jpeg")
        );
    }
}
