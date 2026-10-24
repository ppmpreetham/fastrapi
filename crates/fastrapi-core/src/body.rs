//! Types for a parsed request body.
//! A body is JSON or form data, and form data can contain text fields and uploaded files.

use ahash::AHashMap;
use bytes::Bytes;
use smallvec::SmallVec;

#[derive(Clone, Debug)]
pub struct UploadedFile {
    pub filename: Option<String>,
    pub content_type: Option<String>,
    pub content: Bytes,
}

#[derive(Clone, Debug)]
pub enum BodyField {
    Text(String),
    File(UploadedFile),
}

#[derive(Clone, Debug)]
pub enum BodyPayload {
    Json {
        raw: Bytes,
        value: Option<simd_json::OwnedValue>,
    },
    Form {
        fields: AHashMap<String, SmallVec<[BodyField; 2]>>,
        raw: Bytes,
    },
}

impl BodyPayload {
    #[must_use]
    pub const fn raw(&self) -> &Bytes {
        match self {
            Self::Json { raw, .. } | Self::Form { raw, .. } => raw,
        }
    }
}
