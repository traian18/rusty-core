//! Secrets written into files, and files that should never be committed.
use harness_code_health::{Category, Severity};

use crate::rule::{code, finding, rule};

const SECRET_WORDS: [&str; 7] = [
    "PASSWORD",
    "PASSWD",
    "SECRET",
    "TOKEN",
    "API_KEY",
    "APIKEY",
    "PRIVATE_KEY",
];

fn secret_named(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    SECRET_WORDS.iter().any(|w| upper.contains(w))
}

/// A literal value: not empty, not a variable reference or an expression.
fn literal(value: &str) -> bool {
    let v = value.trim();
    !v.is_empty()
        && !v.starts_with('$')
        && !v.starts_with("{{")
        && !v.starts_with("<%")
        && !v.starts_with('!')
        && !matches!(v, "null" | "~" | "true" | "false")
}

rule!(
    SecretLiteral,
    "sec_secret_literal",
    Category::Security,
    "A pipeline or service definition holds a secret as a literal value.",
    [
        GithubActions,
        GitlabCi,
        CircleCi,
        AzurePipelines,
        Compose,
        Kubernetes,
        Ansible
    ],
    |this, file, _config| {
        let Some(yaml) = &file.yaml else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for document in &yaml.documents {
            document.for_each_entry(&mut |entry| {
                let holder = matches!(
                    entry.key.as_str(),
                    "variables" | "env" | "environment"
                );
                if !holder {
                    return;
                }
                // `KEY: value` maps, and compose's `- KEY=value` lists.
                let mut pairs: Vec<(String, String, u32)> = entry
                    .value
                    .entries()
                    .iter()
                    .filter_map(|e| Some((e.key.clone(), e.value.as_str()?.to_owned(), e.key_line)))
                    .collect();
                for item in entry.value.items() {
                    if let Some((k, v)) = item.as_str().and_then(|s| s.split_once('=')) {
                        pairs.push((k.to_owned(), v.to_owned(), item.line));
                    }
                }
                for (key, value, line) in pairs {
                    if secret_named(&key) && literal(&value) {
                        out.push(finding(
                            this,
                            file,
                            Severity::High,
                            (line, line),
                            &key,
                            format!(
                                "{} is set to a literal, so it is stored in the repository; read it from the platform's secret store.",
                                code(&key)
                            ),
                        ));
                    }
                }
            });
        }
        out
    }
);

fn name_of(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

rule!(
    CommittedSecretFile,
    "sec_committed_secret_file",
    Category::Security,
    "A state file, environment file or private key is committed to the repository.",
    [RepoConfig],
    |this, file, _config| {
        let name = name_of(file.path).to_ascii_lowercase();
        let (severity, why) = if name.ends_with(".tfstate") || name.ends_with(".tfstate.backup") {
            (Severity::Critical, "Terraform state holds resource secrets in plain text; keep it in a remote backend and ignore it in git.")
        } else if name == ".env" || name.starts_with(".env.") {
            (Severity::High, "Environment files usually hold credentials; commit a `.env.example` instead and ignore the real file.")
        } else if name.ends_with(".pem")
            || name.ends_with(".key")
            || name == "id_rsa"
            || name == "id_ed25519"
        {
            if !file.text.contains("PRIVATE KEY") {
                return Vec::new();
            }
            (Severity::Critical, "A private key is committed; revoke it, remove it from history, and load keys from a secret store.")
        } else {
            return Vec::new();
        };
        vec![finding(this, file, severity, (1, 1), "", why.to_owned())]
    }
);
