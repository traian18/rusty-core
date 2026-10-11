use super::*;

use harness_protocol::ids::ToolCallId;
use harness_protocol::ids::{RequestId, RunId, Timestamp};
use harness_protocol::messages::MessageRole;
use harness_protocol::tools::{ToolCall, ToolResultSummary};
use harness_runtime::workspace::FakeWorkspace;

fn empty_request() -> ExecutionRequest {
    ExecutionRequest {
        request_id: RequestId::new(),
        run_id: RunId::new(),
        system_prompt: String::new(),
        messages: vec![],
        tools: vec![],
        extended_thinking: false,
        params: Default::default(),
    }
}

fn text_message(role: MessageRole, text: &str) -> AgentMessage {
    AgentMessage {
        id: MessageId::new(),
        role,
        content: vec![ContentBlock::Text {
            text: text.to_string(),
        }],
        created_at: Timestamp::now(),
    }
}

#[tokio::test]
async fn static_prompt_seeds_an_empty_system_prompt() {
    let provider = StaticSystemPromptProvider::new("be helpful");
    let workspace = FakeWorkspace::new();
    let result = provider.assemble(empty_request(), &workspace).await;
    assert_eq!(result.system_prompt, "be helpful");
}

#[tokio::test]
async fn static_prompt_prepends_to_an_existing_prompt() {
    let provider = StaticSystemPromptProvider::new("be helpful");
    let workspace = FakeWorkspace::new();
    let mut request = empty_request();
    request.system_prompt = "existing".to_string();
    let result = provider.assemble(request, &workspace).await;
    assert_eq!(result.system_prompt, "be helpful\n\nexisting");
}

#[tokio::test]
async fn workspace_info_includes_root_path() {
    let provider = WorkspaceInfoProvider::default();
    let workspace = FakeWorkspace::new();
    let result = provider.assemble(empty_request(), &workspace).await;
    assert!(result.system_prompt.contains("Workspace root:"));
}

#[tokio::test]
async fn compaction_is_a_noop_under_the_char_budget() {
    let base = Arc::new(StaticSystemPromptProvider::new("noop"));
    let provider = TruncatingCompactionProvider::new(base, 10_000, 2);
    let workspace = FakeWorkspace::new();
    let mut request = empty_request();
    request.messages = vec![
        text_message(MessageRole::User, "hi"),
        text_message(MessageRole::Assistant, "hello"),
    ];
    let result = provider.assemble(request, &workspace).await;
    assert_eq!(result.messages.len(), 2);
}

#[tokio::test]
async fn compaction_truncates_when_over_budget() {
    let base = Arc::new(StaticSystemPromptProvider::new("noop"));
    let provider = TruncatingCompactionProvider::new(base, 10, 1);
    let workspace = FakeWorkspace::new();
    let mut request = empty_request();
    request.messages = vec![
        text_message(
            MessageRole::User,
            "this message is long enough to exceed budget",
        ),
        text_message(MessageRole::Assistant, "so is this one honestly"),
        text_message(MessageRole::User, "most recent message"),
    ];
    let result = provider.assemble(request, &workspace).await;
    // Truncation note + the single kept (most recent) message.
    assert_eq!(result.messages.len(), 2);
    assert_eq!(result.messages[0].role, MessageRole::System);
    assert!(matches!(
        &result.messages[0].content[0],
        ContentBlock::Text { text } if text.contains("truncated")
    ));
    assert!(matches!(
        &result.messages[1].content[0],
        ContentBlock::Text { text } if text == "most recent message"
    ));
}

// -----------------------------------------------------------------------
// PolicyDrivenCompactionProvider
// -----------------------------------------------------------------------

fn policy_provider(
    context_window: Option<u64>,
    keep_recent: usize,
    fallback_max_chars: usize,
) -> PolicyDrivenCompactionProvider {
    let base = Arc::new(StaticSystemPromptProvider::new("noop"));
    PolicyDrivenCompactionProvider::new(
        base,
        ContextPolicy::default(),
        context_window,
        keep_recent,
        fallback_max_chars,
    )
}

fn long_message(role: MessageRole, approx_tokens: usize) -> AgentMessage {
    text_message(role, &"word ".repeat(approx_tokens))
}

#[tokio::test]
async fn under_soft_limit_proceeds_without_compaction() {
    let provider = policy_provider(Some(1_000_000), 2, 10_000);
    let workspace = FakeWorkspace::new();
    let mut request = empty_request();
    request.messages = vec![text_message(MessageRole::User, "hi")];
    let result = provider.assemble(request, &workspace).await;
    assert_eq!(result.messages.len(), 1);
    assert!(provider.last_compaction().is_none());
    assert_eq!(provider.compaction_count(), 0);
}

