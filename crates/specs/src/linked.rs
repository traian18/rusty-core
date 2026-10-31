//! A change linked across the repositories of a workspace: one change folder per repository,
//! each with its own number, design, tasks and status, on one branch name. `change.md` names the
//! repository with the whole change's spec (`main`) and the other parts (`links`, as
//! `<repository>/<change folder>`, the repository relative to the workspace). This module reads
//! those links and writes what a run on one part needs to know about the others.
use crate::{
    context::push_document, layout::Layout, model::ChangeFront, parse, record::Step,
    store::SpecStore,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Part {
    /// The repository's folder, relative to the workspace (`rusty-core`, `apps/web`).
    pub repository: String,
    /// The change folder in that repository (`0003-dark-mode`).
    pub dir: String,
}

/// `rusty-core/0003-dark-mode` as a part; `None` without a repository or a folder.
pub fn parse_link(link: &str) -> Option<Part> {
    let (repository, dir) = link.trim().trim_end_matches('/').rsplit_once('/')?;
    (!repository.is_empty() && !dir.is_empty()).then(|| Part {
        repository: repository.to_owned(),
        dir: dir.to_owned(),
    })
}

pub fn link(part: &Part) -> String {
    format!("{}/{}", part.repository, part.dir)
}

/// A linked part with its repository's files, or `None` when the repository is not in the
/// workspace that is open.
pub struct Found<'a> {
    pub part: Part,
    pub store: Option<&'a dyn SpecStore>,
}

/// What a run on the part of `this` repository needs about the others: the parts and their state,
/// this part's job for `step`, and the other parts' documents the step works from. Empty for a
/// change that is not linked.
///
/// Every part is self-contained: it has the complete spec of what its repository delivers, and it
/// is planned, built and verified on its own, with its own repository's tests. The parts meet in
/// contracts, requirements written identically in each part that uses them and checked against a
/// test double, so no part ever waits for, or checks, another part's work.
pub fn compose(this: &str, front: &ChangeFront, step: Step, parts: &[Found]) -> String {
    if front.links.is_empty() {
        return String::new();
    }
    let main = front.main.as_deref().unwrap_or(this);
    let is_main = main == this;
    let repositories: Vec<&str> = std::iter::once(this)
        .chain(parts.iter().map(|f| f.part.repository.as_str()))
        .collect();
    let mut out = String::from("\n## This change spans several repositories\n\n");
    out.push_str(&format!(
        "This change is linked across the repositories {} of the workspace. Each has its own part: a change folder with its own complete spec, design, tasks and status, on the same branch, planned, built and verified on its own with its own repository's tests. The overview of the whole change (the discussion, how the work is split, the shared contracts) is the file overview.md in {main}'s part, when it was written; it is background, not a spec to verify. This run works on the part in {this}{}.\n\nThe other parts:\n",
        list(&repositories),
        if is_main { ", the main part" } else { "" },
    ));
    for found in parts {
        out.push_str(&format!("- {}\n", state_line(found, main)));
    }
    out.push('\n');
    out.push_str(&job(step, this, &repositories));
    out.push('\n');
    for found in parts {
        let Some(store) = found.store else { continue };
        let base = Layout::of(store).change(&found.part.dir);
        let repository = &found.part.repository;
        let from_main = found.part.repository == main;
        let wanted: &[(&str, &str)] = match step {
            Step::Specify if from_main => &[
                ("The overview of the whole change", "overview.md"),
                ("Its part of the spec", "spec.md"),
            ],
            Step::Specify => &[("Its part of the spec", "spec.md")],
            Step::Plan | Step::Implement => &[("Its design", "design.md")],
            Step::Start | Step::Capability => &[],
        };
        for (title, name) in wanted {
            let path = format!("{base}/{name}");
            if let Some(text) = store.read(&path) {
                push_document(
                    &mut out,
                    &format!("{repository}: {title} ({repository}/{path})"),
                    &text,
                );
            }
        }
    }
    out
}

