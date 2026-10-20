use harness_code_health::{Category, Finding, Severity};

use crate::{
    doc::hcl::{object_has_key, Block},
    file::InfraFile,
    graph::dir_of,
    kind::InfraKind,
    rule::{code, finding, graph_rule, rule, InfraConfig, InfraContext, InfraRule},
};

/// A `required_providers` entry with no `version` constraint.
pub struct UnpinnedProvider;

impl InfraRule for UnpinnedProvider {
    fn id(&self) -> &'static str {
        "tf_unpinned_provider"
    }
    fn version(&self) -> u32 {
        1
    }
    fn category(&self) -> Category {
        Category::Infrastructure
    }
    fn summary(&self) -> &'static str {
        "A required provider has no version constraint, so an upgrade can change the plan."
    }
    fn applies(&self, kind: InfraKind) -> bool {
        matches!(kind, InfraKind::Terraform | InfraKind::Terragrunt)
    }

    fn check(&self, file: &InfraFile<'_>, _: &InfraContext<'_>, _: &InfraConfig) -> Vec<Finding> {
        let Some(hcl) = &file.hcl else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for terraform in hcl.body.blocks_of("terraform") {
            for providers in terraform.body.blocks_of("required_providers") {
                for provider in &providers.body.attrs {
                    // `aws = "~> 5.0"` is the legacy form and is constrained.
                    let constrained =
                        provider.literal().is_some() || object_has_key(&provider.value, "version");
                    if constrained {
                        continue;
                    }
                    out.push(finding(
                        self,
                        file,
                        Severity::Medium,
                        (provider.line, provider.end_line),
                        &provider.name,
                        format!(
                            "Provider {} has no `version` constraint; add one such as `~> 5.0`.",
                            code(&provider.name)
                        ),
                    ));
                }
            }
        }
        out
    }
}

rule!(
    UnpinnedModule,
    "tf_unpinned_module",
    Category::Infrastructure,
    "A module is fetched without a version or from a moving git ref.",
    [Terraform],
    |this, file, _config| {
        let Some(hcl) = &file.hcl else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for module in hcl.body.blocks_of("module") {
            let name = module.labels.first().map_or("", String::as_str);
            let Some(source) = module.body.attr("source").and_then(|a| a.literal()) else {
                continue;
            };
            let local =
                source.starts_with("./") || source.starts_with("../") || source.starts_with('/');
            if local {
                continue;
            }
            let git = source.starts_with("git::")
                || source.starts_with("github.com/")
                || source.starts_with("git@")
                || source.contains(".git");
            let line = module.body.attr("source").map_or(module.line, |a| a.line);
            if git {
                let reference = source
                    .split("ref=")
                    .nth(1)
                    .map(|r| r.split(['&', '?']).next().unwrap_or(r));
                let moving =
                    reference.is_none_or(|r| matches!(r, "main" | "master" | "develop" | "HEAD"));
                if moving {
                    out.push(finding(
                        this,
                        file,
                        Severity::Medium,
                        (line, line),
                        name,
                        format!(
                            "Module {} follows a moving git ref; pin `?ref=` to a tag or commit.",
                            code(name)
                        ),
                    ));
                }
            } else if module.body.attr("version").is_none() {
                out.push(finding(
                    this,
                    file,
                    Severity::Medium,
                    (line, line),
                    name,
                    format!(
                        "Registry module {} has no `version` constraint.",
                        code(name)
                    ),
                ));
            }
        }
        out
    }
);

rule!(
    VariableHygiene,
    "tf_variable_hygiene",
    Category::Infrastructure,
    "A variable has no type or description, so callers must read the code to use it.",
    [Terraform],
    |this, file, _config| {
        let Some(hcl) = &file.hcl else {
            return Vec::new();
        };
        hcl.body
            .blocks_of("variable")
            .filter_map(|variable| {
                let name = variable.labels.first()?;
                let missing: Vec<&str> = [("type", "type"), ("description", "description")]
                    .iter()
                    .filter(|(attr, _)| variable.body.attr(attr).is_none())
                    .map(|(_, label)| *label)
                    .collect();
                (!missing.is_empty()).then(|| {
                    finding(
                        this,
                        file,
                        Severity::Low,
                        (variable.line, variable.line),
                        name,
                        format!("Variable {} has no {}.", code(name), missing.join(" or ")),
                    )
                })
            })
            .collect()
    }
);

rule!(
    OutputHygiene,
    "tf_output_hygiene",
    Category::Infrastructure,
    "An output has no description.",
    [Terraform],
    |this, file, _config| {
        let Some(hcl) = &file.hcl else {
            return Vec::new();
        };
        hcl.body
            .blocks_of("output")
            .filter(|o| o.body.attr("description").is_none())
            .filter_map(|output| {
                let name = output.labels.first()?;
                Some(finding(
                    this,
                    file,
                    Severity::Low,
                    (output.line, output.line),
                    name,
                    format!("Output {} has no description.", code(name)),
                ))
            })
            .collect()
    }
);

fn provisioners<'a>(block: &'a Block, out: &mut Vec<&'a Block>) {
    for inner in &block.body.blocks {
        if inner.kind == "provisioner" {
            out.push(inner);
        }
        provisioners(inner, out);
    }
}

