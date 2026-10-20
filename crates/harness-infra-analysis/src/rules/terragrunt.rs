use harness_code_health::{Category, Severity};

use crate::{
    doc::hcl::Block,
    graph::{cycles, dir_of, join},
    rule::{code, finding, graph_rule, rule},
};

fn git_ref(source: &str) -> Option<&str> {
    source
        .split("ref=")
        .nth(1)
        .map(|r| r.split(['&', '?']).next().unwrap_or(r))
}

rule!(
    UnpinnedSource,
    "tg_unpinned_source",
    Category::Infrastructure,
    "A Terragrunt unit takes its Terraform module from a moving ref or an unversioned registry source.",
    [Terragrunt],
    |this, file, _config| {
        let Some(hcl) = &file.hcl else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for block in hcl.body.blocks_of("terraform") {
            let Some(source) = block.body.attr("source") else {
                continue;
            };
            let Some(value) = source.literal() else {
                continue;
            };
            let local = value.starts_with("./") || value.starts_with("../") || value.starts_with('/');
            if local {
                continue;
            }
            let git = value.contains("git::") || value.contains("github.com") || value.contains(".git");
            let moving = if git {
                git_ref(value).is_none_or(|r| matches!(r, "main" | "master" | "develop" | "HEAD"))
            } else if value.starts_with("tfr://") {
                !value.contains("version=")
            } else {
                false
            };
            if moving {
                out.push(finding(
                    this,
                    file,
                    Severity::Medium,
                    (source.line, source.line),
                    "",
                    format!(
                        "Unit source {} does not pin a version; use a tag or commit (`?ref=v1.2.3`) or `?version=`.",
                        code(value)
                    ),
                ));
            }
        }
        out
    }
);

/// 12-digit runs not part of a longer number, the shape of an AWS account id.
fn account_ids(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut found = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i - start == 12 {
                found += 1;
            }
        } else {
            i += 1;
        }
    }
    found
}

fn scan_accounts(block: &Block, out: &mut Vec<(u32, String)>) {
    for attr in &block.body.attrs {
        if attr.name != "description" && account_ids(&attr.value) > 0 {
            out.push((attr.line, attr.name.clone()));
        }
    }
    for inner in &block.body.blocks {
        scan_accounts(inner, out);
    }
}

rule!(
    HardcodedAccount,
    "iac_hardcoded_account",
    Category::Infrastructure,
    "A 12-digit cloud account id is written into the configuration instead of coming from a variable or data source.",
    [Terraform, Terragrunt],
    |this, file, _config| {
        let Some(hcl) = &file.hcl else {
            return Vec::new();
        };
        let mut found = Vec::new();
        for attr in &hcl.body.attrs {
            if account_ids(&attr.value) > 0 {
                found.push((attr.line, attr.name.clone()));
            }
        }
        for block in &hcl.body.blocks {
            // Variable defaults and descriptions are where an id is documented, not used.
            if block.kind != "variable" {
                scan_accounts(block, &mut found);
            }
        }
        found
            .into_iter()
            .map(|(line, name)| {
                finding(
                    this,
                    file,
                    Severity::Low,
                    (line, line),
                    &name,
                    format!("{} contains a hardcoded account id; take it from a variable, `data.aws_caller_identity` or a Terragrunt `locals` file.", code(&name)),
                )
            })
            .collect()
    }
);

rule!(
    MissingRemoteState,
    "tg_missing_remote_state",
    Category::Infrastructure,
    "A root Terragrunt file configures no remote state.",
    [Terragrunt],
    |this, file, _config| {
        let Some(hcl) = &file.hcl else {
            return Vec::new();
        };
        let name = file.path.rsplit('/').next().unwrap_or("");
        // Only a root file (`root.hcl`) is expected to own the state configuration.
        if name != "root.hcl" {
            return Vec::new();
        }
        let configured = hcl.body.blocks_of("remote_state").next().is_some()
            || hcl
                .body
                .blocks_of("generate")
                .any(|g| g.labels.first().is_some_and(|l| l == "backend"));
        if configured {
            return Vec::new();
        }
        vec![finding(
            this,
            file,
            Severity::Medium,
            (1, 1),
            "",
            "This root configuration defines no `remote_state`, so each unit keeps local state."
                .to_owned(),
        )]
    }
);

