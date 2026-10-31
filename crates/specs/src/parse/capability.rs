//! A capability's `spec.md`: a title, `Purpose:` and `### Requirement: Name` blocks.
use crate::model::CapabilityDoc;

pub fn parse(text: &str) -> CapabilityDoc {
    let mut doc = CapabilityDoc::default();
    for line in text.lines() {
        if let Some(title) = line.strip_prefix("# ") {
            doc.title.get_or_insert_with(|| title.trim().to_owned());
        } else if let Some(purpose) = line.trim().strip_prefix("Purpose:") {
            doc.purpose.get_or_insert_with(|| purpose.trim().to_owned());
        } else if let Some(name) = line.strip_prefix("### Requirement:") {
            doc.requirements.push(name.trim().to_owned());
        }
    }
    doc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_title_purpose_and_named_requirements() {
        let doc = parse("# Albums\n\nPurpose: grouping photos.\n\n## Requirements\n\n### Requirement: Album creation\nText\n\n### Requirement: Album deletion\n");
        assert_eq!(doc.title.as_deref(), Some("Albums"));
        assert_eq!(doc.purpose.as_deref(), Some("grouping photos."));
        assert_eq!(doc.requirements, ["Album creation", "Album deletion"]);
    }
}
