//! Which commits fix a bug, from the subject line alone.
//!
//! A subject counts as a fix when one of its words is a form of *fix* (`fix`, `fixes`, `fixed`,
//! `fixing`, and the same after `hot` or `bug`), `bug`, `patch`, `resolve(s)`, or it closes an
//! issue (`close #12`, `closes #12`). It does not count when it is a merge, or when it mentions
//! a typo, a version bump, dependencies, chores, linting, formatting, style or documentation,
//! because those "fixes" rarely touch behaviour. Words are compared without case; a conventional
//! prefix such as `fix(parser):` reads as the word `fix`. Versioned, because every count of fixes
//! in the product must count the same commits.
pub const FIX_RULE_VERSION: u32 = 1;

const FIX_STEMS: [&str; 3] = ["fix", "hotfix", "bugfix"];
const FIX_SUFFIXES: [&str; 4] = ["", "es", "ed", "ing"];
const FIX_WORDS: [&str; 4] = ["bug", "patch", "resolve", "resolves"];
const CLOSE_WORDS: [&str; 2] = ["close", "closes"];
const EXCLUDED_WORDS: [&str; 12] = [
    "typo",
    "typos",
    "bump",
    "dep",
    "deps",
    "chore",
    "lint",
    "format",
    "formatting",
    "style",
    "doc",
    "docs",
];

fn words(subject: &str) -> Vec<String> {
    subject
        .split(|c: char| !(c.is_alphanumeric() || c == '#'))
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn is_fix_word(word: &str) -> bool {
    FIX_WORDS.contains(&word)
        || FIX_STEMS.iter().any(|stem| {
            word.strip_prefix(stem)
                .is_some_and(|rest| FIX_SUFFIXES.contains(&rest))
        })
}

pub fn is_fix_commit(subject: &str) -> bool {
    let subject = subject.trim();
    if subject.is_empty() || subject.to_lowercase().starts_with("merge ") {
        return false;
    }
    let words = words(subject);
    if words.iter().any(|w| EXCLUDED_WORDS.contains(&w.as_str())) {
        return false;
    }
    let closes_issue = words.windows(2).any(|pair| {
        CLOSE_WORDS.contains(&pair[0].as_str())
            && pair[1]
                .strip_prefix('#')
                .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
    });
    closes_issue || words.iter().any(|w| is_fix_word(w))
}

#[cfg(test)]
mod tests {
    use super::is_fix_commit;

    #[test]
    fn fixes_bugs_patches_and_closed_issues_count() {
        for subject in [
            "Fix crash when the list is empty",
            "fix(parser): handle trailing commas",
            "Fixes #12",
            "fixed the off-by-one",
            "Hotfix for login",
            "bugfixes in the scheduler",
            "Patch the race in refresh",
            "Resolves the timeout",
            "Closes #431",
            "Null pointer bug",
        ] {
            assert!(is_fix_commit(subject), "{subject}");
        }
    }

    #[test]
    fn maintenance_merges_and_lookalikes_do_not() {
        for subject in [
            "Merge branch 'fix/login'",
            "fix typo in README",
            "chore: fix lint",
            "Bump serde to 1.0.200",
            "docs: fix links",
            "Fix formatting",
            "Add prefix handling",
            "fixture updates",
            "debug output",
            "close the file handle",
            "",
        ] {
            assert!(!is_fix_commit(subject), "{subject}");
        }
    }
}
