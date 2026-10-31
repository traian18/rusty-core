//! Commands a repository declares: `package.json` scripts and Makefile targets. A writer may give
//! `npm run <key>` or `make <target>` for a key it sees here, and no other command it made up.
use serde_json::Value;

pub struct Script {
    /// The file that declares it.
    pub manifest: String,
    /// The folder to run it from.
    pub directory: String,
    pub invocation: String,
    /// What it runs, as declared.
    pub command: String,
}

const MAX_SCRIPTS: usize = 24;

fn directory_of(path: &str) -> String {
    path.rsplit_once('/')
        .map_or_else(|| ".".to_owned(), |(d, _)| d.to_owned())
}

/// Scripts from a `package.json`.
pub fn npm(path: &str, text: &str) -> Vec<Script> {
    let Ok(json) = serde_json::from_str::<Value>(text) else {
        return Vec::new();
    };
    let Some(scripts) = json.get("scripts").and_then(Value::as_object) else {
        return Vec::new();
    };
    scripts
        .iter()
        .take(MAX_SCRIPTS)
        .map(|(key, command)| Script {
            manifest: path.to_owned(),
            directory: directory_of(path),
            invocation: format!("npm run {key}"),
            command: command.as_str().unwrap_or_default().to_owned(),
        })
        .collect()
}

/// Targets from a Makefile: lines `name:` at the start, not variables or special targets.
pub fn make(path: &str, text: &str) -> Vec<Script> {
    text.lines()
        .filter(|line| !line.starts_with(['\t', ' ', '#', '.']))
        .filter_map(|line| {
            let (name, rest) = line.split_once(':')?;
            let name = name.trim();
            let valid = !name.is_empty()
                && !rest.starts_with('=')
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '/'));
            valid.then(|| Script {
                manifest: path.to_owned(),
                directory: directory_of(path),
                invocation: format!("make {name}"),
                command: rest.trim().to_owned(),
            })
        })
        .take(MAX_SCRIPTS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn npm_scripts_and_make_targets() {
        let n = npm(
            "web/package.json",
            r#"{"scripts":{"dev":"vite","test":"vitest run"}}"#,
        );
        assert_eq!(
            n.iter()
                .map(|s| (
                    s.invocation.as_str(),
                    s.directory.as_str(),
                    s.command.as_str()
                ))
                .collect::<Vec<_>>(),
            [
                ("npm run dev", "web", "vite"),
                ("npm run test", "web", "vitest run")
            ]
        );
        let m = make(
            "Makefile",
            "CC := cc\nbuild: deps\n\tcargo build\n.PHONY: build\ntest:\n\tcargo test\n",
        );
        assert_eq!(
            m.iter().map(|s| s.invocation.as_str()).collect::<Vec<_>>(),
            ["make build", "make test"]
        );
        assert!(npm("p.json", "not json").is_empty());
    }
}
