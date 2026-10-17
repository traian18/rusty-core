//! Matching of excluded (generated or vendored) paths.
use harness_workspace::repository_intelligence::RootRelativePath;

/// Compiled exclusion patterns.
///
/// * `name/` excludes any path with a directory component called `name`.
/// * Any other pattern is matched against the file name, where `*` matches any text.
pub struct ExclusionSet {
    directories: Vec<String>,
    file_patterns: Vec<String>,
}

impl ExclusionSet {
    pub fn new(patterns: &[String]) -> Self {
        let mut directories = Vec::new();
        let mut file_patterns = Vec::new();
        for pattern in patterns {
            match pattern.strip_suffix('/') {
                Some(directory) => directories.push(directory.to_owned()),
                None => file_patterns.push(pattern.clone()),
            }
        }
        Self {
            directories,
            file_patterns,
        }
    }

    pub fn excludes(&self, path: &RootRelativePath) -> bool {
        let path = path.as_path();
        let in_excluded_directory = path
            .parent()
            .into_iter()
            .flat_map(|parent| parent.components())
            .any(|component| {
                let name = component.as_os_str().to_string_lossy();
                self.directories.iter().any(|d| *d == name)
            });
        if in_excluded_directory {
            return true;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.file_patterns.iter().any(|p| wildcard_match(p, &name))
    }
}

/// `*` matches any run of characters, including none.
fn wildcard_match(pattern: &str, text: &str) -> bool {
    let parts = pattern.split('*').collect::<Vec<_>>();
    let [first, rest @ ..] = parts.as_slice() else {
        return pattern == text;
    };
    if rest.is_empty() {
        return pattern == text;
    }
    let Some(mut remainder) = text.strip_prefix(first) else {
        return false;
    };
    let (last, middle) = rest.split_last().expect("rest is not empty");
    for part in middle {
        match remainder.find(part) {
            Some(position) => remainder = &remainder[position + part.len()..],
            None => return false,
        }
    }
    remainder.ends_with(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set() -> ExclusionSet {
        ExclusionSet::new(&crate::GitAnalysisConfig::default().exclusions)
    }

    fn excluded(path: &str) -> bool {
        set().excludes(&RootRelativePath::new(path).unwrap())
    }

    #[test]
    fn excludes_generated_and_vendored_paths() {
        assert!(excluded("Cargo.lock"));
        assert!(excluded("web/node_modules/x/index.js"));
        assert!(excluded("app/dist/main.js"));
        assert!(excluded("assets/app.min.js"));
        assert!(excluded("tests/__snapshots__/a.snap"));
        assert!(excluded("yarn.lock"));
    }

    #[test]
    fn keeps_ordinary_sources() {
        assert!(!excluded("src/main.rs"));
        assert!(!excluded("src/distance.rs"));
        assert!(!excluded("src/build.rs"));
        assert!(!excluded("docs/vendor.md"));
    }

    #[test]
    fn wildcard_handles_prefix_suffix_and_middle() {
        assert!(wildcard_match("*.min.*", "a.min.js"));
        assert!(!wildcard_match("*.min.*", "a.js"));
        assert!(wildcard_match("a*c", "abbbc"));
        assert!(!wildcard_match("a*c", "abbbd"));
        assert!(wildcard_match("exact", "exact"));
    }
}
