//! A stable digest of a page's content, so equal digests mean equal pages.
use sha2::{Digest, Sha256};

use crate::model::Section;

pub fn sections_digest(sections: &[Section]) -> String {
    let mut hasher = Sha256::new();
    for section in sections {
        hasher.update(section.key.as_bytes());
        for fact in &section.facts {
            hasher.update(fact.id.as_bytes());
            hasher.update(fact.text.as_bytes());
            for citation in &fact.citations {
                hasher.update(citation.file.as_bytes());
                hasher.update(citation.start_line.to_le_bytes());
                hasher.update(citation.end_line.to_le_bytes());
                hasher.update(citation.content_hash.as_bytes());
            }
            for page in &fact.refs {
                hasher.update(page.0.as_bytes());
            }
        }
        hasher.update(section.omitted.to_le_bytes());
    }
    hasher
        .finalize()
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
