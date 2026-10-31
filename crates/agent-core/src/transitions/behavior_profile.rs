//! Installing and switching behavior profiles, and applying what a rule decides.

use rusty_protocol::effects::AgentEffect;
use rusty_protocol::events::AgentEvent;
use rusty_protocol::ids::ToolCallId;
use rusty_protocol::messages::{ContentBlock, MessageRole};

use crate::agent::Agent;
use crate::behavior::{
    compile, system_reminder, BehaviorProfile, CompiledProfile, EnteredFrom, Placement, ProfileRef,
    ProfileRegistry, RuleOutcome, RuleStop, MAX_SWITCHES_PER_RUN,
};

/// Compile a profile and its switch library, and check that every switch
/// target reachable from the profile resolves within the library.
pub(super) fn install_bundle(
    profile: serde_json::Value,
    library: Vec<serde_json::Value>,
) -> Option<(
    std::sync::Arc<CompiledProfile>,
    std::sync::Arc<ProfileRegistry>,
)> {
    let profile = compile(serde_json::from_value::<BehaviorProfile>(profile).ok()?).ok()?;
    let documents = library
        .into_iter()
        .map(serde_json::from_value::<BehaviorProfile>)
        .chain(std::iter::once(Ok(profile.profile.clone())))
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    let registry = ProfileRegistry::from_library(documents).ok()?;
    let profile = std::sync::Arc::new(profile);
    registry.resolve_closure(&profile).ok()?;
    Some((profile, std::sync::Arc::new(registry)))
}

impl Agent {
    /// Install a profile and its switch library. Hosts validate both
    /// against their registry before sending, so an invalid document or an
    /// unresolvable switch target here is a host bug; the command is ignored
    /// rather than half-applied. An idle agent switches at once; during a
    /// run the switch applies before the next model request.
    pub(super) fn set_behavior_profile(
        &mut self,
        profile: serde_json::Value,
        library: Vec<serde_json::Value>,
        allow_commands: bool,
    ) -> Vec<AgentEffect> {
        self.state.behavior.commands_trusted = allow_commands;
        let Some((compiled, library)) = install_bundle(profile, library) else {
            debug_assert!(false, "SetBehaviorProfile received an invalid bundle");
            return Vec::new();
        };
        if self.state.active_run.is_some() {
            self.state.pending_profile = Some((compiled, library));
            Vec::new()
        } else {
            self.apply_switch(compiled, library)
        }
    }

    /// Make `profile` active. Counters start fresh and the switch is
    /// announced (and `ProfileEntered` rules fire) before the next request.
    pub(super) fn apply_switch(
        &mut self,
        profile: std::sync::Arc<crate::behavior::CompiledProfile>,
        library: std::sync::Arc<crate::behavior::ProfileRegistry>,
    ) -> Vec<AgentEffect> {
        let from = self.state.behavior.reference();
        if self.state.behavior.profile.content_hash == profile.content_hash {
            self.state.behavior.library = library;
            return Vec::new();
        }
        self.state.behavior = self.state.behavior.switched_to(profile, library);
        let mut effects = vec![AgentEffect::Emit {
            event: AgentEvent::ProfileChanged {
                from: from.to_string(),
                to: self.state.behavior.reference().to_string(),
            },
        }];
        if self.state.active_run.is_some()
            && self.state.behavior.run.switches > MAX_SWITCHES_PER_RUN
        {
            effects.extend(self.fail(
                "BEHAVIOR_SWITCH_LOOP",
                format!(
                    "more than {MAX_SWITCHES_PER_RUN} profile switches in one run (last to {})",
                    self.state.behavior.reference()
                ),
            ));
        }
        effects
    }

