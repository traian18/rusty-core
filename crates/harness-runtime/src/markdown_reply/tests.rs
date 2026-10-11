use super::*;
use serde_json::json;

fn plan_schema() -> Value {
    json!({"type":"object","required":["status","requirements","tasks","summary"],"properties":{
            "status":{"type":"string","enum":["ready","blocked","incomplete"]},
            "requirements":{"type":"array","items":{"type":"object","required":["id","text","criteria"],"properties":{
                "id":{"type":"string"},"text":{"type":"string"},
                "criteria":{"type":"array","items":{"type":"object","required":["id","text"],"properties":{"id":{"type":"string"},"text":{"type":"string"}}}}}}},
            "tasks":{"type":"array","items":{"type":"object","required":["id","requirement_ids","criterion_ids","depends_on","instructions"],"properties":{
                "id":{"type":"string"},
                "requirement_ids":{"type":"array","items":{"type":"string"}},
                "criterion_ids":{"type":"array","items":{"type":"string"}},
                "depends_on":{"type":"array","items":{"type":"string"}},
                "instructions":{"type":"string"}}}},
            "summary":{"type":"string"}}})
}

#[test]
fn a_plan_is_read_from_markdown() {
    let reply = "## status\nReady.\n\n## requirements\n### R1\nShow the badge\n#### criteria\n- C1: badge visible\n- C2: badge hidden when empty\n\n## tasks\n### T1\nrequirement_ids: R1\ncriterion_ids: C1, C2\ndepends_on:\nEdit `badge.ts`.\nThen test it.\n\n## summary\nOne task.";
    let value = parse(&plan_schema(), reply).unwrap();
    assert_eq!(
        value,
        json!({
            "status":"ready",
            "requirements":[{"id":"R1","text":"Show the badge","criteria":[
                {"id":"C1","text":"badge visible"},{"id":"C2","text":"badge hidden when empty"}]}],
            "tasks":[{"id":"T1","requirement_ids":["R1"],"criterion_ids":["C1","C2"],"depends_on":[],
                "instructions":"Edit `badge.ts`.\nThen test it."}],
            "summary":"One task."
        })
    );
}

#[test]
fn verdicts_and_criteria_are_read_from_markdown() {
    let schema = json!({"type":"object","required":["verdict","criteria","summary"],"properties":{
            "verdict":{"type":"string"},
            "criteria":{"type":"array","items":{"type":"object","required":["id","status","evidence"],"properties":{
                "id":{"type":"string"},"status":{"type":"string","enum":["pass","fail","unverified"]},"evidence":{"type":"string"}}}},
            "summary":{"type":"string"}}});
    let reply = "# Verdict\nfail: src/a.rs:3 misses the case\n# Criteria\n## C1\nstatus: **Fail**\nsrc/a.rs:3 has no branch.\n# Summary\nNot done.";
    let value = parse(&schema, reply).unwrap();
    assert_eq!(value["verdict"], "fail: src/a.rs:3 misses the case");
    assert_eq!(
        value["criteria"],
        json!([{"id":"C1","status":"fail","evidence":"src/a.rs:3 has no branch."}])
    );
}

#[test]
fn inputs_are_rendered_without_json_escaping() {
    let text = render_input(&json!({
        "request": "do \"it\"",
        "research": "line one\nline two",
        "plan": {"status":"ready","tasks":[{"id":"T1","depends_on":[],"instructions":"Edit a.rs\nthen b.rs"}]}
    }));
    assert!(text.contains("## research\nline one\nline two"));
    assert!(text.contains("do \"it\""));
    assert!(text.contains("### T1\ndepends_on: \n"));
    assert!(!text.contains("#### criteria") || text.contains("- C1:"));
    assert!(text.contains("#### instructions\nEdit a.rs\nthen b.rs"));
    assert!(!text.contains("\\n") && !text.contains('{'));
}