fn state_line(found: &Found, main: &str) -> String {
    let Part { repository, dir } = &found.part;
    let role = if repository == main {
        ", the main spec"
    } else {
        ""
    };
    let Some(store) = found.store else {
        return format!(
            "{repository} ({dir}){role}: not in this workspace, so its files are not shown"
        );
    };
    let base = Layout::of(store).change(dir);
    let front = store
        .read(&format!("{base}/change.md"))
        .map(|t| parse::front::change(&t));
    let tasks = store
        .read(&format!("{base}/tasks.md"))
        .map(|t| parse::tasks::parse(&t).tasks)
        .unwrap_or_default();
    let status = front
        .as_ref()
        .and_then(|f| f.status)
        .map_or("no change.md", |s| s.word());
    let progress = if tasks.is_empty() {
        String::from("no tasks yet")
    } else {
        format!(
            "{} of {} tasks done",
            tasks.iter().filter(|t| t.done).count(),
            tasks.len()
        )
    };
    format!("{repository} ({dir}){role}: {status}, {progress}")
}

fn job(step: Step, this: &str, repositories: &[&str]) -> String {
    let others: Vec<&str> = repositories
        .iter()
        .copied()
        .filter(|r| *r != this)
        .collect();
    match step {
        Step::Specify => format!(
            "Your job in this part: write the complete spec of what {this} delivers, and only that. It must stand on its own: every requirement and criterion can be built and checked with {this}'s own tests, and none needs {} to be running or finished. Never write a `Repository:` line and never specify another repository's work. What {this} exchanges with {} (an API, a file format, an event, a command, a state) is a contract: write it as a requirement of its own with its exact shape (names, fields, values, errors), word for word the same as in the other parts' specs below when they already state it, and give it criteria that check {this}'s side against a test double or an example that follows that text (a provider checks that it produces the contract, a consumer that it handles it). A criterion that can only be seen with several repositories running together belongs in no spec: leave it out, or split it into one criterion per repository that each checks its own side. Keep your spec consistent with the parts already written below.",
            list(&others),
            list(&others)
        ),
        Step::Plan => format!(
            "Your job in this part: design and plan only the work in {this}, from its own spec. Never write a task, a criterion or a precondition that needs {} to do something first or to be running: the contract {this} exchanges with them is in your spec, keep it identical to the other parts' designs below (where they disagree, say so at the top of the design), and verify your side against a test double or an example that follows it. Work that belongs to another repository is not a task here.",
            list(&others)
        ),
        Step::Implement => format!(
            "Your job in this part: build only in the folder {this}/, from its own spec, design and tasks. The other parts ({}) are built in their own runs and never wait for or check each other: do not change their files, and check your side of each contract against a test double or an example. Never leave a task undone, or report it as not ready, because another repository has not done its share.",
            list(&others)
        ),
        Step::Start | Step::Capability => String::new(),
    }
}

