//! Delivering user input to a live session.

use harness_protocol::commands::UserInput;
use harness_runtime::session_runtime::SessionCommand;

use super::SessionHandle;
use crate::session_builder::HarnessError;

/// When a piece of user input reaches the root agent.
#[derive(Clone, Copy)]
enum Delivery {
    /// Sent as a prompt.
    Prompt,
    /// Injected into the session.
    Steer,
    /// Queued to run after the current command boundary.
    FollowUp,
}

/// Plain-text input with no attachments.
fn text_input(prompt: &str) -> UserInput {
    UserInput {
        text: prompt.to_string(),
        attachments: vec![],
    }
}

impl SessionHandle {
    pub async fn send(&self, prompt: &str) -> Result<(), HarnessError> {
        self.deliver(Delivery::Prompt, text_input(prompt)).await
    }

    /// Send a complete user input, preserving any supplied attachments.
    pub async fn send_input(&self, input: UserInput) -> Result<(), HarnessError> {
        self.deliver(Delivery::Prompt, input).await
    }

    /// Inject additional user input into this session.
    pub async fn steer(&self, prompt: &str) -> Result<(), HarnessError> {
        self.deliver(Delivery::Steer, text_input(prompt)).await
    }

    /// Inject complete user input, preserving any supplied attachments.
    pub async fn steer_input(&self, input: UserInput) -> Result<(), HarnessError> {
        self.deliver(Delivery::Steer, input).await
    }

    /// Queue a prompt to run after the current command boundary.
    pub async fn follow_up(&self, prompt: &str) -> Result<(), HarnessError> {
        self.deliver(Delivery::FollowUp, text_input(prompt)).await
    }

    /// Queue complete user input, preserving any supplied attachments.
    pub async fn follow_up_input(&self, input: UserInput) -> Result<(), HarnessError> {
        self.deliver(Delivery::FollowUp, input).await
    }

    async fn deliver(&self, delivery: Delivery, input: UserInput) -> Result<(), HarnessError> {
        match delivery {
            Delivery::Prompt => {
                self.client.send(SessionCommand::Prompt(input)).await?;
            }
            Delivery::Steer => {
                self.client.steer(input).await?;
            }
            Delivery::FollowUp => {
                self.client.follow_up(input).await?;
            }
        }
        Ok(())
    }
}
