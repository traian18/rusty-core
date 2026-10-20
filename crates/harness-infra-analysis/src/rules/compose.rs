use harness_code_health::{Category, Severity};

use crate::{
    graph::cycles,
    rule::{code, finding, rule},
};

rule!(
    LatestImage,
    "compose_latest_image",
    Category::Infrastructure,
    "A Compose service uses an image that is untagged or tagged latest.",
    [Compose],
    |this, file, _config| {
        let Some(yaml) = &file.yaml else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for document in &yaml.documents {
            for service in document.get("services").map(|s| s.entries()).unwrap_or(&[]) {
                let Some(image) = service.value.entry("image") else {
                    continue;
                };
                let Some(value) = image.value.as_str() else {
                    continue;
                };
                if value.contains('$') || value.contains('@') {
                    continue;
                }
                let tag = value
                    .rsplit_once(':')
                    .filter(|(name, _)| !name.ends_with('/'));
                if tag.is_none_or(|(_, t)| t == "latest") {
                    out.push(finding(
                        this,
                        file,
                        Severity::Medium,
                        (image.key_line, image.key_line),
                        &service.key,
                        format!(
                            "Service {} uses {} without a fixed tag.",
                            code(&service.key),
                            code(value)
                        ),
                    ));
                }
            }
        }
        out
    }
);

rule!(
    Privileged,
    "compose_privileged",
    Category::Security,
    "A Compose service runs privileged or on the host network.",
    [Compose],
    |this, file, _config| {
        let Some(yaml) = &file.yaml else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for document in &yaml.documents {
            for service in document.get("services").map(|s| s.entries()).unwrap_or(&[]) {
                for (key, expected, why) in [
                    (
                        "privileged",
                        "true",
                        "runs privileged, with nearly full access to the host",
                    ),
                    ("network_mode", "host", "shares the host network namespace"),
                ] {
                    if let Some(entry) = service.value.entry(key) {
                        if entry.value.as_str() == Some(expected) {
                            out.push(finding(
                                this,
                                file,
                                Severity::High,
                                (entry.key_line, entry.key_line),
                                &service.key,
                                format!("Service {} {why}.", code(&service.key)),
                            ));
                        }
                    }
                }
            }
        }
        out
    }
);

rule!(
    DependsCycle,
    "compose_depends_cycle",
    Category::Infrastructure,
    "Services depend on each other in a loop, so Compose cannot order their start.",
    [Compose],
    |this, file, _config| {
        let Some(yaml) = &file.yaml else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for document in &yaml.documents {
            let services = document.get("services").map(|s| s.entries()).unwrap_or(&[]);
            let edges = services
                .iter()
                .map(|s| {
                    let deps = match s.value.get("depends_on") {
                        Some(d) if !d.entries().is_empty() => {
                            d.entries().iter().map(|e| e.key.clone()).collect()
                        }
                        Some(d) => d
                            .items()
                            .iter()
                            .filter_map(|i| i.as_str().map(str::to_owned))
                            .collect(),
                        None => Vec::new(),
                    };
                    (s.key.clone(), deps)
                })
                .collect();
            for component in cycles(&edges) {
                for service in services.iter().filter(|s| component.contains(&s.key)) {
                    let line = service
                        .value
                        .entry("depends_on")
                        .map_or(service.key_line, |d| d.key_line);
                    out.push(finding(
                        this,
                        file,
                        Severity::High,
                        (line, line),
                        &service.key,
                        format!(
                            "Service {} is part of a `depends_on` loop.",
                            code(&service.key)
                        ),
                    ));
                }
            }
        }
        out
    }
);