    /// Tell the model its mode changed (only when there is earlier
    /// conversation to reinterpret) and fire `ProfileEntered` rules.
    pub(super) fn enter_profile(
        &mut self,
        from: EnteredFrom,
    ) -> Result<Vec<AgentEffect>, Vec<AgentEffect>> {
        let behavior = &self.state.behavior;
        let has_history = self
            .state
            .messages
            .iter()
            .any(|message| message.role == MessageRole::Assistant);
        if has_history {
            let from_name = from.name.clone();
            let mut tools: Vec<String> = self
                .capabilities
                .tools
                .enabled_descriptors()
                .into_iter()
                .map(|descriptor| descriptor.name.clone())
                .filter(|name| behavior.profile.allows_tool(name))
                .collect();
            tools.sort();
            let text = format!(
                "Your operating mode changed from {from_name} to {}. Tools available now: {}. \
                 Follow the instructions for this mode from here on.",
                behavior.profile.profile.name,
                if tools.is_empty() {
                    "none".to_string()
                } else {
                    tools.join(", ")
                }
            );
            let source = format!("profile:{}", behavior.reference());
            let reminder = system_reminder(&source, &text);
            let chars = reminder.chars().count() as u64;
            self.state.behavior.run.pending_request.push(reminder);
            let mut effects = vec![AgentEffect::Emit {
                event: AgentEvent::ContextInjected {
                    source,
                    placement: Placement::NextRequest.name().into(),
                    chars,
                },
            }];
            effects.extend(self.entered_rules(&from.profile)?);
            return Ok(effects);
        }
        self.entered_rules(&from.profile)
    }

    pub(super) fn entered_rules(
        &mut self,
        from: &ProfileRef,
    ) -> Result<Vec<AgentEffect>, Vec<AgentEffect>> {
        let outcome = self.state.behavior.evaluate_entered(from.id.as_str());
        let mut effects = self.apply_rule_outcome(&outcome, None);
        if let Some(stop) = &outcome.stop {
            effects.extend(self.behavior_stop(stop));
            return Err(effects);
        }
        Ok(effects)
    }

    /// Record a rule evaluation: emit what fired, and route injected
    /// context to where its placement says. `with_result` text is held for
    /// `call_id` until its result is recorded.
    pub(super) fn apply_rule_outcome(
        &mut self,
        outcome: &RuleOutcome,
        call_id: Option<ToolCallId>,
    ) -> Vec<AgentEffect> {
        let profile = self.state.behavior.reference().to_string();
        let mut effects: Vec<AgentEffect> = outcome
            .fired
            .iter()
            .map(|fired| AgentEffect::Emit {
                event: AgentEvent::BehaviorRuleFired {
                    profile: profile.clone(),
                    rule_id: fired.rule_id.clone(),
                    event: fired.event.name().into(),
                    action: fired.action.into(),
                },
            })
            .collect();
        for injection in &outcome.injections {
            match (injection.placement, call_id) {
                (Placement::WithResult, Some(call_id)) => self
                    .state
                    .behavior
                    .run
                    .pending_results
                    .push((call_id, injection.text.clone())),
                (Placement::Persistent, _) => {
                    if let Some(message) = self
                        .state
                        .messages
                        .last_mut()
                        .filter(|message| message.role == MessageRole::User)
                    {
                        message.content.push(ContentBlock::Text {
                            text: injection.text.clone(),
                        });
                    }
                }
                _ => self
                    .state
                    .behavior
                    .run
                    .pending_request
                    .push(injection.text.clone()),
            }
            effects.push(AgentEffect::Emit {
                event: AgentEvent::ContextInjected {
                    source: format!("profile:{profile} rule:{}", injection.rule_id),
                    placement: injection.placement.name().into(),
                    chars: injection.text.chars().count() as u64,
                },
            });
        }
        if let Some((_, target)) = &outcome.switch {
            match self.state.behavior.resolve_switch(target) {
                Some(next) => {
                    let library = self.state.behavior.library.clone();
                    self.state.pending_profile = Some((next, library));
                }
                // Installed bundles are closed over their switch targets.
                None => debug_assert!(false, "switch target {target} missing from the library"),
            }
        }
        effects
    }

    pub(super) fn behavior_stop(&mut self, stop: &RuleStop) -> Vec<AgentEffect> {
        self.fail(
            "BEHAVIOR_STOP",
            format!("stopped by rule {}: {}", stop.rule_id, stop.reason),
        )
    }
}