graph_rule!(
    DependencyCycle,
    "tg_dependency_cycle",
    Category::Infrastructure,
    "Terragrunt units depend on each other in a loop, so none can be applied first.",
    [Terragrunt],
    |this, file, _config, context| {
        let Some(graph) = context.graph else {
            return Vec::new();
        };
        let dir = dir_of(file.path);
        let Some(unit) = graph.units.get(dir) else {
            return Vec::new();
        };
        let edges = graph
            .units
            .iter()
            .map(|(d, u)| {
                let targets = u
                    .dependencies
                    .iter()
                    .filter_map(|dep| dep.target.clone())
                    .collect();
                (d.clone(), targets)
            })
            .collect();
        let Some(component) = cycles(&edges).into_iter().find(|c| c.contains(dir)) else {
            return Vec::new();
        };
        unit.dependencies
            .iter()
            .filter(|dep| dep.target.as_ref().is_some_and(|t| component.contains(t)))
            .map(|dep| {
                finding(
                    this,
                    file,
                    Severity::High,
                    (dep.line, dep.line),
                    &dep.written,
                    format!("This unit and {} depend on each other (directly or through other units); break the loop by moving shared outputs into a third unit.", code(&dep.written)),
                )
            })
            .collect()
    }
);

graph_rule!(
    MissingDependency,
    "tg_missing_dependency",
    Category::Infrastructure,
    "A Terragrunt dependency points at a directory that is not a unit.",
    [Terragrunt],
    |this, file, _config, context| {
        let Some(graph) = context.graph else {
            return Vec::new();
        };
        let Some(unit) = graph.units.get(dir_of(file.path)) else {
            return Vec::new();
        };
        unit.dependencies
            .iter()
            .filter(|dep| {
                dep.target
                    .as_ref()
                    .is_some_and(|t| !graph.units.contains_key(t))
            })
            .map(|dep| {
                finding(
                    this,
                    file,
                    Severity::Medium,
                    (dep.line, dep.line),
                    &dep.written,
                    format!(
                        "Dependency {} has no `terragrunt.hcl` in that directory.",
                        code(&dep.written)
                    ),
                )
            })
            .collect()
    }
);

graph_rule!(
    IncludeDepth,
    "tg_include_depth",
    Category::Infrastructure,
    "Terragrunt files include each other in a deep chain, which hides where a value comes from.",
    [Terragrunt],
    |this, file, config, context| {
        let Some(graph) = context.graph else {
            return Vec::new();
        };
        let limit = config.limit("tg_include_depth", "depth", 3);
        let depth = graph.include_depth(file.path);
        if depth <= limit {
            return Vec::new();
        }
        vec![finding(
            this,
            file,
            Severity::Low,
            (1, 1),
            "",
            format!("Includes are {depth} files deep (limit {limit}); flatten the chain so a reader can find a setting in one hop."),
        )]
    }
);

graph_rule!(
    MissingLocalSource,
    "iac_missing_local_source",
    Category::Infrastructure,
    "A module or unit source is a local path that holds no files.",
    [Terraform, Terragrunt],
    |this, file, _config, context| {
        let Some(graph) = context.graph else {
            return Vec::new();
        };
        let Some(hcl) = &file.hcl else {
            return Vec::new();
        };
        let mut sources = Vec::new();
        for block in hcl
            .body
            .blocks_of("module")
            .chain(hcl.body.blocks_of("terraform"))
        {
            if let Some(attr) = block.body.attr("source") {
                sources.push(attr);
            }
        }
        sources
            .into_iter()
            .filter_map(|attr| {
                let value = attr.literal()?;
                if !(value.starts_with("./") || value.starts_with("../")) || value.contains('$') {
                    return None;
                }
                let target = join(dir_of(file.path), value)?;
                (!graph.has_files_under(&target)).then(|| {
                    finding(
                        this,
                        file,
                        Severity::Medium,
                        (attr.line, attr.line),
                        value,
                        format!(
                            "Source {} is not a directory with files in this repository.",
                            code(value)
                        ),
                    )
                })
            })
            .collect()
    }
);