#[tokio::test]
async fn over_hard_limit_compacts_down_to_the_policy_target() {
    // A 50k-token window (well above the default 8192-token reserved
    // output budget, unlike a tiny window which would make the budget
    // itself `Unavailable` — see `ContextBudgetUnavailable`) with six
    // ~7500-token messages (~45k tokens total) comfortably crosses the
    // default 85% hard limit against the resulting ~36.8k input budget.
    let provider = policy_provider(Some(50_000), 1, 10_000);
    let workspace = FakeWorkspace::new();
    let mut request = empty_request();
    request.messages = vec![
        long_message(MessageRole::User, 6_000),
        long_message(MessageRole::Assistant, 6_000),
        long_message(MessageRole::User, 6_000),
        long_message(MessageRole::Assistant, 6_000),
        long_message(MessageRole::User, 6_000),
        long_message(MessageRole::Assistant, 6_000),
    ];
    let original_len = request.messages.len();

    let result = provider.assemble(request, &workspace).await;

    assert!(
        result.messages.len() < original_len,
        "over-budget request must be compacted, got {} messages unchanged",
        result.messages.len()
    );
    assert_eq!(result.messages[0].role, MessageRole::System);
    let record = provider
        .last_compaction()
        .expect("a compaction should have been recorded");
    assert!(
        record.pressure_percent >= 85,
        "recorded pressure should reflect the hard-limit trigger"
    );
    assert_eq!(provider.compaction_count(), 1);
}

#[tokio::test]
async fn unknown_context_window_falls_back_to_flat_cap_rather_than_skipping_compaction() {
    let provider = policy_provider(None, 1, 10);
    let workspace = FakeWorkspace::new();
    let mut request = empty_request();
    request.messages = vec![
        text_message(
            MessageRole::User,
            "this message is long enough to exceed the fallback cap",
        ),
        text_message(
            MessageRole::Assistant,
            "so is this one, honestly, quite long",
        ),
        text_message(MessageRole::User, "most recent"),
    ];
    let result = provider.assemble(request, &workspace).await;
    assert_eq!(
        result.messages.len(),
        2,
        "fallback cap should still compact when the window is unknown"
    );
    assert_eq!(provider.compaction_count(), 1);
    let record = provider
        .last_compaction()
        .expect("fallback compaction should be recorded");
    assert!(!record.exact);
}

#[tokio::test]
async fn known_window_but_under_cap_does_not_use_the_fallback_path() {
    let provider = policy_provider(Some(1_000_000), 5, 10_000);
    let workspace = FakeWorkspace::new();
    let mut request = empty_request();
    request.messages = vec![text_message(MessageRole::User, "short")];
    let result = provider.assemble(request, &workspace).await;
    assert_eq!(result.messages.len(), 1);
    assert_eq!(provider.compaction_count(), 0);
}

#[tokio::test]
async fn chained_provider_runs_each_in_order() {
    let chain = ChainedContextProvider::new(vec![
        Arc::new(StaticSystemPromptProvider::new("first")),
        Arc::new(StaticSystemPromptProvider::new("second")),
    ]);
    let workspace = FakeWorkspace::new();
    let result = chain.assemble(empty_request(), &workspace).await;
    // Each StaticSystemPromptProvider prepends, so "second" (applied
    // last) ends up in front of "first".
    assert_eq!(result.system_prompt, "second\n\nfirst");
}

// -----------------------------------------------------------------------
// Tiered compaction: elision, pair-safe cuts, pinning
// -----------------------------------------------------------------------

fn tool_pair(name: &str, output: &str) -> (AgentMessage, AgentMessage, ToolCallId) {
    let id = ToolCallId::new();
    let call = AgentMessage {
        id: MessageId::new(),
        role: MessageRole::Assistant,
        content: vec![ContentBlock::ToolUse {
            call: ToolCall {
                id,
                name: name.to_string(),
                arguments: serde_json::Value::Null,
            },
        }],
        created_at: Timestamp::now(),
    };
    let result = AgentMessage {
        id: MessageId::new(),
        role: MessageRole::User,
        content: vec![ContentBlock::ToolResult {
            call_id: id,
            result: ToolResultSummary {
                has_error: false,
                output_preview: output.to_string(),
            },
        }],
        created_at: Timestamp::now(),
    };
    (call, result, id)
}