#[test]
fn ids_are_optional_and_numbered_by_the_host() {
    let value = parse(&plan_schema(), "## requirements\n### Show the badge\n#### criteria\n- badge visible\n- C7: hidden when empty\n### Hide it\nbody\n## tasks\n### Edit badge.ts\nEdit it.").unwrap();
    assert_eq!(value["requirements"][0]["id"], "R1");
    assert_eq!(value["requirements"][0]["text"], "Show the badge");
    assert_eq!(value["requirements"][0]["criteria"][0]["id"], "C1");
    assert_eq!(value["requirements"][0]["criteria"][1]["id"], "C7");
    assert_eq!(value["requirements"][1]["id"], "R2");
    assert_eq!(value["tasks"][0]["id"], "T1");
    assert_eq!(value["tasks"][0]["instructions"], "Edit it.");
}

#[test]
fn an_unclear_check_result_is_never_a_pass() {
    let schema = json!({"type":"object","required":["verdict","criteria"],"properties":{
            "verdict":{"type":"string"},
            "criteria":{"type":"array","items":{"type":"object","required":["id","status","evidence"],"properties":{
                "id":{"type":"string"},"status":{"type":"string","enum":["pass","fail","unverified"]},"evidence":{"type":"string"}}}}}});
    let value = parse(
        &schema,
        "## criteria\n### C1\nstatus: FAILED\nbroken\n### C2\nstatus: looks ok\nfine",
    )
    .unwrap();
    assert_eq!(value["criteria"][0]["status"], "fail");
    assert_eq!(value["criteria"][1]["status"], "unverified");
    assert!(value["verdict"].as_str().unwrap().starts_with("fail"));
    let value = parse(&schema, "## criteria\n### C1\nstatus: Passed\nsrc/a.rs:1").unwrap();
    assert_eq!(value["verdict"], "pass");
}

#[test]
fn words_in_the_heading_count_as_the_item_text() {
    let schema = json!({"type":"object","required":["requirements"],"properties":{
            "requirements":{"type":"array","items":{"type":"object","required":["id","text"],"properties":{
                "id":{"type":"string"},"text":{"type":"string","minLength":1}}}}}});
    let reply = "## requirements\n### R1: Show the badge\n### Requirement R2 - Hide it\n### R3\n";
    let value = parse(&schema, reply).unwrap();
    assert_eq!(
        value["requirements"],
        json!([{"id":"R1","text":"Show the badge"},{"id":"R2","text":"Hide it"},{"id":"R3","text":"R3"}])
    );
}

#[test]
fn a_reply_is_forgiven_a_missing_status_and_bare_section_names() {
    let schema = json!({"type":"object","required":["status","summary"],"properties":{
            "status":{"type":"string","enum":["complete","blocked"]},"summary":{"type":"string"}}});
    // No status section at all: the summary is kept, the status inferred.
    let value = parse(&schema, "## summary\nAudited everything.").unwrap();
    assert_eq!(
        value,
        json!({"status":"complete","summary":"Audited everything."})
    );
    // Section names without any `#`.
    let value = parse(&schema, "status\nblocked\nsummary\nNo access.").unwrap();
    assert_eq!(value, json!({"status":"blocked","summary":"No access."}));
    // Plain prose becomes the summary.
    let value = parse(&schema, "I looked at it and all is fine.").unwrap();
    assert_eq!(value["summary"], "I looked at it and all is fine.");
    assert_eq!(value["status"], "complete");
    // The status is found when only stated in passing.
    let value = parse(&schema, "## summary\nStopped.\n\nStatus: blocked").unwrap();
    assert_eq!(value["status"], "blocked");
}

#[test]
fn headings_inside_code_fences_are_text() {
    let schema = json!({"type":"object","properties":{"summary":{"type":"string"}}});
    let value = parse(&schema, "## summary\n```\n## status\n```").unwrap();
    assert_eq!(value["summary"], "```\n## status\n```");
}

#[test]
fn the_template_names_every_section() {
    let text = instructions(&plan_schema());
    for expected in [
        "## status",
        "## requirements",
        "### <short name>",
        "- <text>",
        "## tasks",
        "## summary",
    ] {
        assert!(text.contains(expected), "{expected} missing from {text}");
    }
    assert!(!text.contains("JSON Schema"));
}