rule!(
    ProvisionerUse,
    "tf_provisioner_use",
    Category::Infrastructure,
    "A provisioner runs commands the plan cannot see or revert.",
    [Terraform],
    |this, file, _config| {
        let Some(hcl) = &file.hcl else {
            return Vec::new();
        };
        let mut found = Vec::new();
        for resource in hcl.body.blocks_of("resource") {
            provisioners(resource, &mut found);
        }
        found
            .into_iter()
            .map(|p| {
                let kind = p.labels.first().map_or("", String::as_str);
                finding(
                    this,
                    file,
                    Severity::Low,
                    (p.line, p.line),
                    kind,
                    format!("A {} provisioner runs outside Terraform's plan; prefer user data, a config tool, or a purpose-built resource.", code(kind)),
                )
            })
            .collect()
    }
);

fn open_cidr(value: &str) -> bool {
    value.contains("\"0.0.0.0/0\"") || value.contains("\"::/0\"")
}

fn open_ingress(block: &Block, found: &mut Vec<(u32, String)>) {
    for inner in &block.body.blocks {
        if inner.kind == "ingress" {
            let open = ["cidr_blocks", "ipv6_cidr_blocks"]
                .iter()
                .filter_map(|a| inner.body.attr(a))
                .any(|a| open_cidr(&a.value));
            if open {
                found.push((inner.line, block.labels.get(1).cloned().unwrap_or_default()));
            }
        }
    }
}

rule!(
    OpenIngress,
    "sec_open_ingress",
    Category::Security,
    "A security group accepts traffic from the whole internet.",
    [Terraform],
    |this, file, _config| {
        let Some(hcl) = &file.hcl else {
            return Vec::new();
        };
        let mut found = Vec::new();
        for resource in hcl.body.blocks_of("resource") {
            let kind = resource.labels.first().map_or("", String::as_str);
            match kind {
                "aws_security_group" => open_ingress(resource, &mut found),
                "aws_security_group_rule" | "aws_vpc_security_group_ingress_rule" => {
                    let ingress = kind.ends_with("ingress_rule")
                        || resource.body.attr("type").and_then(|a| a.literal()) == Some("ingress");
                    let open = ["cidr_blocks", "cidr_ipv4", "cidr_ipv6", "ipv6_cidr_blocks"]
                        .iter()
                        .filter_map(|a| resource.body.attr(a))
                        .any(|a| open_cidr(&a.value));
                    if ingress && open {
                        found.push((
                            resource.line,
                            resource.labels.get(1).cloned().unwrap_or_default(),
                        ));
                    }
                }
                _ => {}
            }
        }
        found
            .into_iter()
            .map(|(line, name)| {
                finding(
                    this,
                    file,
                    Severity::High,
                    (line, line),
                    &name,
                    format!("{} allows ingress from `0.0.0.0/0`; restrict it to the networks that need it.", code(&name)),
                )
            })
            .collect()
    }
);

graph_rule!(
    UnusedVariable,
    "tf_unused_variable",
    Category::Infrastructure,
    "A variable is declared but nothing in its module reads it.",
    [Terraform],
    |this, file, _config, context| {
        let Some(module) = context.graph.and_then(|g| g.modules.get(dir_of(file.path))) else {
            return Vec::new();
        };
        module
            .variables
            .iter()
            .filter(|d| {
                d.path == file.path && !module.references.contains(&format!("var.{}", d.name))
            })
            .map(|d| {
                finding(
                    this,
                    file,
                    Severity::Low,
                    (d.line, d.line),
                    &d.name,
                    format!(
                        "Variable {} is never referenced as `var.{}` in this module; remove it.",
                        code(&d.name),
                        d.name
                    ),
                )
            })
            .collect()
    }
);

graph_rule!(
    UnusedLocal,
    "tf_unused_local",
    Category::Infrastructure,
    "A local value is defined but nothing in its module reads it.",
    [Terraform],
    |this, file, _config, context| {
        let Some(module) = context.graph.and_then(|g| g.modules.get(dir_of(file.path))) else {
            return Vec::new();
        };
        module
            .locals
            .iter()
            .filter(|d| {
                d.path == file.path && !module.references.contains(&format!("local.{}", d.name))
            })
            .map(|d| {
                finding(
                    this,
                    file,
                    Severity::Low,
                    (d.line, d.line),
                    &d.name,
                    format!(
                        "Local {} is never referenced as `local.{}`; remove it.",
                        code(&d.name),
                        d.name
                    ),
                )
            })
            .collect()
    }
);

graph_rule!(
    UnusedData,
    "tf_unused_data_source",
    Category::Infrastructure,
    "A data source is declared but nothing in its module reads it.",
    [Terraform],
    |this, file, _config, context| {
        let Some(module) = context.graph.and_then(|g| g.modules.get(dir_of(file.path))) else {
            return Vec::new();
        };
        module
            .data
            .iter()
            .filter(|d| d.path == file.path && !module.references.contains(&format!("data.{}", d.name)))
            .map(|d| {
                finding(
                    this,
                    file,
                    Severity::Low,
                    (d.line, d.line),
                    &d.name,
                    format!("Data source {} is never read; remove it, or it costs a lookup on every plan.", code(&d.name)),
                )
            })
            .collect()
    }
);
