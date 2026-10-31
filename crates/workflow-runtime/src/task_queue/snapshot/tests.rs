use super::*;
use std::fs;

fn repo_with_commit(dir: &Path) -> Repository {
    let repo = Repository::init(dir).unwrap();
    fs::write(dir.join("kept.ts"), "kept\n").unwrap();
    fs::write(dir.join("changed.ts"), "before\n").unwrap();
    fs::write(dir.join("deleted.ts"), "deleted\n").unwrap();
    fs::write(dir.join(".gitignore"), "ignored/\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_all(["*"], IndexAddOption::DEFAULT, None).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let me = Signature::now("Me", "me@example.com").unwrap();
    repo.commit(Some("HEAD"), &me, &me, "init", &tree, &[])
        .unwrap();
    drop(tree);
    repo
}

fn status(repo: &Repository) -> Vec<(String, git2::Status)> {
    let mut options = git2::StatusOptions::new();
    options.include_untracked(true).include_ignored(false);
    let mut list: Vec<_> = repo
        .statuses(Some(&mut options))
        .unwrap()
        .iter()
        .map(|entry| (entry.path().unwrap().to_owned(), entry.status()))
        .collect();
    list.sort_by(|a, b| a.0.cmp(&b.0));
    list
}

#[test]
fn undo_puts_back_the_tree_the_task_started_from() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let repo = repo_with_commit(root);
    // Uncommitted work from earlier tasks is part of the baseline.
    fs::write(root.join("changed.ts"), "earlier task\n").unwrap();
    fs::write(root.join("earlier_new.ts"), "earlier\n").unwrap();
    let head = repo.head().unwrap().target();
    let before = status(&repo);

    let baseline = take(root, "run/T4-start");
    assert_eq!(baseline.repos.len(), 1);

    // The failed attempt.
    fs::write(root.join("changed.ts"), "broken attempt\n").unwrap();
    fs::remove_file(root.join("deleted.ts")).unwrap();
    fs::write(root.join("attempt_new.ts"), "new\n").unwrap();
    fs::create_dir_all(root.join("ignored")).unwrap();
    fs::write(root.join("ignored/cache"), "keep me\n").unwrap();

    let undone = undo_to(root, &baseline, "run/T4-attempt-1").unwrap();
    let mut files = undone.files.clone();
    files.sort();
    assert_eq!(files, ["attempt_new.ts", "changed.ts", "deleted.ts"]);
    assert_eq!(
        fs::read_to_string(root.join("changed.ts")).unwrap(),
        "earlier task\n"
    );
    assert_eq!(
        fs::read_to_string(root.join("deleted.ts")).unwrap(),
        "deleted\n"
    );
    assert_eq!(
        fs::read_to_string(root.join("earlier_new.ts")).unwrap(),
        "earlier\n"
    );
    assert!(!root.join("attempt_new.ts").exists());
    assert_eq!(
        fs::read_to_string(root.join("ignored/cache")).unwrap(),
        "keep me\n"
    );

    // Branch, index and status are as they were.
    assert_eq!(repo.head().unwrap().target(), head);
    assert_eq!(status(&repo), before);

    // The undone attempt is kept under its own ref.
    let kept = repo
        .find_reference("refs/rusty/checkpoints/run/T4-attempt-1")
        .unwrap();
    let tree = kept.peel_to_commit().unwrap().tree().unwrap();
    let blob = repo
        .find_blob(tree.get_path(Path::new("changed.ts")).unwrap().id())
        .unwrap();
    assert_eq!(blob.content(), b"broken attempt\n");
}

#[test]
fn a_workspace_of_repositories_snapshots_each_of_them() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("app")).unwrap();
    fs::create_dir_all(root.join("core")).unwrap();
    fs::create_dir_all(root.join("notes")).unwrap();
    repo_with_commit(&root.join("app"));
    repo_with_commit(&root.join("core"));

    let baseline = take(root, "run/T1-start");
    let names: Vec<_> = baseline
        .repos
        .iter()
        .map(|repo| repo.repo.as_str())
        .collect();
    assert_eq!(names, ["app", "core"]);

    fs::write(root.join("core/changed.ts"), "attempt\n").unwrap();
    let undone = undo_to(root, &baseline, "run/T1-attempt-1").unwrap();
    assert_eq!(undone.files, ["core/changed.ts"]);
    assert_eq!(
        fs::read_to_string(root.join("core/changed.ts")).unwrap(),
        "before\n"
    );
}