fn list(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryStore;

    fn front(main: &str, links: &[&str]) -> ChangeFront {
        ChangeFront {
            main: Some(main.into()),
            links: links.iter().map(|l| (*l).to_owned()).collect(),
            ..Default::default()
        }
    }

    fn core_part() -> MemoryStore {
        let store = MemoryStore::default();
        store.insert(
            "specs/changes/0004-dark/change.md",
            "---\nstatus: planned\n---\n",
        );
        store.insert("specs/changes/0004-dark/spec.md", "# Core part\n");
        store.insert("specs/changes/0004-dark/design.md", "# Core design\n");
        store.insert(
            "specs/changes/0004-dark/tasks.md",
            "- [x] **T1** a. R1\n- [ ] **T2** b. R1\n",
        );
        store
    }

    #[test]
    fn links_name_a_repository_and_a_change_folder() {
        assert_eq!(
            parse_link("apps/web/0002-dark"),
            Some(Part {
                repository: "apps/web".into(),
                dir: "0002-dark".into()
            })
        );
        assert_eq!(parse_link("0002-dark"), None);
        assert_eq!(parse_link("/0002-dark"), None);
        let part = parse_link("rusty-core/0004-dark").unwrap();
        assert_eq!(link(&part), "rusty-core/0004-dark");
    }

    #[test]
    fn a_change_without_links_adds_nothing() {
        assert_eq!(
            compose("rusty-ide", &ChangeFront::default(), Step::Specify, &[]),
            ""
        );
    }

    #[test]
    fn every_part_specifies_only_its_own_share_and_sees_the_parts() {
        let core = core_part();
        core.insert("specs/changes/0004-dark/overview.md", "# Overview\n");
        let parts = [
            Found {
                part: parse_link("rusty-core/0004-dark").unwrap(),
                store: Some(&core),
            },
            Found {
                part: parse_link("rusty-website/0002-dark").unwrap(),
                store: None,
            },
        ];
        let text = compose(
            "rusty-ide",
            &front(
                "rusty-ide",
                &["rusty-core/0004-dark", "rusty-website/0002-dark"],
            ),
            Step::Specify,
            &parts,
        );
        assert!(text.contains("rusty-ide, rusty-core and rusty-website"));
        assert!(text.contains("the part in rusty-ide, the main part"));
        assert!(text.contains("- rusty-core (0004-dark): planned, 1 of 2 tasks done"));
        assert!(text.contains("rusty-website (0002-dark): not in this workspace"));
        assert!(text.contains("write the complete spec of what rusty-ide delivers, and only that"));
        assert!(text.contains("none needs rusty-core and rusty-website to be running or finished"));
        assert!(
            text.contains("never write a `Repository:` line")
                || text.contains("Never write a `Repository:` line")
        );
        assert!(!text.contains("spec of the whole change"));
        assert!(text.contains(
            "rusty-core: Its part of the spec (rusty-core/specs/changes/0004-dark/spec.md)"
        ));
        assert!(text.contains("# Core part") && !text.contains("# Core design"));
        // The overview is only shown from the main part.
        assert!(!text.contains("# Overview"));
    }

    #[test]
    fn the_overview_of_the_main_part_is_background_for_the_others() {
        let main = MemoryStore::default();
        main.insert("specs/changes/0001-dark/overview.md", "# Overview\n");
        main.insert("specs/changes/0001-dark/spec.md", "# Ide part\n");
        let parts = [Found {
            part: parse_link("rusty-ide/0001-dark").unwrap(),
            store: Some(&main),
        }];
        let text = compose(
            "rusty-core",
            &front(
                "rusty-ide",
                &["rusty-ide/0001-dark", "rusty-core/0004-dark"],
            ),
            Step::Specify,
            &parts,
        );
        assert!(text.contains(
            "The overview of the whole change (rusty-ide/specs/changes/0001-dark/overview.md)"
        ));
        assert!(text.contains("it is background, not a spec to verify"));
    }

    #[test]
    fn a_part_plans_and_builds_on_its_own_against_the_contract() {
        let main = MemoryStore::default();
        main.insert("specs/changes/0001-dark/spec.md", "# Ide spec\n");
        main.insert("specs/changes/0001-dark/design.md", "# Ide design\n");
        let core = core_part();
        let parts = [
            Found {
                part: parse_link("rusty-ide/0001-dark").unwrap(),
                store: Some(&main),
            },
            Found {
                part: parse_link("rusty-website/0004-dark").unwrap(),
                store: Some(&core),
            },
        ];
        let links = ["rusty-ide/0001-dark", "rusty-website/0004-dark"];
        let plan = compose(
            "rusty-core",
            &front("rusty-ide", &links),
            Step::Plan,
            &parts,
        );
        assert!(plan.contains("design and plan only the work in rusty-core"));
        assert!(plan.contains("Never write a task, a criterion or a precondition that needs rusty-ide and rusty-website"));
        assert!(plan.contains("# Ide design") && plan.contains("# Core design"));
        assert!(!plan.contains("# Ide spec"));
        let implement = compose(
            "rusty-core",
            &front("rusty-ide", &links),
            Step::Implement,
            &parts,
        );
        assert!(implement.contains("build only in the folder rusty-core/"));
        assert!(implement.contains("never wait for or check each other"));
        assert!(!implement.contains("# Ide spec"));
    }
}
