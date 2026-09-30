//! Stable content hashing for versioned definitions.

use serde::Serialize;

/// FNV-1a over the canonical JSON encoding. Stable across processes and
/// Rust releases, unlike `DefaultHasher`, so it can be persisted and compared
/// on restore.
pub(crate) fn content_hash(value: &impl Serialize) -> String {
    let bytes = serde_json::to_vec(value).expect("definitions always serialize");
    let hash = bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    format!("fnv1a64:{hash:016x}")
}
