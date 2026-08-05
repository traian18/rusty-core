//! Minimal end-to-end example: build a client, start a session, send one
//! prompt, and print streamed text deltas until the run completes.
//!
//! Requires `ANTHROPIC_API_KEY` in the environment. Run with:
//!
//! ```console
//! ANTHROPIC_API_KEY=sk-... cargo run -p rusty-harness-sdk --example basic_chat
//! ```

use std::sync::Arc;

use rusty_harness_sdk::protocol::{AgentEvent, AgentOutcome};
use rusty_harness_sdk::{Client, Session, SdkError};

#[tokio::main]
async fn main() -> Result<(), SdkError> {
    tracing_subscriber::fmt::init();

    let client = Client::builder()
        .register_integration(Arc::new(
            harness_integration_anthropic::AnthropicFactory,
        ))
        .build()
        .await?;

    let handle = client
        .session()
        .integration("anthropic", serde_json::json!({}))?
        .start()
        .await?;
    let session = Session::from(handle);

    let mut events = session.events();
    session.send("In one short sentence, what is a Rust trait?").await?;

    while let Some(event) = events.next().await {
        let envelope = event?;
        match envelope.event {
            AgentEvent::AssistantTextDelta { delta, .. } => print!("{delta}"),
            AgentEvent::Completed { outcome } => {
                println!();
                if !matches!(outcome, AgentOutcome::Success) {
                    eprintln!("run ended with outcome: {outcome:?}");
                }
                break;
            }
            AgentEvent::Failed { error } => {
                eprintln!("run failed: {error:?}");
                break;
            }
            _ => {}
        }
    }

    Ok(())
}
