//! The evidence each page is written from: facts as plain text, real code excerpts, the README's
//! own vocabulary, and summaries of pages already written.
use harness_workspace::{
    dependency_groups::Group,
    folder_tree,
    repository_intelligence::{RepositorySnapshot, RootRelativePath},
};

use super::{
    brief::{BriefBuilder, BriefContext},
    concepts,
    diagram::{component_weights, limit_nodes, DiagramNode},
    evidence::Evidence,
    excerpts, flow, glossary, landscape, readme, scripts,
};
use crate::{
    context::Centrality,
    error::WikiError,
    facts::Facts,
    model::{Page, PageId},
};

/// Top-level parts described on the architecture page.
pub const MAX_COMPONENTS: usize = 12;
const COMPONENT_EDGES: usize = 20;
const CENTRAL_EXCERPTS: usize = 4;
const MODULE_KEY_FILES: usize = 6;
const EXCERPT_LINES: u32 = 50;
const SUMMARY_CHARS: usize = 600;
const CONCEPTS: usize = 7;

/// What a page is built from, before it becomes a prompt.
pub struct Body {
    pub facts: String,
    pub diagram: bool,
    pub prewritten: Option<String>,
}

fn section_text(page: &Page, keys: &[&str]) -> String {
    let mut out = String::new();
    for key in keys {
        let Some(section) = page.sections.iter().find(|s| s.key == *key) else {
            continue;
        };
        if section.facts.is_empty() {
            continue;
        }
        out.push_str(&format!("{}:\n", section.title));
        for fact in &section.facts {
            out.push_str(&format!("- {}\n", fact.text));
        }
    }
    out
}

/// The opening of pages already written, so later pages stay consistent with them.
fn earlier(context: &BriefContext, ids: &[String]) -> String {
    let mut out = String::new();
    for id in ids {
        let Some(text) = context.written.get(id) else {
            continue;
        };
        let opening: String = text
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.starts_with('#') && !l.starts_with("@@"))
            .take(3)
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(SUMMARY_CHARS)
            .collect();
        if !opening.is_empty() {
            out.push_str(&format!("- {id}: {opening}\n"));
        }
    }
    if out.is_empty() {
        out
    } else {
        format!("\n## What the pages already written say (for consistency; do not copy)\n{out}")
    }
}

fn readme_digest(evidence: &mut Evidence<'_>, directory: &str) -> String {
    for name in readme::README_NAMES {
        let path = if directory.is_empty() {
            name.to_owned()
        } else {
            format!("{directory}/{name}")
        };
        if let Some(text) = evidence.read(&path) {
            let digest = readme::digest(&text);
            if !digest.trim().is_empty() {
                evidence.mention(&path);
                return format!("## What the project says about itself (from `{path}`: vocabulary and purpose only)\n{digest}\n");
            }
        }
    }
    String::new()
}

fn group_path(group: &Group) -> String {
    folder_tree::directory_path(group).map_or_else(|| group.id.0.clone(), str::to_owned)
}

fn components_text(
    builder: &BriefBuilder,
    snapshot: &RepositorySnapshot,
    components: &[Group],
    evidence: &mut Evidence<'_>,
) -> Result<String, WikiError> {
    let mut out = String::from("## The parts (top-level components)\n");
    for group in components {
        let page = builder.pages().page(&BriefBuilder::page_id(group), None)?;
        let path = group_path(group);
        evidence.mention(&path);
        out.push_str(&format!(
            "- **{}** (`{path}`, {} files): {}\n",
            group.label,
            group.members.len(),
            page.summary
        ));
    }
    let nodes = limit_nodes(
        components
            .iter()
            .map(|g| DiagramNode {
                label: g.label.clone(),
                members: g.members.clone(),
            })
            .collect(),
    );
    let mut edges: Vec<_> = component_weights(snapshot, &nodes).into_iter().collect();
    edges.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    if !edges.is_empty() {
        out.push_str("\nWho imports whom (importer -> imported, number of imports):\n");
        for ((from, to), weight) in edges.into_iter().take(COMPONENT_EDGES) {
            out.push_str(&format!(
                "- {} -> {} ({weight})\n",
                nodes[from].label, nodes[to].label
            ));
        }
    }
    Ok(out)
}

