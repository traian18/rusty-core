//! `repo_wiki`: cited documentation pages for a file, folder, package or the whole repository, and
//! the tree to walk them. Read-only; pages are rendered from the published snapshots, so they are
//! as fresh as the snapshot and say so when the file has changed since.
use async_trait::async_trait;
use harness_repository_wiki::{PageId, PageLevel, PageRenderer, WikiError};
use harness_tools::CancellationToken;
use harness_workspace::{folder_tree, repository_intelligence::RootRelativePath};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    args,
    failure::Failure,
    graph::GraphContext,
    operation::{Operation, Success},
};

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Action {
    Page,
    Children,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    action: Option<Action>,
    target: Option<String>,
    format: Option<Format>,
    snapshot: Option<String>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Format {
    Markdown,
    Json,
}

pub(crate) struct RepoWiki(pub GraphContext);

#[async_trait]
impl Operation for RepoWiki {
    fn name(&self) -> &'static str {
        "repo_wiki"
    }

    fn description(&self) -> &'static str {
        "Read the repository's documentation pages: what a file, folder, package or the whole repository is, what it contains, what it imports and who imports it, framework links, tests, findings and history, each fact with the file and line range it comes from. action 'page' (default) returns one page as Markdown (or JSON with format 'json'); action 'children' lists the pages one level below, to walk the tree. target is a page id (file:src/a.rs, folder:dir:src, package:<group id>, repo:), a workspace path, or empty for the repository. Pages are rendered from the index and say when the file changed since it was built (status stale). Read-only."
    }

    fn schema(&self) -> Value {
        json!({"type":"object","properties":{
            "action":{"type":"string","enum":["page","children"],"description":"Default page"},
            "target":{"type":"string","description":"A page id, a workspace path, or empty for the repository"},
            "format":{"type":"string","enum":["markdown","json"],"description":"Default markdown"},
            "snapshot":{"type":"string"}},
            "additionalProperties":false})
    }

    async fn run(&self, arguments: Value, cancel: &CancellationToken) -> Result<Success, Failure> {
        let args: Arguments = args::parse(arguments)?;
        let snapshot = self.0.snapshot(&args.snapshot)?;
        if cancel.is_cancelled() {
            return Err(Failure::cancelled());
        }
        let id = resolve_id(args.target.as_deref().unwrap_or(""), &snapshot);
        let mut renderer = PageRenderer::new(self.0.graph.clone());
        if let Some(git) = &self.0.git {
            renderer = renderer.with_git(git.clone());
        }
        if let Some(findings) = &self.0.findings {
            renderer = renderer.with_findings(findings.clone());
        }
        let result = match args.action.unwrap_or(Action::Page) {
            Action::Page => {
                let page = renderer.page(&id, Some(&snapshot.id)).map_err(failure)?;
                match args.format.unwrap_or(Format::Markdown) {
                    Format::Markdown => json!({
                        "id": page.id, "level": page.level, "title": page.title, "status": page.status,
                        "language": page.language, "digest": page.digest, "gaps": page.gaps,
                        "markdown": page.to_markdown(),
                        "next": [{"tool": "repo_wiki", "arguments": {"action": "children", "target": id.0}, "why": "the pages one level below"}],
                    }),
                    Format::Json => json!({"page": page}),
                }
            }
            Action::Children => {
                json!({"id": id, "children": renderer.children(&id, Some(&snapshot.id)).map_err(failure)?})
            }
        };
        Ok(self.0.success(&snapshot, None, result))
    }
}

fn failure(error: WikiError) -> Failure {
    Failure::new(error.code(), error.to_string())
}

/// A page id from what the caller wrote: an id as is, a path that names an indexed file, a folder
/// path, or the repository.
fn resolve_id(
    raw: &str,
    snapshot: &harness_workspace::repository_intelligence::RepositorySnapshot,
) -> PageId {
    let raw = raw.trim();
    if raw.is_empty() || raw == "." || raw == "/" {
        return PageId::repository();
    }
    let id = PageId(raw.to_owned());
    if id.parse().is_ok()
        && raw.contains(':')
        && !raw.starts_with(folder_tree::DIRECTORY_PREFIX)
        && !raw.starts_with(folder_tree::REST_PREFIX)
    {
        return id;
    }
    if raw.starts_with(folder_tree::DIRECTORY_PREFIX) || raw.starts_with(folder_tree::REST_PREFIX) {
        return PageId::folder(raw);
    }
    let trimmed = raw.trim_end_matches('/');
    let is_file = RootRelativePath::new(trimmed)
        .ok()
        .is_some_and(|path| snapshot.imports.has_file(&path));
    if is_file || !raw.ends_with('/') && std::path::Path::new(trimmed).extension().is_some() {
        PageId::file(trimmed)
    } else {
        PageId::new(
            PageLevel::Folder,
            &format!("{}{trimmed}", folder_tree::DIRECTORY_PREFIX),
        )
    }
}