#[test]
fn repositories_grouped_in_folders_are_snapshotted_and_undone_too() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("services/payments/api")).unwrap();
    fs::create_dir_all(root.join("apps/web")).unwrap();
    repo_with_commit(&root.join("services/payments/api"));
    repo_with_commit(&root.join("apps/web"));

    let baseline = take(root, "run/T1-start");
    let names: Vec<_> = baseline
        .repos
        .iter()
        .map(|repo| repo.repo.as_str())
        .collect();
    assert_eq!(names, ["apps/web", "services/payments/api"]);

    fs::write(root.join("services/payments/api/changed.ts"), "attempt\n").unwrap();
    let undone = undo_to(root, &baseline, "run/T1-attempt-1").unwrap();
    assert_eq!(undone.files, ["services/payments/api/changed.ts"]);
    assert_eq!(
        fs::read_to_string(root.join("services/payments/api/changed.ts")).unwrap(),
        "before\n"
    );
}

#[test]
fn a_workspace_repository_keeps_the_repositories_inside_it_in_their_own_snapshots() {
    // The Rusty workspace layout: the folder is a repository that ignores the
    // repositories in it, which hold the code.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let workspace = Repository::init(root).unwrap();
    fs::write(root.join(".gitignore"), "/*\n!/.gitignore\n").unwrap();
    let mut index = workspace.index().unwrap();
    index.add_path(Path::new(".gitignore")).unwrap();
    let tree = workspace.find_tree(index.write_tree().unwrap()).unwrap();
    let who = git2::Signature::now("t", "t@example.com").unwrap();
    workspace
        .commit(Some("HEAD"), &who, &who, "init", &tree, &[])
        .unwrap();
    fs::create_dir_all(root.join("core")).unwrap();
    repo_with_commit(&root.join("core"));

    let baseline = take(root, "run/T1-start");
    let names: Vec<_> = baseline
        .repos
        .iter()
        .map(|repo| repo.repo.as_str())
        .collect();
    assert_eq!(names, ["", "core"]);
    fs::write(root.join("core/changed.ts"), "attempt\n").unwrap();
    assert_eq!(
        undo_to(root, &baseline, "run/T1-attempt-1").unwrap().files,
        ["core/changed.ts"]
    );
}

#[test]
fn a_folder_without_repositories_has_nothing_to_undo() {
    let dir = tempfile::tempdir().unwrap();
    let baseline = take(dir.path(), "run/T1-start");
    assert!(baseline.repos.is_empty());
    assert!(undo_to(dir.path(), &baseline, "x")
        .unwrap()
        .files
        .is_empty());
}

#[test]
fn labels_become_valid_ref_names() {
    assert_eq!(
        ref_name("run 1/T4:start"),
        "refs/rusty/checkpoints/run-1/T4-start"
    );
}

#[test]
fn a_fingerprint_changes_only_with_the_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    repo_with_commit(root);
    let first = fingerprint(root);
    assert_eq!(first.len(), 1);
    // Recording a checkpoint is not a change.
    take(root, "run/T1-start");
    assert_eq!(fingerprint(root), first);
    fs::write(root.join("changed.ts"), "after\n").unwrap();
    assert_ne!(fingerprint(root), first);
}

#[test]
fn an_undone_attempt_keeps_its_diff() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    repo_with_commit(root);
    let baseline = take(root, "run/T2-start");
    fs::write(root.join("changed.ts"), "after\n").unwrap();
    let undone = undo_to(root, &baseline, "run/T2-undone").unwrap();
    let diff = patch(root, &baseline, &undone.attempt, 10_000);
    assert!(diff.contains("-before\n+after\n"), "{diff}");
    let cut = patch(root, &baseline, &undone.attempt, 10);
    assert!(
        cut.ends_with("(diff cut; the whole attempt is kept under its ref)"),
        "{cut}"
    );
}
