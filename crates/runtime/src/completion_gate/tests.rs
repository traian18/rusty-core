use rusty_protocol::commands::VerdictOutcome;
use serde_json::{json, Value};

use super::command_check::command_verdict;
use super::text::truncate;
use super::tool_check::with_answer;
use super::verdict::parse_verdict;

#[test]
fn a_tool_check_of_the_answer_gets_it_under_its_argument() {
    let args = json!({"mode": "strict"});
    assert_eq!(with_answer(args.clone(), None, "the list"), args);
    assert_eq!(
        with_answer(args, Some("tasks"), "the list"),
        json!({"mode": "strict", "tasks": "the list"})
    );
    assert_eq!(
        with_answer(Value::Null, Some("tasks"), "x"),
        json!({"tasks": "x"})
    );
}

#[test]
fn verdicts_parse_with_or_without_fences() {
    assert_eq!(
        parse_verdict(r#"{"passed": true, "feedback": ""}"#),
        Ok((true, String::new()))
    );
    assert_eq!(
        parse_verdict("```json\n{\"passed\": false, \"feedback\": \"add tests\"}\n```"),
        Ok((false, "add tests".into()))
    );
    assert!(parse_verdict("looks good to me").is_err());
    assert!(parse_verdict(r#"{"passed": "yes"}"#).is_err());
}

#[test]
fn command_exit_codes_follow_the_hook_contract() {
    assert_eq!(command_verdict(Some(0), "", ""), VerdictOutcome::Pass);
    assert_eq!(
        command_verdict(
            Some(0),
            r#"{"decision": "block", "reason": "tests fail"}"#,
            ""
        ),
        VerdictOutcome::Fail {
            feedback: "tests fail".into()
        }
    );
    assert_eq!(
        command_verdict(Some(0), r#"{"decision": "approve"}"#, ""),
        VerdictOutcome::Pass
    );
    assert_eq!(
        command_verdict(Some(2), "", "lint errors"),
        VerdictOutcome::Fail {
            feedback: "lint errors".into()
        }
    );
    assert!(matches!(
        command_verdict(Some(1), "", "boom"),
        VerdictOutcome::Error { .. }
    ));
    assert!(matches!(
        command_verdict(None, "", ""),
        VerdictOutcome::Error { .. }
    ));
}

#[test]
fn long_text_is_truncated_on_a_char_boundary() {
    assert_eq!(truncate("héllo", 10), "héllo");
    assert_eq!(truncate("héllo", 2), "hé… (truncated)");
}
