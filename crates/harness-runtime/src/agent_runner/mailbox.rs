//! Reporting back to an agent's mailbox from work running off its loop.

use tokio::sync::mpsc;

use harness_protocol::commands::AgentCommand;

/// Deliver `command` to the agent's mailbox. The mailbox closes only once
/// the runner has stopped, and then nothing is left to act on the command,
/// so that is logged rather than treated as an error.
pub(super) async fn deliver(commands: &mpsc::Sender<AgentCommand>, command: AgentCommand) {
    if commands.send(command).await.is_err() {
        tracing::debug!("agent mailbox closed; dropping a late command");
    }
}
