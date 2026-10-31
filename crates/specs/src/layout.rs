//! Where the specs live. Every path is relative to the repository root and uses `/`.
use crate::store::SpecStore;

pub const DEFAULT_ROOT: &str = "specs";
/// Optional `{"root": "docs/specs"}` for repositories where `specs/` is taken.
pub const CONFIG_PATH: &str = ".rusty/specs.json";
/// The documents of a change, in reading order.
pub const CHANGE_DOCUMENTS: [&str; 7] = [
    "change.md",
    "spec.md",
    "checklist.md",
    "research.md",
    "design.md",
    "tasks.md",
    "verification.md",
];
const MAX_SLUG_CHARS: usize = 48;

/// The configured root, or [`DEFAULT_ROOT`] when there is none or it is not a plain relative path.
pub fn resolve_root(store: &dyn SpecStore) -> String {
    store
        .read(CONFIG_PATH)
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|value| value["root"].as_str().and_then(clean_root))
        .unwrap_or_else(|| DEFAULT_ROOT.to_owned())
}

/// A relative folder path without `..`, empty parts or a trailing slash; `None` when unusable.
pub fn clean_root(raw: &str) -> Option<String> {
    let normalized = raw.trim().replace('\\', "/");
    let parts: Vec<&str> = normalized.split('/').filter(|p| !p.is_empty()).collect();
    let usable = !parts.is_empty()
        && !normalized.starts_with('/')
        && !parts
            .iter()
            .any(|p| *p == ".." || *p == "." || p.contains(':'));
    usable.then(|| parts.join("/"))
}

/// Paths under one specs root.
#[derive(Clone, Debug)]
pub struct Layout {
    pub root: String,
}

impl Layout {
    pub fn new(root: impl Into<String>) -> Self {
        Self { root: root.into() }
    }
    pub fn of(store: &dyn SpecStore) -> Self {
        Self::new(resolve_root(store))
    }
    pub fn readme(&self) -> String {
        format!("{}/README.md", self.root)
    }
    pub fn project(&self) -> String {
        format!("{}/project.md", self.root)
    }
    pub fn capabilities(&self) -> String {
        format!("{}/capabilities", self.root)
    }
    pub fn capability_spec(&self, name: &str) -> String {
        format!("{}/capabilities/{name}/spec.md", self.root)
    }
    pub fn changes(&self) -> String {
        format!("{}/changes", self.root)
    }
    pub fn change(&self, dir: &str) -> String {
        format!("{}/changes/{dir}", self.root)
    }
    pub fn archive(&self) -> String {
        format!("{}/archive", self.root)
    }
    /// Whether `path` is inside the root and free of `..` (what the document command may read).
    pub fn contains(&self, path: &str) -> bool {
        let normalized = path.replace('\\', "/");
        normalized.starts_with(&format!("{}/", self.root))
            && !normalized
                .split('/')
                .any(|part| part == ".." || part.is_empty())
    }
}

/// `Photo albums!` becomes `photo-albums`; `change` when nothing usable is left.
pub fn slugify(text: &str) -> String {
    let mut slug = String::new();
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    let slug: String = slug.chars().take(MAX_SLUG_CHARS).collect();
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "change".to_owned()
    } else {
        slug.to_owned()
    }
}

pub fn change_dir_name(number: u32, slug: &str) -> String {
    format!("{number:04}-{slug}")
}

/// `0007-photo-albums` or an archived `2026-10-30-0007-photo-albums` gives `(7, "photo-albums")`.
pub fn parse_dir_name(name: &str) -> Option<(u32, String)> {
    let name = strip_date_prefix(name);
    let (number, slug) = name.split_once('-')?;
    if number.len() < 4 || !number.bytes().all(|b| b.is_ascii_digit()) || slug.is_empty() {
        return None;
    }
    Some((number.parse().ok()?, slug.to_owned()))
}

fn strip_date_prefix(name: &str) -> &str {
    let bytes = name.as_bytes();
    let dated = bytes.len() > 11
        && bytes[..10].iter().enumerate().all(|(i, b)| match i {
            4 | 7 => *b == b'-',
            _ => b.is_ascii_digit(),
        })
        && bytes[10] == b'-';
    if dated {
        &name[11..]
    } else {
        name
    }
}

/// The next free number given every change and archive folder name.
pub fn next_number<'a>(names: impl IntoIterator<Item = &'a str>) -> u32 {
    names
        .into_iter()
        .filter_map(|name| parse_dir_name(name).map(|(n, _)| n))
        .max()
        .map_or(1, |n| n + 1)
}

pub fn branch_name(dir: &str) -> String {
    format!("spec/{dir}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryStore;

    #[test]
    fn slugs_are_plain() {
        assert_eq!(slugify("Photo albums!"), "photo-albums");
        assert_eq!(slugify("  --  "), "change");
        assert_eq!(slugify("Ünï cødé"), "n-c-d");
        assert!(slugify(&"a ".repeat(100)).len() <= MAX_SLUG_CHARS);
    }

    #[test]
    fn folder_names_round_trip_with_and_without_a_date() {
        assert_eq!(change_dir_name(7, "photo-albums"), "0007-photo-albums");
        assert_eq!(
            parse_dir_name("0007-photo-albums"),
            Some((7, "photo-albums".into()))
        );
        assert_eq!(
            parse_dir_name("2026-10-30-0007-photo-albums"),
            Some((7, "photo-albums".into()))
        );
        assert_eq!(parse_dir_name("notes"), None);
        assert_eq!(parse_dir_name("7-short"), None);
    }

    #[test]
    fn numbering_continues_after_the_highest_including_the_archive() {
        assert_eq!(next_number([]), 1);
        assert_eq!(next_number(["0001-a", "2026-10-30-0009-b", "misc"]), 10);
    }

    #[test]
    fn the_root_comes_from_config_only_when_it_is_a_plain_relative_path() {
        let store = MemoryStore::default();
        assert_eq!(resolve_root(&store), "specs");
        store.insert(CONFIG_PATH, r#"{"root": "docs\\specs/"}"#);
        assert_eq!(resolve_root(&store), "docs/specs");
        store.insert(CONFIG_PATH, r#"{"root": "../elsewhere"}"#);
        assert_eq!(resolve_root(&store), "specs");
        store.insert(CONFIG_PATH, r#"{"root": "/abs"}"#);
        assert_eq!(resolve_root(&store), "specs");
    }

    #[test]
    fn only_paths_inside_the_root_may_be_read() {
        let layout = Layout::new("specs");
        assert!(layout.contains("specs/changes/0001-a/spec.md"));
        assert!(!layout.contains("specs/../secret"));
        assert!(!layout.contains("src/main.rs"));
        assert!(!layout.contains("specsx/a.md"));
    }
}
