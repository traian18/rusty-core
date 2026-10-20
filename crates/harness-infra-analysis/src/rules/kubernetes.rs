use harness_code_health::{Category, Severity};

use crate::{
    doc::yaml::Node_,
    file::InfraFile,
    rule::{code, finding, rule},
};

const WORKLOADS: [&str; 7] = [
    "Pod",
    "Deployment",
    "StatefulSet",
    "DaemonSet",
    "ReplicaSet",
    "Job",
    "CronJob",
];

/// The pod spec of a workload document.
fn pod_spec(document: &Node_) -> Option<&Node_> {
    let kind = document.get("kind")?.as_str()?;
    if !WORKLOADS.contains(&kind) {
        return None;
    }
    let spec = document.get("spec")?;
    match kind {
        "Pod" => Some(spec),
        "CronJob" => spec
            .get("jobTemplate")?
            .get("spec")?
            .get("template")?
            .get("spec"),
        _ => spec.get("template")?.get("spec"),
    }
}

/// `(workload name, container)` for every container of every workload in the file.
fn containers<'a>(file: &'a InfraFile<'_>) -> Vec<(String, &'a Node_)> {
    let mut out = Vec::new();
    let Some(yaml) = &file.yaml else {
        return out;
    };
    for document in &yaml.documents {
        let Some(spec) = pod_spec(document) else {
            continue;
        };
        let name = document
            .get("metadata")
            .and_then(|m| m.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_owned();
        for key in ["containers", "initContainers"] {
            for container in spec.get(key).map(|c| c.items()).unwrap_or(&[]) {
                out.push((name.clone(), container));
            }
        }
    }
    out
}

fn container_name(container: &Node_) -> &str {
    container.get("name").and_then(|n| n.as_str()).unwrap_or("")
}

rule!(
    LatestTag,
    "k8s_latest_tag",
    Category::Infrastructure,
    "A container image is untagged or tagged latest.",
    [Kubernetes],
    |this, file, _config| {
        containers(file)
            .into_iter()
            .filter_map(|(workload, container)| {
                let image = container.get("image")?;
                let value = image.as_str()?;
                if value.contains('@') || value.contains('$') || value.contains("{{") {
                    return None;
                }
                let tag = value.rsplit_once(':').filter(|(name, _)| !name.ends_with('/'));
                let floating = tag.is_none_or(|(_, tag)| tag == "latest");
                floating.then(|| {
                    finding(
                        this,
                        file,
                        Severity::Medium,
                        (image.line, image.line),
                        &workload,
                        format!("Image {} has no fixed tag, so pods change when the registry does; pin a version or digest.", code(value)),
                    )
                })
            })
            .collect()
    }
);

rule!(
    Privileged,
    "k8s_privileged",
    Category::Security,
    "A container runs privileged or may escalate privileges.",
    [Kubernetes],
    |this, file, _config| {
        containers(file)
            .into_iter()
            .filter_map(|(workload, container)| {
                let context = container.get("securityContext")?;
                let privileged = context.get("privileged").and_then(|p| p.as_str()) == Some("true");
                let escalation =
                    context.get("allowPrivilegeEscalation").and_then(|p| p.as_str()) == Some("true");
                (privileged || escalation).then(|| {
                    let line = context.entry("privileged").or_else(|| context.entry("allowPrivilegeEscalation")).map_or(context.line, |e| e.key_line);
                    finding(
                        this,
                        file,
                        if privileged { Severity::High } else { Severity::Medium },
                        (line, line),
                        &format!("{workload}/{}", container_name(container)),
                        format!("Container {} runs with elevated privileges; drop `privileged` and set `allowPrivilegeEscalation: false`.", code(container_name(container))),
                    )
                })
            })
            .collect()
    }
);

rule!(
    NoLimits,
    "k8s_no_limits",
    Category::Infrastructure,
    "A container sets no resource limits, so one pod can starve its node.",
    [Kubernetes],
    |this, file, _config| {
        containers(file)
            .into_iter()
            .filter(|(_, c)| c.get("resources").and_then(|r| r.get("limits")).is_none())
            .map(|(workload, container)| {
                finding(
                    this,
                    file,
                    Severity::Low,
                    (container.line, container.line),
                    &format!("{workload}/{}", container_name(container)),
                    format!(
                        "Container {} has no `resources.limits`.",
                        code(container_name(container))
                    ),
                )
            })
            .collect()
    }
);

rule!(
    NoProbes,
    "k8s_no_probes",
    Category::Infrastructure,
    "A long-running container has no readiness or liveness probe.",
    [Kubernetes],
    |this, file, _config| {
        let Some(yaml) = &file.yaml else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for document in &yaml.documents {
            let kind = document.get("kind").and_then(|k| k.as_str()).unwrap_or("");
            if !matches!(kind, "Deployment" | "StatefulSet" | "DaemonSet") {
                continue;
            }
            let Some(spec) = pod_spec(document) else {
                continue;
            };
            for container in spec.get("containers").map(|c| c.items()).unwrap_or(&[]) {
                if container.get("readinessProbe").is_none()
                    && container.get("livenessProbe").is_none()
                {
                    out.push(finding(
                        this,
                        file,
                        Severity::Low,
                        (container.line, container.line),
                        container_name(container),
                        format!("Container {} has no readiness or liveness probe, so traffic reaches it before it is ready.", code(container_name(container))),
                    ));
                }
            }
        }
        out
    }
);
