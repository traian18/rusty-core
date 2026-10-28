use harness_goals::{
    decide, pass_prompt, record, status_text, CheckOutcome, FileMeasure, FindingSummary, Goal,
    GoalStatus, GuidelineSummary, Measurement, PassReport, Severity, Targets, Verdict,
};

fn finding(severity: Severity) -> FindingSummary {
    FindingSummary {
        id: "f1".into(),
        issue: "complex method".into(),
        severity,
        lines: "10-80".into(),
        reason: "ccn 14".into(),
        worth: 1.2,
    }
}

fn measured(
    score: f64,
    findings: Vec<FindingSummary>,
    test_passes: bool,
    fingerprint: &str,
) -> Measurement {
    Measurement {
        files: vec![FileMeasure {
            path: "src/run.ts".into(),
            score: Some(score),
            findings,
        }],
        guideline_errors: Vec::new(),
        guideline_warnings: 0,
        checks: vec![CheckOutcome {
            name: "test".into(),
            passed: test_passes,
            ran: true,
            summary: if test_passes {
                String::new()
            } else {
                "1 failed".into()
            },
        }],
        fingerprint: fingerprint.into(),
    }
}

fn goal() -> Goal {
    let mut goal = Goal::new("g1".into(), "Make run.ts healthy".into(), "t0".into());
    goal.scope = vec!["src/run.ts".into()];
    goal.targets = Targets {
        min_file_health: Some(8.5),
        max_severity: Some(Severity::Medium),
        checks: vec!["test".into()],
        ..Targets::default()
    };
    goal.baseline = Some(measured(5.0, vec![finding(Severity::High)], true, "a"));
    goal
}

fn pass(goal: &mut Goal, now: Measurement, report: PassReport) -> Verdict {
    let decision = decide(goal, &now, &report);
    record(goal, &decision, now, &report, "t".into());
    decision.verdict
}

#[test]
fn passes_go_on_while_they_improve_and_stop_when_the_measurement_says_done() {
    let mut goal = goal();
    assert_eq!(
        pass(
            &mut goal,
            measured(7.0, vec![finding(Severity::Medium)], true, "b"),
            PassReport::default()
        ),
        Verdict::Continue
    );
    assert_eq!(goal.status, GoalStatus::Running);
    // The agent's "done" does not end the goal; the measurement does.
    let says_done = PassReport {
        agent_status: Some("done".into()),
        ..PassReport::default()
    };
    assert_eq!(
        pass(
            &mut goal,
            measured(8.0, vec![], true, "c"),
            says_done.clone()
        ),
        Verdict::Continue
    );
    assert_eq!(
        pass(&mut goal, measured(9.1, vec![], true, "d"), says_done),
        Verdict::Done
    );
    assert_eq!(goal.status, GoalStatus::Done);
    assert_eq!(goal.passes, 3);
    assert_eq!(goal.journal.len(), 3);
}

#[test]
fn a_pass_that_breaks_a_check_is_undone_and_the_next_starts_from_the_last_good_state() {
    let mut goal = goal();
    pass(
        &mut goal,
        measured(7.0, vec![], true, "b"),
        PassReport::default(),
    );
    let decision = decide(
        &goal,
        &measured(9.0, vec![], false, "c"),
        &PassReport::default(),
    );
    assert!(decision.undo);
    assert_eq!(decision.verdict, Verdict::Undone);
    record(
        &mut goal,
        &decision,
        measured(9.0, vec![], false, "c"),
        &PassReport::default(),
        "t".into(),
    );
    assert_eq!(goal.last.as_ref().unwrap().fingerprint, "b");
    assert!(pass_prompt(&goal, goal.last.as_ref().unwrap()).contains("undone"));
}

