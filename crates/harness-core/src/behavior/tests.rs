use std::sync::Arc;

use harness_protocol::backend::{ExecutionParams, ReasoningEffort};
use harness_protocol::effects::SpawnAgentSpec;
use harness_protocol::tools::PermissionMode;
use serde_json::json;

use super::*;
use crate::orchestration::{DefinitionStatus, ToolScope};

fn profile(id: &str, revision: u64) -> BehaviorProfile {
    BehaviorProfile {
        id: ProfileId::from(id),
        revision,
        name: format!("{id} r{revision}"),
        description: None,
        ..default_profile_definition()
    }
}

fn compiled(profile: BehaviorProfile) -> Arc<CompiledProfile> {
    Arc::new(compile(profile).expect("valid profile"))
}

fn codes(error: &ProfileValidationError) -> Vec<&'static str> {
    error.issues.iter().map(|issue| issue.code).collect()
}

// ---------------------------------------------------------------------------
// Default profile
// ---------------------------------------------------------------------------

#[test]
fn default_profile_is_behavior_neutral() {
    let default = default_profile();
    assert_eq!(
        default.reference(),
        ProfileRef::exact(DEFAULT_PROFILE_ID, 1)
    );
    assert_eq!(default.system_prompt("base", Some("claude-x")), "base");
    assert_eq!(default.system_prompt("", None), "");
    assert!(default.allows_tool("anything"));
    for mode in [
        PermissionMode::Allow,
        PermissionMode::Ask,
        PermissionMode::Deny,
    ] {
        let expected = format!("{mode:?}");
        assert_eq!(format!("{:?}", default.permission("t", mode)), expected);
    }
    let params = ExecutionParams {
        model: Some("m".into()),
        temperature: Some(0.7),
        ..Default::default()
    };
    assert_eq!(
        serde_json::to_value(default.execution_params(&params)).unwrap(),
        serde_json::to_value(&params).unwrap()
    );
    assert_eq!(default.tool_description("t", "desc"), "desc");
    assert!(Arc::ptr_eq(&default, &default_profile()), "shared instance");
}

// ---------------------------------------------------------------------------
// JSON contract
// ---------------------------------------------------------------------------

fn full_profile_json() -> serde_json::Value {
    json!({
        "schema_version": 1,
        "id": "careful-coder",
        "revision": 3,
        "name": "Careful coder",
        "status": "published",
        "instructions": {
            "mode": "append",
            "text": "Smallest correct change.",
            "variants": { "gpt": "Be terse." }
        },
        "tools": { "type": "allow_list", "tools": ["fs.read", "fs.edit"] },
        "tool_overrides": {
            "fs.edit": { "permission": "ask", "description_append": "Read first." }
        },
        "execution": { "temperature": 0.2, "reasoning_effort": "medium" },
        "limits": { "max_turns": 40, "max_tool_calls": 120, "final_turn_prompt": "Wrap up." },
        "children": { "type": "inherit" },
        "metadata": { "editor": { "position": { "x": 420, "y": 180 }, "color": "agent" } }
    })
}

#[test]
fn profiles_round_trip_through_json_and_keep_editor_metadata() {
    let profile: BehaviorProfile = serde_json::from_value(full_profile_json()).unwrap();
    let compiled = compile(profile.clone()).expect("valid");
    assert_eq!(
        serde_json::to_value(&profile).unwrap()["metadata"],
        full_profile_json()["metadata"]
    );
    let again: BehaviorProfile =
        serde_json::from_value(serde_json::to_value(&profile).unwrap()).unwrap();
    assert_eq!(compile(again).unwrap().content_hash, compiled.content_hash);
}

#[test]
fn unknown_fields_are_rejected_outside_metadata() {
    let mut document = full_profile_json();
    document["tolls"] = json!([]);
    assert!(serde_json::from_value::<BehaviorProfile>(document).is_err());

    let mut document = full_profile_json();
    document["limits"]["max_turn"] = json!(3);
    assert!(serde_json::from_value::<BehaviorProfile>(document).is_err());

    let mut document = full_profile_json();
    document["children"] = json!({ "type": "clone" });
    assert!(serde_json::from_value::<BehaviorProfile>(document).is_err());
}

#[test]
fn omitted_fields_take_neutral_defaults() {
    let profile: BehaviorProfile = serde_json::from_value(json!({
        "schema_version": 1, "id": "minimal", "revision": 1, "name": "Minimal"
    }))
    .unwrap();
    assert_eq!(profile.tools, ToolScope::Inherit);
    assert_eq!(profile.status, DefinitionStatus::Published);
    assert_eq!(profile.children, ChildPolicy::Inherit);
    compile(profile).expect("minimal profile is valid");
}

#[test]
fn published_json_schema_is_current() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../schema/behavior-profile-v1.schema.json"
    );
    let generated = serde_json::to_string_pretty(&profile_json_schema()).unwrap() + "\n";
    if std::env::var_os("RUSTY_UPDATE_SCHEMAS").is_some() {
        std::fs::write(path, &generated).expect("write schema");
        return;
    }
    let published = std::fs::read_to_string(path).unwrap_or_default();
    assert!(
        published == generated,
        "schema/behavior-profile-v1.schema.json is out of date; \
         regenerate with RUSTY_UPDATE_SCHEMAS=1 cargo test -p harness-core published_json_schema"
    );
}

// ---------------------------------------------------------------------------
// Compiler
// ---------------------------------------------------------------------------

#[test]
fn compiler_reports_every_issue_at_once() {
    let mut profile = profile("bad", 0);
    profile.schema_version = 9;
    profile.name = " ".into();
    profile.tools = ToolScope::AllowList(vec!["a".into(), "a".into(), "".into()]);
    profile.execution.temperature = Some(3.0);
    profile.limits.max_turns = Some(0);
    profile.limits.final_turn_prompt = Some("".into());
    profile
        .instructions
        .variants
        .insert("GPT".into(), "x".into());
    profile.rules = vec![serde_json::from_value(json!({
        "id": "", "on": "PostToolUse", "do": { "deny": { "reason": "" } }
    }))
    .unwrap()];
    profile.completion_gate = Some(CompletionGate {
        checks: Vec::new(),
        max_continuations: 1,
        on_exhausted: None,
    });
    profile.children = ChildPolicy::Named {
        profile: ProfileRef::latest("other"),
    };

    let codes = codes(&compile(profile).unwrap_err());
    for expected in [
        "unsupported_schema_version",
        "invalid_revision",
        "empty_name",
        "duplicate_tool",
        "invalid_tool_reference",
        "invalid_temperature",
        "invalid_limit",
        "empty_final_turn_prompt",
        "invalid_variant_key",
        "empty_id",
        "empty_text",
        "action_not_available",
        "empty_gate",
        "unsupported_child_policy",
    ] {
        assert!(codes.contains(&expected), "missing {expected} in {codes:?}");
    }
}

#[test]
fn replace_mode_requires_text() {
    let mut profile = profile("replace", 1);
    profile.instructions.mode = InstructionMode::Replace;
    assert_eq!(
        codes(&compile(profile).unwrap_err()),
        vec!["empty_replacement"]
    );
}

