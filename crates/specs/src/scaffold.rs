//! Setting up the skeleton and creating a change from templates. Nothing is overwritten.
use crate::{
    layout::{branch_name, change_dir_name, next_number, slugify, Layout},
    model::{ChangeFront, Status},
    parse::front,
    store::{SpecError, SpecStore},
    templates,
};

/// Writes `README.md` and `project.md` where missing. Returns the paths it created.
pub fn init(store: &dyn SpecStore) -> Result<Vec<String>, SpecError> {
    let layout = Layout::of(store);
    let mut created = Vec::new();
    for (path, text) in [
        (layout.readme(), templates::readme(&layout.root)),
        (layout.project(), templates::project()),
    ] {
        if store.read(&path).is_none() {
            store.write(&path, &text)?;
            created.push(path);
        }
    }
    Ok(created)
}

/// What a new change would be called, before anything is written (the host creates the branch
/// between planning and creating, so the files land on it).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewChange {
    pub number: u32,
    pub slug: String,
    pub dir: String,
    pub branch: String,
}

pub fn plan_change(store: &dyn SpecStore, title: &str) -> NewChange {
    let layout = Layout::of(store);
    let changes = store.dirs(&layout.changes());
    let archived = store.dirs(&layout.archive());
    let number = next_number(changes.iter().chain(&archived).map(String::as_str));
    let slug = slugify(title);
    let dir = change_dir_name(number, &slug);
    NewChange {
        number,
        branch: branch_name(&dir),
        slug,
        dir,
    }
}

pub struct ChangeRequest<'a> {
    pub title: &'a str,
    pub description: &'a str,
    /// `YYYY-MM-DD`, from the caller's clock.
    pub date: &'a str,
    /// The branch the caller created and switched to, if any.
    pub branch: Option<&'a str>,
    /// Set when the change is one part of a change linked across repositories.
    pub linked: Option<LinkedRequest<'a>>,
}

pub struct LinkedRequest<'a> {
    /// The repository, relative to the workspace, that holds the whole change's spec.
    pub main: &'a str,
    /// The other parts, as `<repository>/<change folder>`.
    pub links: &'a [String],
}

/// Writes `change.md` and `spec.md`; returns their paths. Refuses to touch an existing change.
pub fn create_change(
    store: &dyn SpecStore,
    plan: &NewChange,
    request: &ChangeRequest,
) -> Result<Vec<String>, SpecError> {
    let layout = Layout::of(store);
    let base = layout.change(&plan.dir);
    if !store.files(&base).is_empty() {
        return Err(SpecError::new(format!("{} already exists", plan.dir)));
    }
    let title = request.title.trim();
    let front = ChangeFront {
        id: Some(format!("{:04}", plan.number)),
        title: Some(title.to_owned()),
        status: Some(Status::Draft),
        created: Some(request.date.to_owned()),
        branch: request.branch.map(str::to_owned),
        capabilities: Vec::new(),
        main: request.linked.as_ref().map(|l| l.main.to_owned()),
        links: request
            .linked
            .as_ref()
            .map(|l| l.links.to_vec())
            .unwrap_or_default(),
        ..Default::default()
    };
    let mut history = vec![format!("{} Created.", request.date)];
    if let Some(branch) = request.branch {
        history.push(format!("{} Branch {branch} created.", request.date));
    }
    if let Some(linked) = &request.linked {
        history.push(format!(
            "{} Linked with {}.",
            request.date,
            linked.links.join(", ")
        ));
    }
    let files = [
        (
            format!("{base}/change.md"),
            front::render_change(&front, &history),
        ),
        (
            format!("{base}/spec.md"),
            templates::spec(title, request.description),
        ),
    ];
    let mut created = Vec::new();
    for (path, text) in files {
        store.write(&path, &text)?;
        created.push(path);
    }
    Ok(created)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{overview, store::MemoryStore};

    #[test]
    fn init_writes_the_skeleton_once_and_never_overwrites() {
        let store = MemoryStore::default();
        assert_eq!(
            init(&store).unwrap(),
            ["specs/README.md", "specs/project.md"]
        );
        store.insert("specs/project.md", "mine");
        assert!(init(&store).unwrap().is_empty());
        assert_eq!(store.read("specs/project.md").as_deref(), Some("mine"));
    }

    #[test]
    fn a_new_change_gets_the_next_number_across_changes_and_archive() {
        let store = MemoryStore::default();
        store.insert("specs/changes/0003-a/spec.md", "x");
        store.insert("specs/archive/2026-01-01-0005-b/change.md", "x");
        let plan = plan_change(&store, "Photo albums!");
        assert_eq!(
            (plan.number, plan.dir.as_str(), plan.branch.as_str()),
            (6, "0006-photo-albums", "spec/0006-photo-albums")
        );
    }

    #[test]
    fn a_created_change_shows_up_as_a_draft_with_its_branch() {
        let store = MemoryStore::default();
        let plan = plan_change(&store, "Photo albums");
        let request = ChangeRequest {
            title: "Photo albums",
            description: "Group shoots.",
            date: "2026-10-23",
            branch: Some(&plan.branch),
            linked: None,
        };
        create_change(&store, &plan, &request).unwrap();
        let found = overview(&store);
        let change = &found.changes[0];
        assert_eq!(
            (
                change.title.as_str(),
                change.status,
                change.branch.as_deref()
            ),
            (
                "Photo albums",
                Status::Draft,
                Some("spec/0001-photo-albums")
            )
        );
        assert_eq!(change.documents, ["change.md", "spec.md"]);
        assert!(store
            .read("specs/changes/0001-photo-albums/spec.md")
            .unwrap()
            .contains("Group shoots."));
        assert!(store
            .read("specs/changes/0001-photo-albums/change.md")
            .unwrap()
            .contains("Branch spec/0001-photo-albums created."));
    }

    #[test]
    fn an_existing_change_is_never_replaced() {
        let store = MemoryStore::default();
        let plan = plan_change(&store, "A");
        let request = ChangeRequest {
            title: "A",
            description: "",
            date: "2026-10-23",
            branch: None,
            linked: None,
        };
        create_change(&store, &plan, &request).unwrap();
        assert!(create_change(&store, &plan, &request).is_err());
    }
}