fn central_excerpts(
    snapshot: &RepositorySnapshot,
    evidence: &mut Evidence<'_>,
    count: usize,
) -> String {
    let centrality = Centrality::new(snapshot);
    let files = concepts::central_files(snapshot, &centrality, count);
    let mut out = String::from("\n## The most depended-on files\n");
    for path in &files {
        evidence.mention(path);
        out.push_str(&format!("- `{path}`\n"));
        excerpts::add(evidence, path, 1, EXCERPT_LINES);
    }
    out
}

pub fn overview(
    builder: &BriefBuilder,
    snapshot: &RepositorySnapshot,
    context: &BriefContext,
    evidence: &mut Evidence<'_>,
) -> Result<Body, WikiError> {
    let repo = builder.pages().page(&PageId::repository(), None)?;
    let mut facts = readme_digest(evidence, "");
    facts.push_str(&format!(
        "## The repository\n{}: {}\n{}\n",
        context.repository_name,
        repo.summary,
        section_text(&repo, &["contents", "languages", "entry_points"])
    ));
    let components = BriefBuilder::components(snapshot);
    facts.push_str(&components_text(
        builder,
        snapshot,
        &components[..components.len().min(MAX_COMPONENTS)],
        evidence,
    )?);
    facts.push_str(&central_excerpts(snapshot, evidence, CENTRAL_EXCERPTS));
    let ids: Vec<String> = [
        super::brief::ARCHITECTURE,
        super::brief::KEY_CONCEPTS,
        super::brief::HOW_IT_WORKS,
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .chain(
        components
            .iter()
            .take(MAX_COMPONENTS)
            .map(|g| BriefBuilder::page_id(g).0),
    )
    .collect();
    facts.push_str(&earlier(context, &ids));
    Ok(Body {
        facts,
        diagram: true,
        prewritten: None,
    })
}

pub fn getting_started(
    snapshot: &RepositorySnapshot,
    evidence: &mut Evidence<'_>,
) -> Result<Body, WikiError> {
    let mut facts = String::new();
    let mut setup = String::new();
    for name in readme::README_NAMES {
        let Some(text) = evidence.read(name) else {
            continue;
        };
        for (heading, body) in readme::setup_sections(&text) {
            setup.push_str(&format!("### From `{name}`: {heading}\n{body}\n\n"));
        }
        evidence.mention(name);
        break;
    }
    for doc in [
        "CONTRIBUTING.md",
        "docs/getting-started.md",
        "docs/development.md",
        "DEVELOPMENT.md",
    ] {
        if let Some(text) = evidence.read(doc) {
            for (heading, body) in readme::setup_sections(&text) {
                setup.push_str(&format!("### From `{doc}`: {heading}\n{body}\n\n"));
            }
            evidence.mention(doc);
        }
    }
    facts.push_str(if setup.is_empty() { "## Setup sections\nThe repository documents no install, build, run or test steps in its README or contributing guide.\n\n" } else { "## Setup sections (verbatim; the only source of commands)\n" });
    facts.push_str(&setup);
    let mut manifests: Vec<String> = snapshot
        .graph
        .packages
        .iter()
        .map(|p| p.manifest.as_path().display().to_string())
        .collect();
    for extra in ["package.json", "Makefile", "makefile"] {
        manifests.push(extra.to_owned());
    }
    manifests.sort();
    manifests.dedup();
    let mut declared = String::new();
    let mut present = Vec::new();
    for manifest in manifests {
        let Some(text) = evidence.read(&manifest) else {
            continue;
        };
        present.push(manifest.clone());
        evidence.mention(&manifest);
        let found = if manifest.ends_with("package.json") {
            scripts::npm(&manifest, &text)
        } else if manifest.ends_with("akefile") {
            scripts::make(&manifest, &text)
        } else {
            Vec::new()
        };
        for script in found {
            declared.push_str(&format!(
                "- `{}` in `{}` (from `{}`): runs `{}`\n",
                script.invocation, script.directory, script.manifest, script.command
            ));
        }
    }
    if !declared.is_empty() {
        facts.push_str("## Declared scripts (you may use these invocations as written)\n");
        facts.push_str(&declared);
    }
    if !present.is_empty() {
        facts.push_str(&format!(
            "\n## Manifests in the repository\n{}\n",
            present
                .iter()
                .map(|m| format!("`{m}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let facts_for_entries = Facts::new(snapshot);
    if let Some((entry, _)) = flow::entry(snapshot, &facts_for_entries) {
        let path = entry.as_path().display().to_string();
        evidence.mention(&path);
        facts.push_str(&format!("\n## Where it starts\n`{path}`\n"));
    }
    Ok(Body {
        facts,
        diagram: false,
        prewritten: None,
    })
}

pub fn key_concepts(
    snapshot: &RepositorySnapshot,
    context: &BriefContext,
    evidence: &mut Evidence<'_>,
) -> Result<Body, WikiError> {
    let centrality = Centrality::new(snapshot);
    let mut facts = readme_digest(evidence, "");
    let (chosen, links) = concepts::choose(snapshot, &centrality, evidence, CONCEPTS);
    facts.push_str("## Candidate concepts (name, where it lives); their code is below\n");
    for c in &chosen {
        facts.push_str(&format!("- `{}` in `{}`\n", c.name, c.path));
    }
    if links.is_empty() {
        facts.push_str("\nNo import links between the candidates' files.\n");
    } else {
        facts.push_str("\n## Links between them (the first one's file imports the second one's)\n");
        for l in &links {
            facts.push_str(&format!("- `{}` uses `{}`\n", l.from, l.to));
        }
    }
    facts.push_str(&earlier(context, &[super::brief::ARCHITECTURE.to_owned()]));
    Ok(Body {
        facts,
        diagram: false,
        prewritten: None,
    })
}

pub fn how_it_works(
    snapshot: &RepositorySnapshot,
    context: &BriefContext,
    evidence: &mut Evidence<'_>,
) -> Result<Body, WikiError> {
    let centrality = Centrality::new(snapshot);
    let facts_index = Facts::new(snapshot);
    let mut facts = readme_digest(evidence, "");
    match flow::entry(snapshot, &facts_index) {
        Some((entry, archetype)) => {
            let path = flow::trace(&facts_index, &centrality, &entry);
            facts.push_str(&format!("## What kind of program this looks like (a hint)\n{}\n\n## The traced path (each step imports the next)\n", archetype.name()));
            for (i, hop) in path.iter().enumerate() {
                let text = hop.as_path().display().to_string();
                facts.push_str(&format!("{}. `{text}`\n", i + 1));
                excerpts::add(evidence, &text, 1, EXCERPT_LINES);
            }
        }
        None => {
            facts.push_str("## The traced path\nNo entry point was found; describe the system from its most depended-on files instead.\n");
            facts.push_str(&central_excerpts(snapshot, evidence, CENTRAL_EXCERPTS));
        }
    }
    facts.push_str(&earlier(
        context,
        &[
            super::brief::ARCHITECTURE.to_owned(),
            super::brief::KEY_CONCEPTS.to_owned(),
        ],
    ));
    Ok(Body {
        facts,
        diagram: false,
        prewritten: None,
    })
}

pub fn architecture(
    builder: &BriefBuilder,
    snapshot: &RepositorySnapshot,
    context: &BriefContext,
    evidence: &mut Evidence<'_>,
) -> Result<Body, WikiError> {
    let repo = builder.pages().page(&PageId::repository(), None)?;
    let mut facts = readme_digest(evidence, "");
    let components = BriefBuilder::components(snapshot);
    let top = &components[..components.len().min(MAX_COMPONENTS)];
    facts.push_str(&components_text(builder, snapshot, top, evidence)?);
    let centrality = Centrality::new(snapshot);
    facts.push_str("\n## Inside each part (its most depended-on file and its central code)\n");
    for group in top.iter().take(6) {
        let mut members: Vec<&RootRelativePath> = group.members.iter().collect();
        members.sort_by(|a, b| {
            centrality
                .score(b)
                .total_cmp(&centrality.score(a))
                .then(a.cmp(b))
        });
        if let Some(file) = members.first() {
            let path = file.as_path().display().to_string();
            facts.push_str(&format!("- {}: `{path}`\n", group.label));
            excerpts::add(evidence, &path, 1, 35);
        }
        let page = builder.pages().page(&BriefBuilder::page_id(group), None)?;
        let boundary = section_text(&page, &["boundary", "entry_points"]);
        if !boundary.is_empty() {
            facts.push_str(&format!("  {}\n", boundary.replace('\n', "\n  ")));
        }
    }
    facts.push_str(&format!(
        "\n## Repository-wide\n{}\n",
        section_text(&repo, &["entry_points", "findings", "most_depended_on"])
    ));
    let ids: Vec<String> = top.iter().map(|g| BriefBuilder::page_id(g).0).collect();
    facts.push_str(&earlier(context, &ids));
    Ok(Body {
        facts,
        diagram: true,
        prewritten: None,
    })
}

pub fn active_landscape(
    builder: &BriefBuilder,
    snapshot: &RepositorySnapshot,
    evidence: &mut Evidence<'_>,
) -> Result<Body, WikiError> {
    let git = builder.pages().git_snapshot().ok_or_else(|| {
        WikiError::InvalidId(
            "the active landscape needs Git history; refresh with Git analysis".into(),
        )
    })?;
    let facts_index = Facts::new(snapshot);
    let landscape = landscape::build(&git, &facts_index, evidence);
    Ok(Body {
        facts: landscape.render(),
        diagram: false,
        prewritten: None,
    })
}

/// Mined documents for the glossary, read through the evidence so the page records its inputs.
pub fn glossary_terms(evidence: &mut Evidence<'_>) -> Vec<glossary::Term> {
    let mut documents = Vec::new();
    for path in glossary::DOCUMENTS {
        if let Some(text) = evidence.read(path) {
            evidence.mention(path);
            documents.push((path.to_owned(), text));
        }
    }
    glossary::mine(&documents)
}

pub fn glossary(context: &BriefContext, evidence: &mut Evidence<'_>) -> Result<Body, WikiError> {
    let terms = glossary_terms(evidence);
    Ok(Body {
        facts: String::new(),
        diagram: false,
        prewritten: Some(glossary::render(&context.repository_name, &terms)),
    })
}

pub fn module(
    builder: &BriefBuilder,
    snapshot: &RepositorySnapshot,
    context: &BriefContext,
    group: &Group,
    evidence: &mut Evidence<'_>,
) -> Result<Body, WikiError> {
    let page = builder.pages().page(&BriefBuilder::page_id(group), None)?;
    let directory = group_path(group);
    let mut facts = readme_digest(evidence, &directory);
    facts.push_str(&format!(
        "## The module\n{} (`{directory}`): {}\n{}\n",
        group.label,
        page.summary,
        section_text(
            &page,
            &[
                "manifest",
                "languages",
                "entry_points",
                "boundary",
                "findings"
            ]
        )
    ));
    let parts = folder_tree::list(group).folders;
    let nodes: Vec<DiagramNode> = parts
        .iter()
        .map(|f| DiagramNode {
            label: f.group.label.clone(),
            members: f.group.members.clone(),
        })
        .collect();
    if nodes.len() >= 2 {
        facts.push_str("## Its parts (sub-folders) and who imports whom inside it\n");
        for part in &parts {
            let path = group_path(&part.group);
            evidence.mention(&path);
            facts.push_str(&format!(
                "- {} (`{path}`, {} files)\n",
                part.group.label,
                part.group.members.len()
            ));
        }
        let nodes = limit_nodes(nodes.clone());
        let mut edges: Vec<_> = component_weights(snapshot, &nodes).into_iter().collect();
        edges.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        for ((from, to), weight) in edges.into_iter().take(12) {
            facts.push_str(&format!(
                "- {} -> {} ({weight} imports)\n",
                nodes[from].label, nodes[to].label
            ));
        }
    }
    let centrality = Centrality::new(snapshot);
    let facts_index = Facts::new(snapshot);
    let mut members: Vec<&RootRelativePath> = group
        .members
        .iter()
        .filter(|m| !crate::tests_of::is_test_file(m.as_path()))
        .collect();
    members.sort_by(|a, b| {
        centrality
            .score(b)
            .total_cmp(&centrality.score(a))
            .then(a.cmp(b))
    });
    facts.push_str("\n## Key files (most depended-on first); their central code is below\n");
    for file in members.iter().take(MODULE_KEY_FILES) {
        let path = file.as_path().display().to_string();
        facts.push_str(&format!(
            "- `{path}`: imported by {} files\n",
            facts_index.dependent_files(file).len()
        ));
        excerpts::add(evidence, &path, 2, EXCERPT_LINES);
    }
    let used_by = section_text(
        &page,
        &["imported_by", "used_by", "depends_on", "dependencies"],
    );
    if !used_by.is_empty() {
        facts.push_str(&format!("\n## Connections\n{used_by}"));
    }
    facts.push_str(&earlier(context, &[super::brief::ARCHITECTURE.to_owned()]));
    Ok(Body {
        facts,
        diagram: nodes.len() >= 2,
        prewritten: None,
    })
}
