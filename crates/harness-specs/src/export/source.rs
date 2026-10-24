//! This project's changes in one plain shape, whichever tool they are written out for.
use crate::{
    import::common::{body, sections},
    layout::{parse_dir_name, Layout},
    model::{Requirement, Task},
    parse::{self, front},
    store::SpecStore,
};

pub struct ChangeDoc {
    /// The folder name in this layout, e.g. `0007-photo-albums`.
    pub dir: String,
    pub number: u32,
    pub slug: String,
    pub title: String,
    pub created: String,
    pub archived: bool,
    pub why: String,
    pub what: String,
    pub requirements: Vec<Requirement>,
    pub open_questions: Vec<String>,
    pub checklist: Option<String>,
    pub design: Option<String>,
    pub research: Option<String>,
    pub tasks: Vec<Task>,
}

/// Every change, active ones first, then archived ones, each in number order.
pub fn changes(store: &dyn SpecStore, layout: &Layout) -> Vec<ChangeDoc> {
    let mut found = Vec::new();
    for (parent, archived) in [(layout.changes(), false), (layout.archive(), true)] {
        for dir in store.dirs(&parent) {
            if let Some(doc) = read(store, &format!("{parent}/{dir}"), &dir, archived) {
                found.push(doc);
            }
        }
    }
    found
}

fn read(store: &dyn SpecStore, base: &str, dir: &str, archived: bool) -> Option<ChangeDoc> {
    let (number, slug) = parse_dir_name(dir)?;
    let spec_text = store.read(&format!("{base}/spec.md"))?;
    let spec = parse::spec::parse(&spec_text);
    let front = store
        .read(&format!("{base}/change.md"))
        .map(|t| front::change(&t))
        .unwrap_or_default();
    let sections = sections(&spec_text);
    let section = |name: &str| {
        sections
            .iter()
            .find(|(h, _)| h == name)
            .map(|(_, lines)| body(lines))
            .unwrap_or_default()
    };
    Some(ChangeDoc {
        dir: dir.to_owned(),
        number,
        slug,
        title: front.title.or(spec.title).unwrap_or_else(|| dir.to_owned()),
        created: front.created.unwrap_or_default(),
        archived,
        why: section("why"),
        what: section("what changes"),
        requirements: spec.requirements,
        open_questions: spec.open_questions.into_iter().map(|(_, q)| q).collect(),
        checklist: store.read(&format!("{base}/checklist.md")),
        design: store.read(&format!("{base}/design.md")),
        research: store.read(&format!("{base}/research.md")),
        tasks: store
            .read(&format!("{base}/tasks.md"))
            .map(|t| parse::tasks::parse(&t).tasks)
            .unwrap_or_default(),
    })
}