// ---------------------------------------------------------------------------
// Profile semantics
// ---------------------------------------------------------------------------

#[test]
fn instructions_append_replace_and_pick_model_variants() {
    let mut append = profile("p", 1);
    append.instructions.text = "Profile text.".into();
    append
        .instructions
        .variants
        .insert("gpt".into(), "GPT text.".into());
    let append = compiled(append);
    assert_eq!(
        append.system_prompt("Base.", None),
        "Base.\n\nProfile text."
    );
    assert_eq!(append.system_prompt("", None), "Profile text.");
    assert_eq!(
        append.system_prompt("Base.", Some("GPT-6-sol")),
        "Base.\n\nGPT text.",
        "variant keys match case-insensitively as substrings"
    );
    assert_eq!(
        append.system_prompt("Base.", Some("claude-opus")),
        "Base.\n\nProfile text."
    );

    let mut replace = profile("p", 2);
    replace.instructions.mode = InstructionMode::Replace;
    replace.instructions.text = "Only this.".into();
    assert_eq!(compiled(replace).system_prompt("Base.", None), "Only this.");
}

#[test]
fn tool_scope_and_overrides_only_ever_narrow() {
    let mut narrow = profile("p", 1);
    narrow.tools = ToolScope::AllowList(vec!["fs.read".into(), "fs.edit".into()]);
    narrow.tool_overrides.insert(
        "fs.edit".into(),
        ToolOverride {
            permission: Some(ToolPermission::Ask),
            description_append: Some("Read first.".into()),
        },
    );
    narrow.tool_overrides.insert(
        "fs.read".into(),
        ToolOverride {
            permission: Some(ToolPermission::Allow),
            description_append: None,
        },
    );
    let narrow = compiled(narrow);

    assert!(narrow.allows_tool("fs.read"));
    assert!(!narrow.allows_tool("shell.exec"));
    assert!(matches!(
        narrow.permission("fs.edit", PermissionMode::Allow),
        PermissionMode::Ask
    ));
    assert!(
        matches!(
            narrow.permission("fs.read", PermissionMode::Ask),
            PermissionMode::Ask
        ),
        "an `allow` override cannot loosen the session's `ask`"
    );
    assert!(matches!(
        narrow.permission("fs.edit", PermissionMode::Deny),
        PermissionMode::Deny
    ));
    assert_eq!(
        narrow.tool_description("fs.edit", "Edit."),
        "Edit.\n\nRead first."
    );

    let mut none = profile("p", 2);
    none.tools = ToolScope::None;
    assert!(!compiled(none).allows_tool("fs.read"));
}

#[test]
fn edit_file_follows_the_write_file_scope_and_permission_of_a_profile() {
    let mut scoped = profile("p", 1);
    scoped.tools = ToolScope::AllowList(vec!["read_file".into(), "write_file".into()]);
    scoped.tool_overrides.insert(
        "write_file".into(),
        ToolOverride {
            permission: Some(ToolPermission::Ask),
            description_append: Some("Write complete content.".into()),
        },
    );
    let scoped = compiled(scoped);
    assert!(
        scoped.allows_tool("edit_file"),
        "an allow-list naming write_file admits edit_file"
    );
    assert!(
        matches!(
            scoped.permission("edit_file", PermissionMode::Allow),
            PermissionMode::Ask
        ),
        "write_file's permission override binds edit_file"
    );
    assert_eq!(
        scoped.tool_description("edit_file", "Edit."),
        "Edit.",
        "descriptions stay per tool: write_file's note is about whole-file content"
    );

    let mut read_only = profile("p", 2);
    read_only.tools = ToolScope::AllowList(vec!["read_file".into()]);
    assert!(
        !compiled(read_only).allows_tool("edit_file"),
        "no write grant, no edit_file"
    );

    let mut alias_only = profile("p", 3);
    alias_only.tools = ToolScope::AllowList(vec!["edit_file".into()]);
    let alias_only = compiled(alias_only);
    assert!(alias_only.allows_tool("edit_file"));
    assert!(
        !alias_only.allows_tool("write_file"),
        "an alias alone does not grant the tool it follows"
    );

    let mut loosened = profile("p", 4);
    loosened.tool_overrides.insert(
        "write_file".into(),
        ToolOverride {
            permission: Some(ToolPermission::Deny),
            description_append: None,
        },
    );
    loosened.tool_overrides.insert(
        "edit_file".into(),
        ToolOverride {
            permission: Some(ToolPermission::Allow),
            description_append: None,
        },
    );
    assert!(
        matches!(
            compiled(loosened).permission("edit_file", PermissionMode::Allow),
            PermissionMode::Deny
        ),
        "naming the alias cannot loosen what the tool it follows denies"
    );
}

#[test]
fn project_info_is_admitted_by_a_read_file_scope_and_permission() {
    let mut analyze = profile("p", 1);
    analyze.tools = ToolScope::AllowList(vec![
        "read_file".into(),
        "list_files".into(),
        "search_codebase".into(),
    ]);
    analyze.tool_overrides.insert(
        "read_file".into(),
        ToolOverride {
            permission: Some(ToolPermission::Ask),
            description_append: Some("Read narrowly.".into()),
        },
    );
    let analyze = compiled(analyze);
    assert!(
        analyze.allows_tool("project_info"),
        "a read-only profile's allow-list names read_file, which admits project_info"
    );
    assert!(
        !analyze.allows_tool("edit_file"),
        "reading does not admit editing"
    );
    assert!(matches!(
        analyze.permission("project_info", PermissionMode::Allow),
        PermissionMode::Ask
    ));
    assert_eq!(
        analyze.tool_description("project_info", "Detect."),
        "Detect.",
        "descriptions stay per tool"
    );

    let mut no_reading = profile("p", 2);
    no_reading.tools = ToolScope::AllowList(vec!["list_files".into()]);
    assert!(!compiled(no_reading).allows_tool("project_info"));
}

#[test]
fn project_info_does_not_count_as_reading_for_rules() {
    let mut state = state_with(json!([
        { "id": "no-read-yet", "on": "BeforeModelRequest",
          "when": { "calls": { "tool": "read_file", "eq": 0 } },
          "do": { "inject": { "text": "read something" } } },
        { "id": "no-reading", "on": "PreToolUse", "when": { "tool": ["read_file"] },
          "do": { "deny": { "reason": "no reading" } } }
    ]));
    let fired = |state: &mut BehaviorState| -> Vec<String> {
        state.begin_turn();
        state
            .evaluate(RuleEvent::BeforeModelRequest, None)
            .fired
            .into_iter()
            .map(|fired| fired.rule_id)
            .collect()
    };

    assert_eq!(fired(&mut state), ["no-read-yet"]);
    state.note_executed("project_info");
    assert_eq!(
        fired(&mut state),
        ["no-read-yet"],
        "a gate that wants evidence of reading is not satisfied by project_info"
    );
    state.note_executed("read_file");
    assert!(fired(&mut state).is_empty());

    assert!(matches!(
        pre(&mut state, &call("read_file", json!({}))).decision,
        Some(ToolDecision::Deny { .. })
    ));
    assert_eq!(
        pre(&mut state, &call("project_info", json!({}))).decision,
        None,
        "a deny written for read_file does not reach project_info"
    );
}

