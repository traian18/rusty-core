//! Parsing and validating tool arguments.
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use harness_workspace::repository_intelligence::RootRelativePath;
use serde::{de::DeserializeOwned, Serialize};

use crate::failure::Failure;

/// Deserializes arguments, rejecting unknown fields via each type's `deny_unknown_fields`.
pub(crate) fn parse<T: DeserializeOwned>(arguments: serde_json::Value) -> Result<T, Failure> {
    serde_json::from_value(arguments).map_err(|error| Failure::invalid(error.to_string()))
}

/// A workspace-relative path; absolute paths and `..` are rejected before any query runs.
pub(crate) fn file(path: &str) -> Result<RootRelativePath, Failure> {
    RootRelativePath::new(path)
        .map_err(|_| Failure::invalid(format!("not a workspace-relative path: {path}")))
}

/// Cursors travel as opaque strings; the contracts stay in charge of validating them.
pub(crate) fn encode_cursor<T: Serialize>(cursor: &T) -> String {
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(cursor).unwrap_or_default())
}

pub(crate) fn decode_cursor<T: DeserializeOwned>(cursor: &str) -> Result<T, Failure> {
    let bytes = URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| Failure::new("invalid_cursor", "cursor is not valid"))?;
    serde_json::from_slice(&bytes)
        .map_err(|_| Failure::new("invalid_cursor", "cursor is not valid"))
}

pub(crate) fn optional_cursor<T: DeserializeOwned>(
    cursor: &Option<String>,
) -> Result<Option<T>, Failure> {
    cursor.as_deref().map(decode_cursor).transpose()
}

pub(crate) fn next_cursor<T: Serialize>(cursor: &Option<T>) -> Option<String> {
    cursor.as_ref().map(encode_cursor)
}
