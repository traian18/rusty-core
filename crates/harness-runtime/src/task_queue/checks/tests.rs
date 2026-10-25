use super::*;

const FAILED: &str = "typecheck for rusty-ide: FAILED\n\n[1/2] npm run typecheck (cwd: rusty-ide): FAILED (exit 2) in 7.5s\n  2 errors:\n  - src/components/tabs/AgentTab.tsx:41:40 [TS2345] Argument of type 'TurnChat' is not assignable.\n  - src/components/tabs/agentTab/useAgentTab.ts:2:25 [TS6133] 'userPrompts' is declared but its value is never read.\n\nOverall: FAILED (1 of 2 commands failed).";

fn known(errors: &[&str]) -> CheckErrors {
    CheckErrors {
        tree: "t".into(),
        count: errors.len() as u32,
        errors: errors.iter().map(|error| error.to_string()).collect(),
    }
}

#[test]
fn a_failed_check_lists_its_errors_without_lines() {
    let (count, errors) = parse(true, FAILED).unwrap();
    assert_eq!(count, 2);
    assert_eq!(
        errors,
        [
            "src/components/tabs/AgentTab.tsx [TS2345] Argument of type 'TurnChat' is not assignable.",
            "src/components/tabs/agentTab/useAgentTab.ts [TS6133] 'userPrompts' is declared but its value is never read.",
        ]
    );
    assert_eq!(parse(false, "typecheck: PASSED"), Some((0, Vec::new())));
    // A failure that lists nothing (no typecheck here) is no result.
    assert_eq!(
        parse(true, "No typecheck check found for this project."),
        None
    );
}

#[test]
fn only_errors_the_build_added_count() {
    let (count, errors) = parse(true, FAILED).unwrap();
    let before = known(&[
        "src/components/tabs/AgentTab.tsx [TS2345] Argument of type 'TurnChat' is not assignable.",
    ]);
    assert_eq!(
        added(&before, count, &errors),
        ["src/components/tabs/agentTab/useAgentTab.ts [TS6133] 'userPrompts' is declared but its value is never read."]
    );
    let all = CheckErrors {
        tree: "t".into(),
        count,
        errors: errors.clone(),
    };
    assert!(added(&all, count, &errors).is_empty());
    // A cut list: the count says more errors appeared.
    let cut = CheckErrors {
        count: 1,
        ..all.clone()
    };
    assert_eq!(
        added(&cut, 5, &errors),
        ["5 errors now, 1 before this task"]
    );
}

#[test]
fn the_same_error_twice_counts_once_more() {
    let error = "src/a.ts [TS2304] Cannot find name 'x'.".to_owned();
    let before = known(&["src/a.ts [TS2304] Cannot find name 'x'."]);
    assert_eq!(added(&before, 2, &[error.clone(), error.clone()]), [error]);
}

#[test]
fn a_task_is_checked_in_the_repositories_it_names() {
    let repos = vec![
        ("rusty-core".to_owned(), "a".to_owned()),
        ("rusty-ide".to_owned(), "b".to_owned()),
    ];
    let task = Task {
        id: "T1".into(),
        requirement_ids: vec![],
        criterion_ids: vec![],
        depends_on: vec![],
        instructions: "Change `rusty-ide/src/turn.ts`.".into(),
        flow: None,
    };
    assert_eq!(named_repos(&repos, &task, None), [repos[1].clone()]);
    assert_eq!(
        named_repos(&repos, &task, Some("and rusty-core too")),
        repos
    );
    let vague = Task {
        instructions: "Change the turn.".into(),
        ..task
    };
    assert_eq!(named_repos(&repos, &vague, None), repos);
}