#[test]
fn run_check_is_a_run_command_for_scopes_permissions_rules_and_gates() {
    let mut docs = profile("p", 1);
    docs.tools = ToolScope::AllowList(vec!["read_file".into(), "run_command".into()]);
    docs.tool_overrides.insert(
        "run_command".into(),
        ToolOverride {
            permission: Some(ToolPermission::Ask),
            description_append: None,
        },
    );
    let docs = compiled(docs);
    assert!(
        docs.allows_tool("run_check"),
        "an allow-list naming run_command admits run_check"
    );
    assert!(matches!(
        docs.permission("run_check", PermissionMode::Allow),
        PermissionMode::Ask
    ));

    let mut no_commands = profile("p", 2);
    no_commands.tools = ToolScope::AllowList(vec!["read_file".into()]);
    assert!(!compiled(no_commands).allows_tool("run_check"));

    let mut state = state_with(json!([
        { "id": "no-commands", "on": "PreToolUse", "when": { "tool": ["run_command"] },
          "do": { "deny": { "reason": "Documentation work does not run commands." } } },
        { "id": "no-evidence", "on": "BeforeModelRequest",
          "when": { "calls": { "tool": ["read_file", "search_codebase", "run_command"], "eq": 0 } },
          "do": { "inject": { "text": "inspect something first" } } }
    ]));
    assert!(
        matches!(
            pre(&mut state, &call("run_check", json!({}))).decision,
            Some(ToolDecision::Deny { .. })
        ),
        "a profile that forbids running commands forbids running checks"
    );
    let fired = |state: &mut BehaviorState| -> Vec<String> {
        state.begin_turn();
        state
            .evaluate(RuleEvent::BeforeModelRequest, None)
            .fired
            .into_iter()
            .map(|fired| fired.rule_id)
            .collect()
    };
    assert_eq!(fired(&mut state), ["no-evidence"]);
    state.note_executed("run_check");
    assert!(
        fired(&mut state).is_empty(),
        "a gate that wants a command to have run is met by run_check"
    );
}

/// Evaluates `BeforeModelRequest` rules on a new turn and returns the ids that fired.
fn fired_on_new_turn(state: &mut BehaviorState) -> Vec<String> {
    state.begin_turn();
    state
        .evaluate(RuleEvent::BeforeModelRequest, None)
        .fired
        .into_iter()
        .map(|fired| fired.rule_id)
        .collect()
}

#[test]
fn outcome_narrows_a_call_count_to_calls_that_succeeded_or_failed() {
    let mut state = state_with(json!([
        { "id": "failed", "on": "BeforeModelRequest",
          "when": { "calls": { "tool": "run_check", "outcome": "failed", "gte": 1 } },
          "do": { "inject": { "text": "a check failed" } } },
        { "id": "passed", "on": "BeforeModelRequest",
          "when": { "calls": { "tool": "run_check", "outcome": "succeeded", "gte": 1 } },
          "do": { "inject": { "text": "a check passed" } } },
        { "id": "ran", "on": "BeforeModelRequest",
          "when": { "calls": { "tool": "run_check", "gte": 1 } },
          "do": { "inject": { "text": "a check ran" } } }
    ]));
    assert!(fired_on_new_turn(&mut state).is_empty());
    state.note_executed_with("run_check", true);
    assert_eq!(fired_on_new_turn(&mut state), ["failed", "ran"]);
    state.note_executed_with("run_check", false);
    assert_eq!(fired_on_new_turn(&mut state), ["failed", "passed", "ran"]);
}

#[test]
fn since_last_call_with_an_outcome_means_a_passing_check_since_the_last_edit() {
    let mut state = state_with(json!([
        { "id": "verified", "on": "BeforeModelRequest",
          "when": { "since_last_call": { "of": "write_file", "called": "run_check", "outcome": "succeeded", "gte": 1 } },
          "do": { "inject": { "text": "verified" } } }
    ]));
    state.note_executed("write_file");
    state.note_executed_with("run_check", true);
    assert!(
        fired_on_new_turn(&mut state).is_empty(),
        "a check that failed after the edit is not a passing one"
    );
    state.note_executed_with("run_check", false);
    assert_eq!(fired_on_new_turn(&mut state), ["verified"]);

    state.note_executed("edit_file");
    assert!(
        fired_on_new_turn(&mut state).is_empty(),
        "edit_file is a write: a new edit needs a new passing check"
    );
    state.note_executed_with("run_check", false);
    assert_eq!(fired_on_new_turn(&mut state), ["verified"]);

    state.note_executed_with("write_file", true);
    assert!(
        fired_on_new_turn(&mut state).is_empty(),
        "`of` matches a call whatever its outcome"
    );
}

#[test]
fn turns_since_call_with_an_outcome_ignores_calls_with_the_other_outcome() {
    let mut state = state_with(json!([
        { "id": "stale-pass", "on": "BeforeModelRequest",
          "when": { "turns_since_call": { "tool": "run_check", "outcome": "succeeded", "gte": 2 } },
          "do": { "inject": { "text": "re-run the check" } } }
    ]));
    assert!(fired_on_new_turn(&mut state).is_empty(), "turn 1");
    assert_eq!(
        fired_on_new_turn(&mut state),
        ["stale-pass"],
        "turn 2: never passed"
    );
    state.note_executed_with("run_check", false);
    assert!(
        fired_on_new_turn(&mut state).is_empty(),
        "turn 3: passed on turn 2"
    );
    state.note_executed_with("run_check", true);
    assert_eq!(
        fired_on_new_turn(&mut state),
        ["stale-pass"],
        "turn 4: a failure on turn 3 does not refresh a pass"
    );
}

#[test]
fn tool_offered_reads_the_latest_offering_and_follows_aliases() {
    let mut state = state_with(json!([
        { "id": "can-run-commands", "on": "BeforeModelRequest",
          "when": { "tool_offered": ["run_command"] },
          "do": { "inject": { "text": "you can run commands" } } },
        { "id": "cannot-edit", "on": "BeforeModelRequest",
          "when": { "not": { "tool_offered": "write_file" } },
          "do": { "inject": { "text": "you cannot edit" } } }
    ]));
    assert_eq!(
        fired_on_new_turn(&mut state),
        ["cannot-edit"],
        "nothing has been offered yet"
    );
    state.note_offered(vec!["read_file".into(), "run_check".into()]);
    assert_eq!(
        fired_on_new_turn(&mut state),
        ["can-run-commands", "cannot-edit"],
        "run_check is a run_command, so a rule about run_command sees it"
    );
    state.note_offered(vec!["edit_file".into(), "read_file".into()]);
    assert!(
        fired_on_new_turn(&mut state).is_empty(),
        "edit_file is a write_file, and run_check is gone"
    );
    state.note_offered(vec!["z".into(), "a".into(), "z".into()]);
    assert_eq!(state.run.offered_tools, ["a", "z"], "sorted, one of each");
}