fn has_result(messages: &[AgentMessage], id: ToolCallId) -> bool {
    messages
        .iter()
        .flat_map(|m| &m.content)
        .any(|b| matches!(b, ContentBlock::ToolResult { call_id, .. } if *call_id == id))
}

fn has_use(messages: &[AgentMessage], id: ToolCallId) -> bool {
    messages
        .iter()
        .flat_map(|m| &m.content)
        .any(|b| matches!(b, ContentBlock::ToolUse { call } if call.id == id))
}

#[tokio::test]
async fn elision_alone_avoids_dropping_turns() {
    let base = Arc::new(StaticSystemPromptProvider::new("noop"));
    let provider = TruncatingCompactionProvider::new(base, 2_000, 2);
    let (call, result, id) = tool_pair("read_file", &"x".repeat(5_000));
    let mut request = empty_request();
    request.messages = vec![
        text_message(MessageRole::User, "task"),
        call,
        result,
        text_message(MessageRole::Assistant, "ok"),
        text_message(MessageRole::User, "next"),
    ];
    let out = provider.assemble(request, &FakeWorkspace::new()).await;
    assert_eq!(out.messages.len(), 5, "no turn should be dropped");
    let ContentBlock::ToolResult { result, .. } = &out.messages[2].content[0] else {
        panic!("expected tool result");
    };
    assert!(result.output_preview.contains("read_file result elided"));
    assert!(has_use(&out.messages, id));
}

#[tokio::test]
async fn cut_never_orphans_a_tool_result() {
    let base = Arc::new(StaticSystemPromptProvider::new("noop"));
    // keep_recent = 1 would cut between the ToolUse and its ToolResult.
    let provider = TruncatingCompactionProvider::new(base, 10, 1);
    let (call, result, id) = tool_pair("grep", "short");
    let mut request = empty_request();
    request.messages = vec![
        text_message(MessageRole::User, "a long first message to exceed budget"),
        call,
        result,
    ];
    let out = provider.assemble(request, &FakeWorkspace::new()).await;
    assert!(has_result(&out.messages, id));
    assert!(
        has_use(&out.messages, id),
        "ToolUse must travel with its result"
    );
}

#[tokio::test]
async fn pinned_and_anchored_messages_survive() {
    let base = Arc::new(StaticSystemPromptProvider::new("noop"));
    let pinned = text_message(MessageRole::Assistant, "the plan: do X then Y");
    let pinned_id = pinned.id;
    let provider = TruncatingCompactionProvider::new(base, 10, 1)
        .with_pinned_messages([pinned_id])
        .with_anchor_first_user_message(true);
    let mut request = empty_request();
    request.messages = vec![
        text_message(MessageRole::User, "original task description"),
        text_message(MessageRole::Assistant, "filler filler filler"),
        pinned,
        text_message(MessageRole::Assistant, "more filler filler"),
        text_message(MessageRole::User, "latest"),
    ];
    let out = provider.assemble(request, &FakeWorkspace::new()).await;
    let ids: Vec<_> = out.messages.iter().map(|m| m.id).collect();
    assert!(ids.contains(&pinned_id));
    assert!(matches!(
        &out.messages[0].content[0],
        ContentBlock::Text { text } if text == "original task description"
    ));
    // anchor, pinned, note, latest
    assert_eq!(out.messages.len(), 4);
}

#[tokio::test]
async fn tool_call_arguments_count_towards_the_budget() {
    let base = Arc::new(StaticSystemPromptProvider::new("noop"));
    let provider = TruncatingCompactionProvider::new(base, 1_000, 2);
    let write = AgentMessage {
        id: MessageId::new(),
        role: MessageRole::Assistant,
        content: vec![ContentBlock::ToolUse {
            call: ToolCall {
                id: ToolCallId::new(),
                name: "write_file".into(),
                arguments: serde_json::json!({ "path": "a.rs", "content": "x".repeat(5_000) }),
            },
        }],
        created_at: Timestamp::now(),
    };
    let mut request = empty_request();
    request.messages = vec![
        text_message(MessageRole::User, "task"),
        write,
        text_message(MessageRole::User, "next"),
        text_message(MessageRole::Assistant, "done"),
    ];
    let out = provider.assemble(request, &FakeWorkspace::new()).await;
    assert!(
        out.messages.len() < 4,
        "a 5k-char write must push the history over a 1k budget"
    );
}
