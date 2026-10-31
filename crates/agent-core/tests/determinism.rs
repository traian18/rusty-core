use std::collections::HashMap;

use rusty_agent_core::agent::Agent;
use rusty_agent_core::capabilities::{AgentCapabilities, WorkspaceCapabilities};
use rusty_protocol::backend::{
    BackendBinding, BackendCapabilities, BackendDescriptor, BackendReference,
};
use rusty_protocol::commands::{AgentCommand, UserInput};
use rusty_protocol::ids::{AgentId, BackendId, ConfigurationId, IntegrationId, SessionId};
use rusty_protocol::tools::AgentToolset;
use rusty_protocol::usage::AgentBudget;

fn agent() -> Agent {
    Agent::new(
        AgentId::new(),
        SessionId::new(),
        None,
        0,
        "system".into(),
        BackendBinding {
            reference: BackendReference {
                integration: IntegrationId::new(),
                configuration: ConfigurationId::new(),
                model: None,
            },
            descriptor: BackendDescriptor {
                id: BackendId::new(),
                name: "fake".into(),
                description: "fake".into(),
                capabilities: BackendCapabilities::default(),
            },
        },
        AgentCapabilities {
            tools: AgentToolset {
                tools: HashMap::new(),
            },
            can_spawn_agents: false,
            max_child_depth: None,
            workspace: WorkspaceCapabilities {
                can_read: false,
                can_write: false,
                can_search: false,
            },
            backend: BackendCapabilities::default(),
        },
        AgentBudget::default(),
    )
}

#[test]
fn identical_agent_and_command_produce_identical_state_and_effects() {
    let mut first = agent();
    let mut second = first.clone();
    let command = AgentCommand::StartRun {
        input: UserInput {
            text: "same input".into(),
            attachments: vec![],
        },
    };

    let first_effects = first.apply(command.clone());
    let second_effects = second.apply(command);

    assert_eq!(
        serde_json::to_string(&first_effects).unwrap(),
        serde_json::to_string(&second_effects).unwrap()
    );
    assert_eq!(first.state.status, second.state.status);
    assert_eq!(first.state.active_run, second.state.active_run);
    assert_eq!(
        first.state.transition_sequence,
        second.state.transition_sequence
    );
    assert_eq!(
        serde_json::to_string(&first.state.messages).unwrap(),
        serde_json::to_string(&second.state.messages).unwrap()
    );
}