#[test]
fn saved_state_without_outcomes_still_loads_and_unused_fields_add_nothing() {
    let old: ExecutedCall =
        serde_json::from_value(json!({ "turn": 2, "tool": "fs.edit" })).unwrap();
    assert!(!old.failed, "older state counts as succeeded");
    assert_eq!(
        serde_json::to_value(ExecutedCall {
            turn: 1,
            tool: "run_check".into(),
            failed: false
        })
        .unwrap(),
        json!({ "turn": 1, "tool": "run_check" })
    );
    assert_eq!(
        serde_json::to_value(ExecutedCall {
            turn: 1,
            tool: "run_check".into(),
            failed: true
        })
        .unwrap(),
        json!({ "turn": 1, "tool": "run_check", "failed": true })
    );
    let counters = serde_json::to_value(RunCounters::default()).unwrap();
    assert!(counters.get("offered_tools").is_none());
}

#[test]
fn outcome_and_tool_offered_are_valid_in_rules_and_gates_and_round_trip() {
    let compiled = with_gate(json!({
        "checks": [{
            "id": "checked",
            "require": { "any": [
                { "calls": { "tool": "write_file", "eq": 0 } },
                { "not": { "tool_offered": "run_check" } },
                { "since_last_call": { "of": "write_file", "called": "run_check", "outcome": "succeeded", "gte": 1 } }
            ] },
            "feedback": "Run run_check."
        }]
    }))
    .expect("valid gate");
    let json = serde_json::to_value(&compiled.profile).unwrap();
    let again: BehaviorProfile = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(again, compiled.profile);
    assert!(json.to_string().contains("\"outcome\":\"succeeded\""));

    // `any` is the default and is left out of the saved document.
    let plain = with_rules(json!([
        { "id": "r", "on": "BeforeModelRequest",
          "when": { "calls": { "tool": "run_check", "outcome": "any", "gte": 1 } },
          "do": { "inject": { "text": "x" } } }
    ]))
    .unwrap();
    assert!(!serde_json::to_string(&plain.profile)
        .unwrap()
        .contains("outcome"));

    // A name that does not exist is rejected, and so is an empty tool pattern.
    let mut document = serde_json::to_value(profile("ruled", 1)).unwrap();
    document["rules"] = json!([{ "id": "r", "on": "BeforeModelRequest",
        "when": { "calls": { "tool": "run_check", "outcome": "sometimes", "gte": 1 } },
        "do": { "inject": { "text": "x" } } }]);
    assert!(serde_json::from_value::<BehaviorProfile>(document).is_err());
    assert!(with_rules(json!([
        { "id": "r", "on": "BeforeModelRequest",
          "when": { "tool_offered": "" }, "do": { "inject": { "text": "x" } } }
    ]))
    .is_err());
}

