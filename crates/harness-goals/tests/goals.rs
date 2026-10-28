use harness_goals::{
    decide, pass_prompt, record, status_text, CheckOutcome, FileMeasure, FindingSummary, Goal,
    GoalStatus, GuidelineSummary, Measurement, PassReport, PlanStep, Severity, StepStatus, Targets,
    Verdict,
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

fn files(scores: &[(&str, f64)], fingerprint: &str) -> Measurement {
    Measurement {
        files: scores
            .iter()
            .map(|(path, score)| FileMeasure {
                path: (*path).into(),
                score: Some(*score),
                findings: vec![finding(Severity::Medium)],
            })
            .collect(),
        guideline_errors: Vec::new(),
        guideline_warnings: 0,
        checks: Vec::new(),
        fingerprint: fingerprint.into(),
    }
}

fn health_goal(min: f64, baseline: Measurement) -> Goal {
    let mut goal = Goal::new("g2".into(), "Raise the repo's health".into(), "t0".into());
    goal.scope = vec!["crates".into()];
    goal.targets = Targets {
        min_file_health: Some(min),
        ..Targets::default()
    };
    goal.baseline = Some(baseline);
    goal
}

#[test]
fn a_scope_with_nothing_measured_never_meets_a_health_target() {
    let empty = files(&[], "e");
    let mut goal = health_goal(8.5, empty.clone());
    let decision = decide(&goal, &empty, &PassReport::default());
    assert_ne!(decision.verdict, Verdict::Done);
    assert!(decision.unmet.iter().any(|u| u.target.contains("measured")));
    goal.targets.min_file_health = None;
    goal.targets.min_mean_health = Some(8.5);
    assert_ne!(
        decide(&goal, &empty, &PassReport::default()).verdict,
        Verdict::Done
    );
}

#[test]
fn the_mean_target_is_the_scope_s_general_health() {
    let mut goal = health_goal(6.0, files(&[("a.rs", 9.0), ("b.rs", 7.0)], "a"));
    goal.targets.min_mean_health = Some(8.5);
    let unmet = goal.baseline.as_ref().unwrap().unmet(&goal.targets);
    assert_eq!(unmet.len(), 1, "{unmet:?}");
    assert!(unmet[0].detail.contains("8"));
    let now = files(&[("a.rs", 9.0), ("b.rs", 8.5)], "b");
    assert_eq!(pass(&mut goal, now, PassReport::default()), Verdict::Done);
}

#[test]
fn fixing_one_file_of_many_is_progress() {
    let many: Vec<(String, f64)> = (0..200).map(|i| (format!("f{i:03}.rs"), 3.0)).collect();
    let scores = |fixed: f64| {
        let mut list: Vec<(&str, f64)> = many.iter().map(|(p, s)| (p.as_str(), *s)).collect();
        list[0].1 = fixed;
        list
    };
    let mut goal = health_goal(8.5, files(&scores(3.0), "a"));
    // Better but still below the target: the shortfall says so, the mean hardly moves.
    assert_eq!(
        pass(&mut goal, files(&scores(5.0), "b"), PassReport::default()),
        Verdict::Continue
    );
    assert_eq!(goal.stalled, 0);
}

#[test]
fn a_pass_is_told_to_plan_a_share_of_the_work_list_and_hand_over() {
    let scores: Vec<(String, f64)> = (0..20)
        .map(|i| (format!("f{i:02}.rs"), 2.0 + i as f64 * 0.4))
        .collect();
    let list: Vec<(&str, f64)> = scores.iter().map(|(p, s)| (p.as_str(), *s)).collect();
    let mut goal = health_goal(8.5, files(&list, "a"));
    let prompt = pass_prompt(&goal, goal.baseline.as_ref().unwrap());
    for expected in [
        "Work list: 17 file(s) miss a target (3 already meet them)",
        "update_todos",
        "at least 3 from the top of the work list (17 left, 8 pass(es) left",
        "- `f00.rs`: health 2",
        "also missing a target: `f15.rs`",
        "Next: <",
    ] {
        assert!(prompt.contains(expected), "missing {expected}:\n{prompt}");
    }
    assert!(
        !prompt.contains("`f19.rs`"),
        "files meeting the target are not work:\n{prompt}"
    );
    let report = PassReport {
        agent_status: Some("done".into()),
        next: Some("split task_queue.rs".into()),
        ..PassReport::default()
    };
    pass(&mut goal, files(&list, "b"), report);
    let prompt = pass_prompt(&goal, goal.last.as_ref().unwrap());
    assert!(
        prompt.contains("The last pass left this for you: split task_queue.rs"),
        "{prompt}"
    );
}

fn step(content: &str, status: StepStatus) -> PlanStep {
    PlanStep {
        content: content.into(),
        status,
    }
}

fn judged(yes: bool, plan: Vec<PlanStep>) -> PassReport {
    PassReport {
        agent_status: Some("done".into()),
        done_when_met: Some(yes),
        plan,
        ..PassReport::default()
    }
}

#[test]
fn a_task_goal_is_planned_first_and_done_only_when_the_plan_is_closed_and_judged_done() {
    let mut goal = Goal::new(
        "g3".into(),
        "Add CSV export to the report page".into(),
        "t0".into(),
    );
    goal.baseline = Some(files(&[], "e"));
    assert!(goal.is_task());
    let prompt = pass_prompt(&goal, goal.baseline.as_ref().unwrap());
    for expected in [
        "Plan the whole goal first",
        "No plan yet",
        "update_todos",
        "Done when: yes",
        "whether the goal is fully done",
    ] {
        assert!(prompt.contains(expected), "missing {expected}:\n{prompt}");
    }
    assert!(!prompt.contains("Work list"), "{prompt}");

    // The agent's word alone, without a plan, does not end it.
    assert_eq!(
        pass(&mut goal, files(&[], "e"), judged(true, vec![])),
        Verdict::Continue
    );

    // A plan with steps open is kept and counts as progress even if the files hash the same.
    let plan = vec![
        step("Add the export button in report.tsx", StepStatus::Completed),
        step("Write the CSV encoder and its test", StepStatus::Pending),
    ];
    assert_eq!(
        pass(&mut goal, files(&[], "e"), judged(true, plan)),
        Verdict::Continue
    );
    assert_eq!(goal.stalled, 0);
    assert_eq!(goal.plan.len(), 2);
    let prompt = pass_prompt(&goal, goal.last.as_ref().unwrap());
    for expected in [
        "The plan so far (1 of 2 closed)",
        "[x] Add the export button",
        "[ ] Write the CSV encoder",
        "Carry on with the plan above",
        "1 step(s) left: Write the CSV encoder",
    ] {
        assert!(prompt.contains(expected), "missing {expected}:\n{prompt}");
    }

    // Closed, but a check the pass ran fails: not done.
    let closed = vec![
        step("Add the export button in report.tsx", StepStatus::Completed),
        step("Write the CSV encoder and its test", StepStatus::Completed),
    ];
    let mut failing = files(&[], "f");
    failing.checks.push(CheckOutcome {
        name: "test".into(),
        passed: false,
        ran: true,
        summary: "1 failed".into(),
    });
    let decision = decide(&goal, &failing, &judged(true, closed.clone()));
    assert_ne!(decision.verdict, Verdict::Done);
    assert!(
        decision.unmet.iter().any(|u| u.target == "test passes"),
        "{:?}",
        decision.unmet
    );

    // Closed and judged not done: goes on; closed, checks pass and judged done: done.
    assert_eq!(
        decide(&goal, &files(&[], "g"), &judged(false, closed.clone())).verdict,
        Verdict::Continue
    );
    assert_eq!(
        pass(&mut goal, files(&[], "g"), judged(true, closed)),
        Verdict::Done
    );
    assert_eq!(goal.status, GoalStatus::Done);
}

#[test]
fn a_task_pass_whose_new_code_scores_lower_is_kept() {
    let mut goal = Goal::new("g4".into(), "Add an importer".into(), "t0".into());
    goal.baseline = Some(files(&[("src/a.rs", 9.0)], "a"));
    let plan = vec![
        step("Write the importer", StepStatus::Completed),
        step("Wire it in", StepStatus::Pending),
    ];
    let verdict = pass(
        &mut goal,
        files(&[("src/a.rs", 6.0), ("src/importer.rs", 5.0)], "b"),
        judged(false, plan),
    );
    assert_eq!(verdict, Verdict::Continue);
    assert!(goal
        .journal
        .last()
        .is_some_and(|e| e.verdict != Verdict::Undone));
}

#[test]
fn a_scope_in_words_is_given_to_the_pass_to_work_out() {
    let mut goal = Goal::new("g5".into(), "Make it fast".into(), "t0".into());
    goal.scope_note = Some("the task queue and whatever schedules it".into());
    goal.baseline = Some(files(&[], "e"));
    let prompt = pass_prompt(&goal, goal.baseline.as_ref().unwrap());
    assert!(
        prompt.contains("Scope: the task queue and whatever schedules it\nWork out from this"),
        "{prompt}"
    );
    assert!(
        prompt.contains("Measured on: the files you change."),
        "{prompt}"
    );
}
