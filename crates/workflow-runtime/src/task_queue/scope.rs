//! What the sessions the queue starts may do and judge.

/// Added to every reviewer's instructions: what a task review judges.
pub(super) const REVIEW_SCOPE: &str = "Judge only workflow_input.task: its own criteria and what its instructions ask of it. Something the task itself leaves to another repository's change, a later task, a prerequisite or a follow-up is not this task's to deliver: mention it in summary, but it is never a reason for needs_repair. Use needs_repair only for a defect in what this task had to do. workflow_input.host_checks, when present, is the workflow's own typecheck after this build: errors it says were there before this task are not this task's.";

/// Tools a session that must not change anything keeps: the task planner and the reviewer.
pub(super) fn is_read_only(tool: &str) -> bool {
    matches!(
        tool,
        "read_file"
            | "search_codebase"
            | "list_files"
            | "project_info"
            | "repo_context"
            | "repo_overview"
            | "repo_health"
            | "repo_risk"
            | "repo_symbol"
            | "code_guidelines"
            | "repo_wiki"
            | "report_progress"
            | "ask_user_question"
            | "open_document"
            | "read_workflow_context"
            | "web_search"
            | "web_extract"
    )
}