#[test]
fn execution_overlay_only_sets_what_it_names() {
    let mut profile = profile("p", 1);
    profile.execution = ExecutionOverlay {
        model: Some("small-model".into()),
        max_tokens: None,
        temperature: Some(0.1),
        reasoning_effort: Some(ReasoningEffortSetting::High),
    };
    let params = compiled(profile).execution_params(&ExecutionParams {
        model: Some("big-model".into()),
        max_tokens: Some(4096),
        ..Default::default()
    });
    assert_eq!(params.model.as_deref(), Some("small-model"));
    assert_eq!(params.max_tokens, Some(4096));
    assert_eq!(params.temperature, Some(0.1));
    assert_eq!(params.reasoning_effort, Some(ReasoningEffort::High));
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

#[test]
fn registry_holds_builtins_and_reserves_their_ids() {
    let mut registry = ProfileRegistry::new();
    assert_eq!(
        registry
            .resolve(&ProfileRef::latest(DEFAULT_PROFILE_ID))
            .unwrap()
            .content_hash,
        default_profile().content_hash
    );
    for source in [ProfileSource::Host, ProfileSource::Workspace] {
        assert!(matches!(
            registry.register(profile("rusty.default", 1), source),
            Err(ProfileRegistryError::Reserved(_))
        ));
        assert!(matches!(
            registry.register(profile("rusty.custom", 1), source),
            Err(ProfileRegistryError::Reserved(_))
        ));
    }
}

#[test]
fn host_profiles_take_precedence_over_workspace_profiles() {
    let mut registry = ProfileRegistry::new();
    let mut from_workspace = profile("team", 1);
    from_workspace.instructions.text = "workspace".into();
    let mut from_host = profile("team", 1);
    from_host.instructions.text = "host".into();

    registry
        .register(from_workspace.clone(), ProfileSource::Workspace)
        .unwrap();
    registry.register(from_host, ProfileSource::Host).unwrap();
    // A later workspace registration does not displace the host's.
    registry
        .register(from_workspace, ProfileSource::Workspace)
        .unwrap();

    let resolved = registry.resolve(&ProfileRef::exact("team", 1)).unwrap();
    assert_eq!(resolved.profile.instructions.text, "host");
}

#[test]
fn published_revisions_are_immutable_but_drafts_can_be_edited() {
    let mut registry = ProfileRegistry::new();
    registry
        .register(profile("p", 1), ProfileSource::Host)
        .unwrap();
    registry
        .register(profile("p", 1), ProfileSource::Host)
        .expect("identical re-registration is idempotent");
    let mut changed = profile("p", 1);
    changed.name = "changed".into();
    assert!(matches!(
        registry.register(changed, ProfileSource::Host),
        Err(ProfileRegistryError::ImmutableRevision(_))
    ));

    let mut draft = profile("p", 2);
    draft.status = DefinitionStatus::Draft;
    registry
        .register(draft.clone(), ProfileSource::Host)
        .unwrap();
    draft.name = "edited draft".into();
    registry
        .register(draft, ProfileSource::Host)
        .expect("drafts are editable");

    assert_eq!(
        registry
            .resolve(&ProfileRef::latest("p"))
            .unwrap()
            .profile
            .revision,
        1,
        "latest skips drafts"
    );
    assert!(matches!(
        registry.resolve(&ProfileRef::exact("p", 2)),
        Err(ProfileRegistryError::DraftNotExecutable(_))
    ));
    let registry = registry.allow_drafts(true);
    assert_eq!(
        registry
            .resolve(&ProfileRef::latest("p"))
            .unwrap()
            .profile
            .revision,
        2,
        "with drafts allowed, the newest draft is the latest"
    );
    assert_eq!(
        registry
            .resolve(&ProfileRef::exact("p", 2))
            .unwrap()
            .profile
            .name,
        "edited draft"
    );
}

#[test]
fn agents_resolve_explicit_then_configured_default_then_builtin() {
    let mut registry = ProfileRegistry::new();
    registry
        .register(profile("team", 1), ProfileSource::Workspace)
        .unwrap();
    registry
        .register(profile("explicit", 1), ProfileSource::Host)
        .unwrap();

    assert_eq!(
        registry
            .resolve_for_agent(None)
            .unwrap()
            .profile
            .id
            .as_str(),
        DEFAULT_PROFILE_ID
    );
    registry.set_default(Some(ProfileRef::latest("team")));
    assert_eq!(
        registry
            .resolve_for_agent(None)
            .unwrap()
            .profile
            .id
            .as_str(),
        "team"
    );
    assert_eq!(
        registry
            .resolve_for_agent(Some(&ProfileRef::exact("explicit", 1)))
            .unwrap()
            .profile
            .id
            .as_str(),
        "explicit"
    );
    registry.set_default(Some(ProfileRef::latest("missing")));
    assert!(
        matches!(
            registry.resolve_for_agent(None),
            Err(ProfileRegistryError::NotFound(_))
        ),
        "an unresolvable default never falls back silently"
    );
}

// ---------------------------------------------------------------------------
// Run state
// ---------------------------------------------------------------------------

fn limited(max_turns: Option<u32>, max_tool_calls: Option<u32>) -> BehaviorState {
    let mut profile = profile("limited", 1);
    profile.limits = Limits {
        max_turns,
        max_tool_calls,
        final_turn_prompt: Some("Wrap up.".into()),
    };
    BehaviorState::new(compiled(profile))
}

#[test]
fn turn_limit_makes_the_last_turn_final_then_refuses_more() {
    let mut state = limited(Some(2), None);
    assert_eq!(state.begin_turn(), TurnPlan::Normal);
    assert!(state.admit_tool_call());
    assert_eq!(
        state.begin_turn(),
        TurnPlan::Final {
            prompt: Some("Wrap up.".into())
        }
    );
    assert!(!state.admit_tool_call(), "no tools on the final turn");
    for _ in 0..FINAL_TURN_GRACE {
        assert_eq!(
            state.begin_turn(),
            TurnPlan::Final {
                prompt: Some("Wrap up.".into())
            },
            "grace retries repeat the final turn"
        );
        assert!(!state.admit_tool_call());
    }
    assert_eq!(state.begin_turn(), TurnPlan::Exceeded);

    state.reset_run();
    assert_eq!(state.begin_turn(), TurnPlan::Normal, "limits are per run");
}

#[test]
fn tool_call_limit_refuses_extra_calls_and_forces_a_final_turn() {
    let mut state = limited(None, Some(2));
    assert_eq!(state.begin_turn(), TurnPlan::Normal);
    assert!(state.admit_tool_call());
    assert!(state.admit_tool_call());
    assert!(
        !state.admit_tool_call(),
        "third call in the same turn is refused"
    );
    assert_eq!(state.run.tool_calls, 2);
    assert!(matches!(state.begin_turn(), TurnPlan::Final { .. }));

    let mut unlimited = BehaviorState::default();
    for _ in 0..1000 {
        assert_eq!(unlimited.begin_turn(), TurnPlan::Normal);
        assert!(unlimited.admit_tool_call());
    }
}

#[test]
fn behavior_state_round_trips_and_detects_tampering() {
    let mut state = limited(Some(5), None);
    state.begin_turn();
    let stored = state.to_stored();
    let restored = BehaviorState::from_stored(Some(&stored)).unwrap();
    assert_eq!(restored.profile.content_hash, state.profile.content_hash);
    assert_eq!(restored.run, state.run);

    let mut tampered = stored.clone();
    tampered["profile"]["limits"]["max_turns"] = json!(500);
    assert!(matches!(
        BehaviorState::from_stored(Some(&tampered)),
        Err(BehaviorRestoreError::HashMismatch(_))
    ));
    assert!(matches!(
        BehaviorState::from_stored(Some(&json!({"profile": 1}))),
        Err(BehaviorRestoreError::Malformed(_))
    ));
    assert_eq!(
        BehaviorState::from_stored(None)
            .unwrap()
            .profile
            .profile
            .id
            .as_str(),
        DEFAULT_PROFILE_ID,
        "pre-behavior snapshots restore under the default"
    );
}

// ---------------------------------------------------------------------------
// Children
// ---------------------------------------------------------------------------

fn spawn_spec() -> SpawnAgentSpec {
    use harness_protocol::effects::{BackendPolicy, SpawnMode, ToolInheritance, WorkspacePolicy};
    SpawnAgentSpec {
        role: None,
        backend: BackendPolicy::Inherit,
        tools: ToolInheritance::InheritAll,
        workspace: WorkspacePolicy::Inherit,
        budget: Default::default(),
        mode: SpawnMode::Concurrent,
        task: None,
        execution_params: Default::default(),
        origin_tool_call_id: None,
    }
}

#[test]
fn children_inherit_the_active_profile_with_fresh_counters() {
    let mut parent = limited(Some(3), None);
    parent.begin_turn();
    parent.admit_tool_call();
    let child = PolicyChildBehaviorResolver
        .resolve(&parent, &spawn_spec())
        .unwrap();
    assert!(Arc::ptr_eq(&child.profile, &parent.profile));
    assert_eq!(child.run, RunCounters::default());
}

// ---------------------------------------------------------------------------
// Rules: validation
// ---------------------------------------------------------------------------

fn with_rules(rules: serde_json::Value) -> Result<CompiledProfile, ProfileValidationError> {
    let mut document = serde_json::to_value(profile("ruled", 1)).unwrap();
    document["rules"] = rules;
    compile(serde_json::from_value(document).expect("rules parse"))
}

#[test]
fn the_design_doc_example_rules_compile() {
    with_rules(json!([
        { "id": "decide-first", "on": "BeforeModelRequest",
          "when": { "calls": { "tool": "infer_decision", "eq": 0 } },
          "do": { "inject": { "text": "Start by calling infer_decision." } } },
        { "id": "fetch-before-edit", "on": "PreToolUse",
          "when": { "all": [ { "tool": "fs.edit" }, { "calls": { "tool": "smart_fetch", "eq": 0 } } ] },
          "do": { "deny": { "reason": "Gather the data with smart_fetch before editing." } } },
        { "id": "redecide", "on": "BeforeModelRequest",
          "when": { "turns_since_call": { "tool": "infer_decision", "gte": 6 } },
          "max_fires": 2,
          "do": { "inject": { "text": "Re-evaluate with infer_decision." } } },
        { "id": "test-after-edit", "on": "PostToolUse", "when": { "tool": "fs.edit" },
          "do": { "inject": { "text": "Run run_tests before claiming completion." } } },
        { "id": "no-loops", "on": "PreToolUse", "when": { "repeated_call": { "gte": 3 } },
          "do": { "deny": { "reason": "Change approach." } } },
        { "id": "billing", "on": "PreToolUse",
          "when": { "arg": { "pointer": "/path", "glob": "src/billing/**" } },
          "do": { "ask": {} } },
        { "id": "trusted-read", "on": "PreToolUse", "when": { "tool": ["fs.read", "workspace.*"] },
          "do": { "allow": {} } },
        { "id": "halt", "on": "PostToolUseFailure", "when": { "result_contains": "FATAL" },
          "do": { "stop_run": { "reason": "Fatal tool error." } } },
        { "id": "greet", "on": "RunStart",
          "do": { "inject": { "text": "Mind the conventions.", "placement": "persistent" } } }
    ]))
    .expect("every example in BEHAVIOR_LAYER_DESIGN.md §6.5 is valid");
}

#[test]
fn rules_are_checked_against_the_event_they_fire_on() {
    let error = with_rules(json!([
        { "id": "a", "on": "BeforeModelRequest", "when": { "tool": "x" },
          "do": { "inject": { "text": "t" } } },
        { "id": "a", "on": "PostToolUse", "do": { "deny": { "reason": "r" } } },
        { "id": "b", "on": "PreToolUse", "when": { "result_contains": "x" },
          "do": { "inject": { "text": "t", "placement": "persistent" } } },
        { "id": "c", "on": "PostToolUse", "when": { "repeated_call": {} },
          "do": { "allow": {} } },
        { "id": "d", "on": "PreToolUse",
          "when": { "all": [ { "arg": { "pointer": "path" } }, { "any": [] } ] },
          "max_fires": 0,
          "do": { "stop_run": { "reason": " " } } }
    ]))
    .unwrap_err();
    let codes = codes(&error);
    for expected in [
        "condition_not_available",
        "duplicate_rule_id",
        "action_not_available",
        "placement_not_available",
        "empty_comparison",
        "invalid_pointer",
        "empty_match",
        "empty_combinator",
        "invalid_limit",
        "empty_text",
    ] {
        assert!(codes.contains(&expected), "missing {expected} in {codes:?}");
    }
}

#[test]
fn unknown_events_conditions_and_actions_fail_to_parse() {
    let mut document = serde_json::to_value(profile("ruled", 1)).unwrap();
    for rule in [
        json!({ "id": "x", "on": "Stop", "do": { "inject": { "text": "t" } } }),
        json!({ "id": "x", "on": "PreToolUse", "when": { "regex": "a.*" }, "do": { "allow": {} } }),
        json!({ "id": "x", "on": "PreToolUse", "do": { "rewrite_args": {} } }),
    ] {
        document["rules"] = json!([rule]);
        assert!(serde_json::from_value::<BehaviorProfile>(document.clone()).is_err());
    }
}

// ---------------------------------------------------------------------------
// Rules: evaluation
// ---------------------------------------------------------------------------

#[test]
fn globs_match_within_and_across_path_segments() {
    use super::rules::glob_match;
    assert!(glob_match("fs.*", "fs.edit"));
    assert!(!glob_match("fs.*", "shell.exec"));
    assert!(glob_match("src/billing/**", "src/billing/a/b.rs"));
    assert!(glob_match("src/**/*.rs", "src/x.rs"));
    assert!(glob_match("src/**/*.rs", "src/a/b/x.rs"));
    assert!(!glob_match("src/*.rs", "src/a/x.rs"));
    assert!(glob_match("file?.txt", "file1.txt"));
    assert!(!glob_match("file?.txt", "file12.txt"));
    assert!(glob_match("exact", "exact"));
    assert!(!glob_match("exact", "exactly"));
}

fn state_with(rules: serde_json::Value) -> BehaviorState {
    BehaviorState::new(Arc::new(with_rules(rules).unwrap()))
}

fn call(name: &str, arguments: serde_json::Value) -> harness_protocol::tools::ToolCall {
    harness_protocol::tools::ToolCall {
        id: harness_protocol::ids::ToolCallId::new(),
        name: name.into(),
        arguments,
    }
}

fn pre(state: &mut BehaviorState, call: &harness_protocol::tools::ToolCall) -> RuleOutcome {
    state.note_tool_request(call);
    state.evaluate(
        RuleEvent::PreToolUse,
        Some(ToolEvent { call, result: None }),
    )
}

#[test]
fn the_first_decision_wins_and_injections_accumulate() {
    let mut state = state_with(json!([
        { "id": "note", "on": "PreToolUse", "do": { "inject": { "text": "one" } } },
        { "id": "ask", "on": "PreToolUse", "do": { "ask": {} } },
        { "id": "deny", "on": "PreToolUse", "do": { "deny": { "reason": "no" } } },
        { "id": "note2", "on": "PreToolUse", "do": { "inject": { "text": "two" } } }
    ]));
    let outcome = pre(&mut state, &call("fs.read", json!({})));
    assert_eq!(
        outcome.decision,
        Some(ToolDecision::Ask {
            rule_id: "ask".into()
        })
    );
    let fired: Vec<_> = outcome.fired.iter().map(|f| f.rule_id.as_str()).collect();
    assert_eq!(
        fired,
        ["note", "ask", "note2"],
        "the losing deny does not fire"
    );
    assert_eq!(outcome.injections.len(), 2);
    assert_eq!(outcome.injections[0].placement, Placement::WithResult);
    assert!(outcome.injections[0]
        .text
        .starts_with("<system-reminder source=\"profile:ruled@1 rule:note\">"));
}

#[test]
fn stop_run_ends_evaluation_and_max_fires_is_per_run() {
    let mut state = state_with(json!([
        { "id": "once", "on": "BeforeModelRequest", "max_fires": 1,
          "do": { "inject": { "text": "hi" } } },
        { "id": "stop", "on": "BeforeModelRequest", "when": { "turn": { "gte": 2 } },
          "do": { "stop_run": { "reason": "enough" } } },
        { "id": "never", "on": "BeforeModelRequest", "do": { "inject": { "text": "x" } } }
    ]));
    state.begin_turn();
    let first = state.evaluate(RuleEvent::BeforeModelRequest, None);
    assert_eq!(first.injections.len(), 2);
    assert!(first.stop.is_none());

    state.begin_turn();
    let second = state.evaluate(RuleEvent::BeforeModelRequest, None);
    assert_eq!(second.stop.as_ref().unwrap().reason, "enough");
    assert!(
        second.injections.is_empty(),
        "once fired already; never is after the stop"
    );

    state.reset_run();
    state.begin_turn();
    assert_eq!(
        state
            .evaluate(RuleEvent::BeforeModelRequest, None)
            .injections
            .len(),
        2,
        "max_fires resets with the run"
    );
}

#[test]
fn history_conditions_follow_executed_calls() {
    let mut state = state_with(json!([
        { "id": "no-fetch-yet", "on": "BeforeModelRequest",
          "when": { "calls": { "tool": "smart_fetch", "eq": 0 } },
          "do": { "inject": { "text": "fetch" } } },
        { "id": "stale-decision", "on": "BeforeModelRequest",
          "when": { "turns_since_call": { "tool": "infer_decision", "gte": 2 } },
          "do": { "inject": { "text": "redecide" } } },
        { "id": "untested-edit", "on": "BeforeModelRequest",
          "when": { "since_last_call": { "of": "fs.edit", "called": "run_tests", "eq": 0 } },
          "do": { "inject": { "text": "test" } } }
    ]));
    let fired = |state: &mut BehaviorState| -> Vec<String> {
        state.begin_turn();
        state
            .evaluate(RuleEvent::BeforeModelRequest, None)
            .fired
            .into_iter()
            .map(|fired| fired.rule_id)
            .collect()
    };

    assert_eq!(
        fired(&mut state),
        ["no-fetch-yet"],
        "turn 1: nothing called yet"
    );
    state.note_executed("infer_decision");
    state.note_executed("smart_fetch");
    state.note_executed("fs.edit");
    assert_eq!(
        fired(&mut state),
        ["untested-edit"],
        "turn 2: edited, untested"
    );
    state.note_executed("run_tests");
    assert_eq!(
        fired(&mut state),
        ["stale-decision"],
        "turn 3: decision is 2 turns old"
    );
}

#[test]
fn a_rule_for_write_file_also_covers_edit_file_but_not_the_reverse() {
    let mut state = state_with(json!([
        { "id": "read-only", "on": "PreToolUse", "when": { "tool": ["write_file"] },
          "do": { "deny": { "reason": "read-only" } } },
        { "id": "alias-only", "on": "PreToolUse", "when": { "tool": "edit_file" },
          "do": { "inject": { "text": "targeted" } } }
    ]));
    let fired = |state: &mut BehaviorState, name: &str| -> Vec<String> {
        pre(state, &call(name, json!({})))
            .fired
            .into_iter()
            .map(|fired| fired.rule_id)
            .collect()
    };

    assert_eq!(fired(&mut state, "write_file"), ["read-only"]);
    assert_eq!(
        fired(&mut state, "edit_file"),
        ["read-only", "alias-only"],
        "a deny written for write_file cannot be sidestepped by editing instead"
    );
    assert!(fired(&mut state, "read_file").is_empty());
    assert!(matches!(
        pre(&mut state, &call("edit_file", json!({}))).decision,
        Some(ToolDecision::Deny { .. })
    ));
}

#[test]
fn history_conditions_count_edit_file_as_write_file() {
    let mut state = state_with(json!([
        { "id": "unchecked-write", "on": "BeforeModelRequest",
          "when": { "since_last_call": { "of": "write_file", "called": ["run_command", "read_file"], "eq": 0 } },
          "do": { "inject": { "text": "check your work" } } },
        { "id": "no-write-yet", "on": "BeforeModelRequest",
          "when": { "calls": { "tool": "write_file", "eq": 0 } },
          "do": { "inject": { "text": "write something" } } }
    ]));
    let fired = |state: &mut BehaviorState| -> Vec<String> {
        state.begin_turn();
        state
            .evaluate(RuleEvent::BeforeModelRequest, None)
            .fired
            .into_iter()
            .map(|fired| fired.rule_id)
            .collect()
    };

    assert_eq!(fired(&mut state), ["no-write-yet"]);
    state.note_executed("edit_file");
    assert_eq!(
        fired(&mut state),
        ["unchecked-write"],
        "an edit is a write: it ends 'no write yet' and starts 'unchecked'"
    );
    state.note_executed("run_command");
    assert!(
        fired(&mut state).is_empty(),
        "checking after the edit satisfies the rule, as it would after write_file"
    );
}

#[test]
fn argument_result_and_loop_conditions() {
    let mut state = state_with(json!([
        { "id": "billing", "on": "PreToolUse",
          "when": { "arg": { "pointer": "/path", "glob": "src/billing/**" } },
          "do": { "ask": {} } },
        { "id": "rm", "on": "PreToolUse",
          "when": { "arg": { "pointer": "/command", "contains": "rm -rf" } },
          "do": { "deny": { "reason": "no rm" } } },
        { "id": "loop", "on": "PreToolUse", "when": { "repeated_call": { "gte": 3 } },
          "do": { "deny": { "reason": "loop" } } },
        { "id": "fatal", "on": "PostToolUseFailure", "when": { "not": { "result_contains": "retry" } },
          "do": { "stop_run": { "reason": "fatal" } } }
    ]));
    let decision = |state: &mut BehaviorState, call| pre(state, &call).decision;

    assert!(matches!(
        decision(
            &mut state,
            call("fs.edit", json!({"path": "src/billing/invoice.rs"}))
        ),
        Some(ToolDecision::Ask { .. })
    ));
    assert_eq!(
        decision(&mut state, call("fs.edit", json!({"path": "src/ui.rs"}))),
        None
    );
    assert!(matches!(
        decision(
            &mut state,
            call("shell.exec", json!({"command": "sudo rm -rf /"}))
        ),
        Some(ToolDecision::Deny { .. })
    ));

    for attempt in 1..=3 {
        let outcome = decision(&mut state, call("fs.read", json!({"path": "a"})));
        assert_eq!(outcome.is_some(), attempt == 3, "attempt {attempt}");
    }
    assert_eq!(
        decision(&mut state, call("fs.read", json!({"path": "b"}))),
        None,
        "different arguments reset the streak"
    );

    let failed = call("shell.exec", json!({}));
    let outcome = state.evaluate(
        RuleEvent::PostToolUseFailure,
        Some(ToolEvent {
            call: &failed,
            result: Some("exit 1, will retry"),
        }),
    );
    assert!(outcome.stop.is_none());
    let outcome = state.evaluate(
        RuleEvent::PostToolUseFailure,
        Some(ToolEvent {
            call: &failed,
            result: Some("exit 1"),
        }),
    );
    assert!(outcome.stop.is_some());
}

#[test]
fn rule_bookkeeping_survives_a_snapshot() {
    let mut state = state_with(json!([
        { "id": "once", "on": "BeforeModelRequest", "max_fires": 1,
          "do": { "inject": { "text": "hi" } } }
    ]));
    state.begin_turn();
    state.evaluate(RuleEvent::BeforeModelRequest, None);
    state.note_executed("fs.read");
    let restored = BehaviorState::from_stored(Some(&state.to_stored())).unwrap();
    assert_eq!(restored.run, state.run);
    assert_eq!(restored.run.fired.get("once"), Some(&1));
}

// ---------------------------------------------------------------------------
// Completion gate: validation
// ---------------------------------------------------------------------------

fn with_gate(gate: serde_json::Value) -> Result<CompiledProfile, ProfileValidationError> {
    let mut document = serde_json::to_value(profile("gated", 1)).unwrap();
    document["completion_gate"] = gate;
    compile(serde_json::from_value(document).expect("gate parses"))
}

#[test]
fn the_design_doc_example_gate_compiles() {
    let compiled = with_gate(json!({
        "checks": [
            { "id": "tested",
              "require": { "not": { "since_last_call": { "of": "fs.edit", "called": "run_tests", "eq": 0 } } },
              "feedback": "You changed code after the last test run. Run run_tests." },
            { "id": "tests-green", "evaluator": { "type": "tool", "tool": "run_tests", "args": {} } },
            { "id": "judge", "evaluator": { "type": "model", "instructions": "Did the answer fully address the request?",
                                            "model": "claude-haiku-4-5-20251001" },
              "error_policy": "pass" }
        ],
        "max_continuations": 3,
        "on_exhausted": "fail"
    }))
    .expect("valid gate");
    let gate = compiled.profile.completion_gate.unwrap();
    assert_eq!(gate.max_continuations, 3);
    assert_eq!(gate.on_exhausted, Some(OnExhausted::Fail));
}

#[test]
fn gate_defaults_are_three_continuations_and_ten_transcript_messages() {
    let compiled = with_gate(json!({
        "checks": [{ "id": "judge", "evaluator": { "type": "model", "instructions": "ok?" } }]
    }))
    .unwrap();
    let gate = compiled.profile.completion_gate.unwrap();
    assert_eq!(gate.max_continuations, 3);
    assert_eq!(gate.on_exhausted, None);
    assert!(matches!(
        &gate.checks[0].evaluator,
        Some(EvaluatorSpec::Model {
            transcript_messages: 10,
            ..
        })
    ));
}

#[test]
fn gate_checks_are_validated() {
    let error = with_gate(json!({
        "checks": [
            { "id": "both", "require": { "turn": { "gte": 1 } }, "feedback": "x",
              "evaluator": { "type": "tool", "tool": "t" } },
            { "id": "neither" },
            { "id": "no-feedback", "require": { "turn": { "gte": 1 } } },
            { "id": "tool-cond", "require": { "tool": "fs.edit" }, "feedback": "x" },
            { "id": "tool-cond", "evaluator": { "type": "agent", "instructions": "check", "max_turns": 0 } },
            { "id": "cmd", "evaluator": { "type": "command", "command": "", "timeout_ms": 0 } },
            { "id": "blank", "evaluator": { "type": "model", "instructions": " ", "transcript_messages": 500 } }
        ]
    }))
    .unwrap_err();
    let codes = codes(&error);
    for expected in [
        "invalid_check",
        "missing_feedback",
        "condition_not_available",
        "duplicate_check_id",
        "empty_text",
        "invalid_limit",
    ] {
        assert!(codes.contains(&expected), "missing {expected} in {codes:?}");
    }
    let empty = with_gate(json!({ "checks": [] })).unwrap_err();
    assert!(empty.issues.iter().any(|issue| issue.code == "empty_gate"));
}

// ---------------------------------------------------------------------------
// Switching: validation and closure
// ---------------------------------------------------------------------------

fn switching(id: &str, target: &str) -> BehaviorProfile {
    serde_json::from_value(json!({
        "schema_version": 1, "id": id, "revision": 1, "name": id,
        "rules": [{ "id": "go", "on": "PostToolUse", "when": { "tool": "done" },
                    "do": { "switch_profile": { "profile": { "id": target } } } }]
    }))
    .unwrap()
}

#[test]
fn switch_rules_are_validated() {
    let error = with_rules(json!([
        { "id": "a", "on": "PreToolUse", "do": { "switch_profile": { "profile": { "id": "" } } } },
        { "id": "b", "on": "BeforeModelRequest", "when": { "profile_entered_from": "plan" },
          "do": { "inject": { "text": "x" } } },
        { "id": "c", "on": "ProfileEntered", "do": { "inject": { "text": "x", "placement": "persistent" } } }
    ]))
    .unwrap_err();
    let codes = codes(&error);
    for expected in [
        "invalid_profile_reference",
        "condition_not_available",
        "placement_not_available",
    ] {
        assert!(codes.contains(&expected), "missing {expected} in {codes:?}");
    }
    with_rules(json!([
        { "id": "ok", "on": "ProfileEntered", "when": { "profile_entered_from": ["plan", "draft-*"] },
          "do": { "inject": { "text": "welcome" } } }
    ]))
    .expect("profile_entered_from on ProfileEntered is valid");
}

#[test]
fn switch_closures_follow_every_reachable_target_and_tolerate_cycles() {
    let mut registry = ProfileRegistry::new();
    for profile in [
        switching("plan", "build"),
        switching("build", "review"),
        switching("review", "plan"),
    ] {
        registry.register(profile, ProfileSource::Host).unwrap();
    }
    let plan = registry.resolve(&ProfileRef::latest("plan")).unwrap();
    let mut ids: Vec<_> = registry
        .resolve_closure(&plan)
        .unwrap()
        .iter()
        .map(|profile| profile.profile.id.to_string())
        .collect();
    assert_eq!(ids.remove(0), "plan", "the root comes first");
    ids.sort();
    assert_eq!(ids, ["build", "review"]);

    registry
        .register(switching("orphan", "missing"), ProfileSource::Host)
        .unwrap();
    let orphan = registry.resolve(&ProfileRef::latest("orphan")).unwrap();
    assert!(matches!(
        registry.resolve_closure(&orphan),
        Err(ProfileRegistryError::NotFound(_))
    ));
}

// ---------------------------------------------------------------------------
// Phase 5: rewrites and evaluators
// ---------------------------------------------------------------------------

#[test]
fn merge_patch_follows_rfc_7396() {
    let mut target = json!({ "a": 1, "b": { "c": 2, "d": 3 }, "e": [1, 2] });
    merge_patch(
        &mut target,
        &json!({ "a": null, "b": { "c": 20, "x": true }, "e": [9], "f": "new" }),
    );
    assert_eq!(
        target,
        json!({ "b": { "c": 20, "d": 3, "x": true }, "e": [9], "f": "new" })
    );
    let mut scalar = json!("text");
    merge_patch(&mut scalar, &json!({ "k": 1 }));
    assert_eq!(scalar, json!({ "k": 1 }));
}

#[test]
fn rewrite_actions_are_validated() {
    let error = with_rules(json!([
        { "id": "a", "on": "PostToolUse", "do": { "rewrite_args": { "merge": { "x": 1 } } } },
        { "id": "b", "on": "PreToolUse", "do": { "rewrite_args": { "merge": [1] } } },
        { "id": "c", "on": "PreToolUse", "do": { "rewrite_result": { "append": "x" } } },
        { "id": "d", "on": "PostToolUse", "do": { "rewrite_result": { "replace": "x", "append": "y" } } },
        { "id": "e", "on": "PostToolUse", "do": { "rewrite_result": {} } }
    ]))
    .unwrap_err();
    let codes = codes(&error);
    for expected in [
        "action_not_available",
        "invalid_merge_patch",
        "invalid_rewrite",
    ] {
        assert!(codes.contains(&expected), "missing {expected} in {codes:?}");
    }
}

#[test]
fn profiles_report_whether_they_run_commands() {
    let with_command = with_gate(json!({
        "checks": [{ "id": "hook", "evaluator": { "type": "command", "command": "true" } }]
    }))
    .unwrap();
    assert!(with_command.uses_commands());
    let with_agent = with_gate(json!({
        "checks": [{ "id": "judge", "evaluator": { "type": "agent", "instructions": "check" } }]
    }))
    .unwrap();
    assert!(!with_agent.uses_commands());
    assert!(matches!(
        &with_agent.profile.completion_gate.as_ref().unwrap().checks[0].evaluator,
        Some(EvaluatorSpec::Agent {
            max_turns: 8,
            transcript_messages: 10,
            ..
        })
    ));
}
