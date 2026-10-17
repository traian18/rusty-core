mod support;

use std::sync::Arc;

use harness_repository_frameworks::TauriAdapter;
use harness_workspace::framework_relations::{Certainty, FrameworkAdapter, RelationKind};
use support::*;

fn adapters() -> Vec<Arc<dyn FrameworkAdapter>> {
    vec![Arc::new(TauriAdapter)]
}

const CARGO: &str =
    "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\ntauri = \"2\"\n";

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

#[tokio::test]
async fn registered_commands_point_at_the_files_that_define_them() {
    let repo = repo(
        &[
            ("Cargo.toml", CARGO),
            ("src/lib.rs", "mod commands;\nmod files;\nuse files::read_file;\n#[tauri::command]\nfn greet(name: String) -> String { name }\nfn helper() {}\npub fn run() {\n    tauri::Builder::default()\n        .invoke_handler(tauri::generate_handler![\n            greet,\n            commands::save,\n            read_file,\n            #[cfg(feature = \"extra\")]\n            commands::extra,\n            missing,\n        ])\n        .run(tauri::generate_context!());\n}\n"),
            ("src/commands.rs", "#[tauri::command]\npub fn save() {}\n#[tauri::command]\npub fn extra() {}\npub fn not_a_command() {}\n"),
            ("src/files.rs", "use tauri::command;\n#[command]\npub fn read_file() {}\n"),
        ],
        adapters(),
    )
    .await;
    assert_eq!(
        sorted(repo.summary()),
        sorted(vec![
            "CommandExposedAs|greet|src/lib.rs->src/lib.rs".into(),
            "CommandExposedAs|save|src/lib.rs->src/commands.rs".into(),
            "CommandExposedAs|read_file|src/lib.rs->src/files.rs".into(),
            "CommandExposedAs|extra|src/lib.rs->src/commands.rs".into(),
        ])
    );
    let save = repo.relations().iter().find(|r| r.label == "save").unwrap();
    assert_eq!(save.kind, RelationKind::CommandExposedAs);
    assert_eq!(save.certainty, Certainty::Declared);
    assert_eq!(save.evidence.text, "commands::save");
    assert_eq!(save.to_symbol.as_deref(), Some("save"));
    let diagnostics = repo.diagnostics();
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert!(diagnostics[0].contains("`missing`"));
}

#[tokio::test]
async fn a_unique_name_is_used_as_a_convention_when_the_path_cannot_be_followed() {
    let repo = repo(
        &[
            ("Cargo.toml", CARGO),
            (
                "src/lib.rs",
                "mod deep;\npub fn run() { let _ = tauri::generate_handler![deep::inner::go]; }\n",
            ),
            ("src/deep/mod.rs", "pub mod inner;\n"),
            ("src/deep/inner.rs", "#[tauri::command]\npub fn go() {}\n"),
        ],
        adapters(),
    )
    .await;
    let go = &repo.relations()[0];
    assert_eq!(go.label, "go");
    assert_eq!(
        go.to.as_ref().unwrap().as_path().to_str(),
        Some("src/deep/inner.rs")
    );
    assert_eq!(go.certainty, Certainty::Convention);
}

#[tokio::test]
async fn non_commands_and_look_alike_macros_are_ignored_and_the_adapter_is_off_without_tauri() {
    let repo = repo(
        &[
            ("Cargo.toml", CARGO),
            ("src/lib.rs", "#[command]\nfn not_tauri() {}\nmacro_rules! generate_handler { () => {} }\nfn f() { generate_handler!(); other::generate_things![a, b]; }\n"),
        ],
        adapters(),
    )
    .await;
    assert!(repo.relations().is_empty(), "{:?}", repo.summary());

    let off = self::off().await;
    assert!(off.relations().is_empty());
    assert!(off.snapshot.frameworks.applications.is_empty());
}

async fn off() -> Repo {
    repo(
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
            ),
            (
                "src/lib.rs",
                "#[tauri::command]\nfn a() {}\nfn f() { tauri::generate_handler![a]; }\n",
            ),
        ],
        adapters(),
    )
    .await
}
