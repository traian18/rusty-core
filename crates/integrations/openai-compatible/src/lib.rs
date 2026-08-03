#![warn(clippy::all)]

//! Implements `ExecutionBackend` for any OpenAI-Chat-Completions-compatible
//! endpoint (OpenRouter, Together, Groq, a local Ollama/vLLM/llama.cpp
//! server, ...) by reusing `harness_integration_openai::OpenAiClient`
//! directly, parameterized by base URL, API key, model, and extra headers.
//! No wire format or SSE parsing logic of its own.

pub mod backend;
pub mod config;

pub use backend::{OpenAiCompatibleBackend, OpenAiCompatibleFactory};
pub use config::OpenAiCompatibleConfig;