#[test]
fn passes_without_progress_pause_the_goal() {
    let mut goal = goal();
    assert_eq!(
        pass(
            &mut goal,
            measured(5.0, vec![finding(Severity::High)], true, "a"),
            PassReport::default()
        ),
        Verdict::Continue
    );
    assert_eq!(goal.stalled, 1);
    assert_eq!(
        pass(
            &mut goal,
            measured(5.0, vec![finding(Severity::High)], true, "a"),
            PassReport::default()
        ),
        Verdict::NoProgress
    );
    assert_eq!(goal.status, GoalStatus::Paused);
    assert!(goal
        .reason
        .as_deref()
        .unwrap()
        .contains("No measurable progress"));
    goal.resume("t2".into());
    assert_eq!(
        (goal.status, goal.passes, goal.stalled),
        (GoalStatus::Running, 0, 0)
    );
}

#[test]
fn the_budget_pauses_and_a_blocker_blocks() {
    let mut goal = goal();
    goal.budget.max_passes = 2;
    pass(
        &mut goal,
        measured(6.0, vec![], true, "b"),
        PassReport::default(),
    );
    assert_eq!(
        pass(
            &mut goal,
            measured(7.0, vec![], true, "c"),
            PassReport::default()
        ),
        Verdict::Budget
    );
    assert_eq!(goal.status, GoalStatus::Paused);

    let mut goal = self::goal();
    let blocked = PassReport {
        agent_status: Some("blocked — needs the API key".into()),
        ..PassReport::default()
    };
    assert_eq!(
        pass(&mut goal, measured(6.0, vec![], true, "b"), blocked),
        Verdict::Blocked
    );
    assert_eq!(goal.reason.as_deref(), Some("Blocked: needs the API key"));
}

#[test]
fn done_when_in_words_waits_for_the_judge_once_everything_measured_is_met() {
    let mut goal = goal();
    goal.targets.done_when = Some("the module has no I/O in its pure functions".into());
    let met = measured(9.0, vec![], true, "b");
    let not_yet = PassReport {
        done_when_met: Some(false),
        judge_reason: Some("load() still reads the disk".into()),
        ..PassReport::default()
    };
    let decision = decide(&goal, &met, &not_yet);
    assert_eq!(decision.verdict, Verdict::Continue);
    assert!(decision.reason.contains("load() still reads the disk"));
    let yes = PassReport {
        done_when_met: Some(true),
        ..PassReport::default()
    };
    assert_eq!(
        decide(&goal, &measured(9.0, vec![], true, "c"), &yes).verdict,
        Verdict::Done
    );
}

#[test]
fn guideline_errors_are_a_target_and_more_of_them_is_worse() {
    let mut goal = goal();
    goal.targets = Targets::default();
    let mut now = measured(5.0, vec![], true, "b");
    now.guideline_errors.push(GuidelineSummary {
        rule: "no-unwrap".into(),
        path: "src/run.ts".into(),
        line: 3,
        message: "x".into(),
    });
    let decision = decide(&goal, &now, &PassReport::default());
    assert_eq!(decision.verdict, Verdict::Undone);
    assert_eq!(decision.unmet[0].target, "no guideline errors");
}

#[test]
fn the_prompt_and_status_say_where_the_goal_stands() {
    let goal = goal();
    let prompt = pass_prompt(&goal, goal.baseline.as_ref().unwrap());
    for expected in [
        "Goal pass 1 of at most 8",
        "Make run.ts healthy",
        "at least 8.5",
        "`test` passes",
        "`src/run.ts`: health 5",
        "complex method",
        "Status: done",
    ] {
        assert!(prompt.contains(expected), "missing {expected}:\n{prompt}");
    }
    assert!(status_text(&goal).starts_with("Goal (running, 0 of 8 passes)"));
}

#[test]
fn a_check_the_pass_did_not_run_is_unmet_but_never_undoes_the_pass() {
    let mut goal = goal();
    let mut now = measured(7.0, vec![], false, "b");
    now.checks[0].ran = false;
    let decision = decide(&goal, &now, &PassReport::default());
    assert!(!decision.undo);
    assert!(decision
        .unmet
        .iter()
        .any(|u| u.detail.contains("not run after the last edit")));
    record(
        &mut goal,
        &decision,
        now,
        &PassReport::default(),
        "t".into(),
    );
    assert_eq!(goal.status, GoalStatus::Running);
}
