use super::*;

fn edit(path: &str) -> Value {
    json!({ "path": path, "old_string": "a", "new_string": "b" })
}

#[test]
fn healthy_attempt_is_left_alone() {
    // The busiest accepted build of the traces: 44 edits of one file.
    let dog = Watchdog::new(Limits::default());
    let verdicts: Vec<_> = (0..44)
        .map(|_| dog.observe("edit_file", &edit("src/a.ts"), false, &json!("ok")))
        .collect();
    let nudges: Vec<_> = verdicts
        .iter()
        .enumerate()
        .filter(|(_, v)| **v != Verdict::Fine)
        .collect();
    assert_eq!(nudges.len(), 1);
    assert_eq!(nudges[0].0, 24);
    assert!(dog.stopped().is_none());
}

#[test]
fn failed_edits_nudge_once_then_stop() {
    let dog = Watchdog::new(Limits::default());
    let mut nudges = 0;
    let mut stopped = None;
    for _ in 0..12 {
        match dog.observe(
            "edit_file",
            &edit("src/a.ts"),
            true,
            &json!({"error":"old_string was not found"}),
        ) {
            Verdict::Nudge(_) => nudges += 1,
            Verdict::Stop(reason) => stopped = Some(reason),
            Verdict::Fine => {}
        }
    }
    assert_eq!(nudges, 1);
    assert!(stopped.unwrap().contains("failed_edits reached 12"));
}

#[test]
fn rereading_one_file_stops_the_attempt() {
    let dog = Watchdog::new(Limits::default());
    let args = json!({ "path": "./src/useAgentTab.ts", "start_line": 1, "end_line": 20 });
    let verdicts: Vec<_> = (0..25)
        .map(|_| dog.observe("read_file", &args, false, &json!("text")))
        .collect();
    assert!(matches!(verdicts[11], Verdict::Nudge(_)));
    assert!(matches!(verdicts[24], Verdict::Stop(ref r) if r.contains("reads:src/useAgentTab.ts")));
}

#[test]
fn checks_that_get_better_are_progress() {
    let dog = Watchdog::new(Limits::default());
    for errors in [29, 28, 16, 14, 12, 11, 2] {
        let out = json!(format!("typecheck: FAILED\n  {errors} errors:\n  - a.ts:1"));
        assert_eq!(
            dog.observe("run_check", &json!({}), true, &out),
            Verdict::Fine
        );
    }
}

#[test]
fn checks_stuck_at_the_same_errors_stop() {
    let dog = Watchdog::new(Limits::default());
    let out = json!("typecheck: FAILED\n  29 errors:");
    let verdicts: Vec<_> = (0..7)
        .map(|_| dog.observe("run_check", &json!({}), true, &out))
        .collect();
    assert!(matches!(verdicts[3], Verdict::Nudge(_)));
    assert!(matches!(verdicts[6], Verdict::Stop(_)));
    // A passing check would have started over.
    let fresh = Watchdog::new(Limits::default());
    for _ in 0..5 {
        fresh.observe("run_check", &json!({}), true, &out);
    }
    fresh.observe("run_check", &json!({}), false, &json!("passed"));
    assert_eq!(
        fresh.observe("run_check", &json!({}), true, &out),
        Verdict::Fine
    );
}

#[test]
fn after_a_stop_tools_are_refused_then_the_run_is_cancelled() {
    let dog = Watchdog::new(Limits {
        failed_edits: Threshold { nudge: 1, stop: 2 },
        ..Limits::default()
    });
    dog.observe("edit_file", &edit("a"), true, &json!("x"));
    assert!(matches!(
        dog.observe("edit_file", &edit("a"), true, &json!("x")),
        Verdict::Stop(_)
    ));
    for _ in 0..3 {
        assert!(dog.refuse().unwrap().contains("Do not call more tools"));
        assert!(!dog.hard_stop().is_cancelled());
    }
    dog.refuse();
    assert!(dog.hard_stop().is_cancelled());
}

#[test]
fn running_out_of_time_stops_at_the_next_call() {
    let dog = Watchdog::new(Limits {
        duration: Duration::ZERO,
        ..Limits::default()
    });
    assert!(dog.refuse().is_some());
    assert!(dog.stopped().unwrap().contains("minutes"));
}

#[test]
fn error_counts_are_read_from_check_output() {
    assert_eq!(
        error_count("FAILED (exit 2) in 6.9s\n  29 errors:\n  - x"),
        Some(29)
    );
    assert_eq!(error_count("found 1 error."), Some(1));
    assert_eq!(error_count("tests failed"), None);
}

#[test]
fn notes_are_added_to_every_kind_of_result() {
    assert_eq!(with_note(json!("a"), "n"), json!("a\n\n[workflow] n"));
    assert_eq!(
        with_note(json!({"x":1}), "n"),
        json!({"x":1,"workflow_note":"n"})
    );
    assert_eq!(
        with_note(json!([1]), "n"),
        json!({"result":[1],"workflow_note":"n"})
    );
}
