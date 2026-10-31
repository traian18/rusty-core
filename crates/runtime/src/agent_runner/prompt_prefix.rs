//! Watches the part of a run's requests that prompt caches reuse.
//!
//! Providers cache a request's prefix: the system prompt, then the tools,
//! then the history. Within one run that prefix should never change; when it
//! does, every later token of the request is processed (and billed) again.
//! This compares each request with the previous one of the same run and
//! names the parts that changed, so a host can show what broke the cache.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use rusty_protocol::backend::ExecutionRequest;
use rusty_protocol::ids::RunId;

/// Part names reported in `AgentEvent::PromptPrefixChanged`.
pub(super) const SYSTEM_PROMPT: &str = "system_prompt";
pub(super) const TOOLS: &str = "tools";

#[derive(Debug, Default)]
pub(super) struct PromptPrefixWatch {
    last: Option<Fingerprint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Fingerprint {
    run_id: RunId,
    system_prompt: u64,
    tools: u64,
}

impl PromptPrefixWatch {
    /// The parts that differ from the previous request of the same run;
    /// empty for a run's first request or an unchanged prefix.
    pub(super) fn observe(&mut self, request: &ExecutionRequest) -> Vec<String> {
        let current = Fingerprint {
            run_id: request.run_id,
            system_prompt: hash(&request.system_prompt),
            tools: hash(&serde_json::to_string(&request.tools).unwrap_or_default()),
        };
        let previous = self.last.replace(current);
        let Some(previous) = previous.filter(|p| p.run_id == current.run_id) else {
            return Vec::new();
        };
        let mut changed = Vec::new();
        if previous.system_prompt != current.system_prompt {
            changed.push(SYSTEM_PROMPT.to_owned());
        }
        if previous.tools != current.tools {
            changed.push(TOOLS.to_owned());
        }
        changed
    }
}

fn hash(value: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusty_protocol::backend::ExecutionParams;
    use rusty_protocol::ids::RequestId;

    fn request(run_id: RunId, system_prompt: &str) -> ExecutionRequest {
        ExecutionRequest {
            request_id: RequestId::new(),
            run_id,
            system_prompt: system_prompt.into(),
            messages: Vec::new(),
            tools: Vec::new(),
            extended_thinking: false,
            params: ExecutionParams::default(),
        }
    }

    #[test]
    fn reports_a_changed_system_prompt_within_a_run_only() {
        let mut watch = PromptPrefixWatch::default();
        let run = RunId::new();
        assert!(watch.observe(&request(run, "a")).is_empty());
        assert!(watch.observe(&request(run, "a")).is_empty());
        assert_eq!(watch.observe(&request(run, "b")), vec![SYSTEM_PROMPT]);
        assert!(
            watch.observe(&request(RunId::new(), "c")).is_empty(),
            "a new run starts fresh"
        );
    }
}
