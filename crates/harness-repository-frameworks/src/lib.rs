#![warn(clippy::all)]

//! Framework adapters for the repository graph: routes, injected providers, rendered components
//! and exposed commands, read from source and manifests without running anything.
//!
//! Each adapter implements `harness_workspace::framework_relations::FrameworkAdapter`, runs only
//! for packages that declare the framework, and states in its capability text what it reads and
//! what it leaves alone. Literal forms become relations with evidence; computed or dynamic
//! registrations become diagnostics and no relation.

mod actix;
mod angular;
mod aspnet;
mod axum;
mod decorators;
mod django;
mod echo;
mod express;
mod fastapi;
mod flask;
mod gin;
mod go_common;
mod go_routes;
mod handlers;
mod laravel;
mod nestjs;
mod nethttp;
mod nextjs;
mod parallel;
mod php_common;
mod py_common;
mod py_items;
mod react;
mod routes;
mod rust_items;
mod spring;
mod symfony;
mod syntax;
mod tauri;
mod vue;

pub use actix::ActixAdapter;
pub use angular::AngularAdapter;
pub use aspnet::AspNetAdapter;
pub use axum::AxumAdapter;
pub use django::DjangoAdapter;
pub use echo::EchoAdapter;
pub use express::ExpressAdapter;
pub use fastapi::FastApiAdapter;
pub use flask::FlaskAdapter;
pub use gin::GinAdapter;
pub use laravel::LaravelAdapter;
pub use nestjs::NestjsAdapter;
pub use nethttp::NetHttpAdapter;
pub use nextjs::NextjsAdapter;
pub use react::ReactAdapter;
pub use spring::SpringAdapter;
pub use symfony::SymfonyAdapter;
pub use syntax::{find_module, join_relative, normalize};
pub use tauri::TauriAdapter;
pub use vue::VueAdapter;

/// Every adapter this crate provides, in a stable order, for a composition that wants them all.
pub fn default_adapters(
) -> Vec<std::sync::Arc<dyn harness_workspace::framework_relations::FrameworkAdapter>> {
    use std::sync::Arc;
    vec![
        Arc::new(ActixAdapter),
        Arc::new(AngularAdapter),
        Arc::new(AspNetAdapter),
        Arc::new(AxumAdapter),
        Arc::new(DjangoAdapter),
        Arc::new(EchoAdapter),
        Arc::new(ExpressAdapter),
        Arc::new(FastApiAdapter),
        Arc::new(FlaskAdapter),
        Arc::new(GinAdapter),
        Arc::new(LaravelAdapter),
        Arc::new(NetHttpAdapter),
        Arc::new(NestjsAdapter),
        Arc::new(NextjsAdapter),
        Arc::new(ReactAdapter),
        Arc::new(SpringAdapter),
        Arc::new(SymfonyAdapter),
        Arc::new(TauriAdapter),
        Arc::new(VueAdapter),
    ]
}
