//! GitLab CI, CircleCI and Azure Pipelines.
use harness_code_health::{Category, Severity};

use crate::{
    doc::yaml::Node_,
    rule::{code, finding, rule},
};

fn floating(image: &str) -> bool {
    if image.contains('@') || image.contains('$') || image.contains("{{") {
        return false;
    }
    let tag = image
        .rsplit_once(':')
        .filter(|(name, _)| !name.ends_with('/'));
    tag.is_none_or(|(_, tag)| tag == "latest")
}

fn images(node: &Node_, out: &mut Vec<(String, u32)>) {
    node.for_each_entry(&mut |entry| {
        if entry.key == "image" {
            // `image: name` or `image: { name: ... }` (GitLab).
            let value = entry
                .value
                .as_str()
                .or_else(|| entry.value.get("name").and_then(|n| n.as_str()));
            if let Some(value) = value {
                out.push((value.to_owned(), entry.key_line));
            }
        }
    });
}

rule!(
    LatestImage,
    "ci_latest_image",
    Category::Infrastructure,
    "A pipeline job runs in a container image that is untagged or tagged latest.",
    [GitlabCi, CircleCi],
    |this, file, _config| {
        let Some(yaml) = &file.yaml else {
            return Vec::new();
        };
        let mut found = Vec::new();
        for document in &yaml.documents {
            images(document, &mut found);
        }
        found
            .into_iter()
            .filter(|(image, _)| floating(image))
            .map(|(image, line)| {
                finding(
                    this,
                    file,
                    Severity::Medium,
                    (line, line),
                    &image,
                    format!("Job image {} has no fixed tag, so builds change when the image does; pin a version or digest.", code(&image)),
                )
            })
            .collect()
    }
);

rule!(
    UnpinnedInclude,
    "gl_unpinned_include",
    Category::Security,
    "A GitLab pipeline includes a remote or project file without a fixed ref.",
    [GitlabCi],
    |this, file, _config| {
        let Some(yaml) = &file.yaml else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for document in &yaml.documents {
            let Some(include) = document.entry("include") else {
                continue;
            };
            let items: Vec<&Node_> = if include.value.items().is_empty() {
                vec![&include.value]
            } else {
                include.value.items().iter().collect()
            };
            for item in items {
                if let Some(remote) = item.get("remote").and_then(|r| r.as_str()) {
                    out.push(finding(
                        this,
                        file,
                        Severity::Medium,
                        (item.line, item.line),
                        remote,
                        format!("Remote include {} is fetched as it is at pipeline time; vendor it or pin a commit URL.", code(remote)),
                    ));
                }
                if item.get("project").is_some() {
                    let reference = item.get("ref").and_then(|r| r.as_str());
                    if reference.is_none_or(|r| matches!(r, "main" | "master" | "HEAD")) {
                        out.push(finding(
                            this,
                            file,
                            Severity::Medium,
                            (item.line, item.line),
                            "project",
                            "Project include has no fixed `ref`; pin a tag or commit.".to_owned(),
                        ));
                    }
                }
            }
        }
        out
    }
);

fn long_scripts(node: &Node_, limit: u32, out: &mut Vec<(u32, u32, u32)>) {
    node.for_each_entry(&mut |entry| {
        if matches!(
            entry.key.as_str(),
            "script" | "before_script" | "after_script"
        ) {
            let lines: u32 = entry
                .value
                .items()
                .iter()
                .map(|i| i.as_str().map_or(1, |s| s.lines().count() as u32))
                .sum::<u32>()
                + u32::from(entry.value.as_str().is_some())
                    * entry.value.as_str().map_or(0, |s| s.lines().count() as u32);
            if lines > limit {
                out.push((entry.key_line, entry.value.end_line, lines));
            }
        }
    });
}

rule!(
    LongScript,
    "ci_long_script",
    Category::Infrastructure,
    "A job holds a long inline script that cannot be tested or reused.",
    [GitlabCi, CircleCi, AzurePipelines],
    |this, file, config| {
        let Some(yaml) = &file.yaml else {
            return Vec::new();
        };
        let limit = config.limit("ci_long_script", "lines", 25);
        let mut found = Vec::new();
        for document in &yaml.documents {
            long_scripts(document, limit, &mut found);
            // CircleCI `run: command: |` and Azure `script: |` blocks.
            document.for_each_entry(&mut |entry| {
                if matches!(entry.key.as_str(), "command" | "script") {
                    if let Some(text) = entry.value.as_str() {
                        let lines = text.lines().count() as u32;
                        if lines > limit {
                            found.push((entry.key_line, entry.value.end_line, lines));
                        }
                    }
                }
            });
        }
        found
            .into_iter()
            .map(|(start, end, lines)| {
                finding(
                    this,
                    file,
                    Severity::Low,
                    (start, end),
                    "",
                    format!("An inline script of {lines} lines (limit {limit}); move it to a script file in the repository."),
                )
            })
            .collect()
    }
);

rule!(
    UnpinnedOrb,
    "cci_unpinned_orb",
    Category::Security,
    "A CircleCI orb is used at a floating version.",
    [CircleCi],
    |this, file, _config| {
        let Some(yaml) = &file.yaml else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for document in &yaml.documents {
            for orb in document.get("orbs").map(|o| o.entries()).unwrap_or(&[]) {
                let Some(value) = orb.value.as_str() else {
                    continue;
                };
                let version = value.split_once('@').map(|(_, v)| v);
                if version.is_none_or(|v| v == "volatile" || v == "dev:alpha") {
                    out.push(finding(
                        this,
                        file,
                        Severity::Medium,
                        (orb.key_line, orb.key_line),
                        &orb.key,
                        format!("Orb {} is not pinned to a fixed version.", code(value)),
                    ));
                }
            }
        }
        out
    }
);
