//! Rendering the short block of repository facts.
use harness_git_analysis::GitSnapshot;
use harness_workspace::{
    dependency_groups::GroupKind,
    repository_intelligence::{RepositorySnapshot, RootRelativePath},
};

/// One line summarising the whole repository.
pub(crate) fn overview_line(snapshot: &RepositorySnapshot) -> String {
    let mut languages = snapshot
        .imports
        .files
        .iter()
        .map(|file| format!("{:?}", file.profile).to_lowercase())
        .collect::<Vec<_>>();
    languages.sort();
    languages.dedup();
    let mut parts = vec![
        count(snapshot.graph.packages.len(), "package"),
        format!(
            "{} indexed files ({})",
            snapshot.imports.files.len(),
            languages.join(", ")
        ),
        count(snapshot.imports.edges.len(), "import"),
    ];
    if snapshot.groups.configured {
        let cycles = snapshot
            .groups
            .groups
            .iter()
            .filter(|group| group.kind == GroupKind::ImportCycle)
            .count();
        parts.push(count(cycles, "import cycle"));
    }
    if snapshot.imports.partial {
        parts.push("partial coverage".to_owned());
    }
    format!(
        "Repository snapshot {}: {}.",
        short(&snapshot.id.0),
        parts.join(", ")
    )
}

/// Facts about one indexed file: groups, import counts, and (when available) Git history.
pub(crate) fn file_line(
    snapshot: &RepositorySnapshot,
    git: Option<&GitSnapshot>,
    file: &RootRelativePath,
) -> String {
    let path = file.as_path().display();
    let imports = snapshot
        .imports
        .edges
        .iter()
        .filter(|edge| edge.fact.importing_file == *file)
        .count();
    let dependents = snapshot
        .imports
        .edges
        .iter()
        .filter(|edge| edge.resolution.target_file() == Some(file))
        .count();
    let mut line = format!("- {path}: {imports} imports, {dependents} dependents");
    let groups = snapshot
        .groups
        .groups_containing(file)
        .map(|group| format!("{:?} {}", group.kind, group.label))
        .collect::<Vec<_>>();
    if !groups.is_empty() {
        line.push_str(&format!("; groups: {}", groups.join("; ")));
    }
    if let Some(history) = git.and_then(|git| git_facts(git, file)) {
        line.push_str(&format!("; {history}"));
    }
    line
}

fn git_facts(git: &GitSnapshot, file: &RootRelativePath) -> Option<String> {
    let churn = git
        .index
        .churn
        .binary_search_by(|entry| entry.file.cmp(file))
        .ok()
        .map(|position| &git.index.churn[position])?;
    let mut text = format!("history: {} commits in the analysed window", churn.commits);
    let start = git.index.co_changes.partition_point(|c| c.file < *file);
    if let Some(partner) = git.index.co_changes[start..]
        .iter()
        .take_while(|c| c.file == *file)
        .next()
    {
        text.push_str(&format!(
            ", often changed with {} ({}/{} commits; correlation, not dependency)",
            partner.partner.as_path().display(),
            partner.support,
            partner.file_commits
        ));
    }
    Some(text)
}

fn short(id: &str) -> &str {
    &id[..id.len().min(12)]
}

/// "1 package", "2 packages".
fn count(number: usize, noun: &str) -> String {
    if number == 1 {
        format!("{number} {noun}")
    } else {
        format!("{number} {noun}s")
    }
}
