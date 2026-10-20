use harness_infra_analysis::{analyse, classify, InfraConfig, InfraContext, InfraKind, Severity};

/// `(rule, line, severity)` of the findings of `rule` alone.
fn run_rule(rule: &str, path: &str, text: &str) -> Vec<(String, u32, Severity)> {
    analyse(
        path,
        text,
        &InfraContext::default(),
        &InfraConfig::default(),
    )
    .unwrap()
    .1
    .into_iter()
    .filter(|f| f.detector == rule)
    .map(|f| (f.detector, f.start_line, f.severity))
    .collect()
}

#[test]
fn classifies_by_name_and_content() {
    assert_eq!(classify("a/Dockerfile", ""), Some(InfraKind::Dockerfile));
    assert_eq!(classify("infra/main.tf", ""), Some(InfraKind::Terraform));
    assert_eq!(
        classify("live/prod/terragrunt.hcl", ""),
        Some(InfraKind::Terragrunt)
    );
    assert_eq!(
        classify(".github/workflows/ci.yml", ""),
        Some(InfraKind::GithubActions)
    );
    assert_eq!(
        classify("k/deploy.yaml", "apiVersion: v1\nkind: Pod\n"),
        Some(InfraKind::Kubernetes)
    );
    assert_eq!(classify("docs/readme.md", ""), None);
}

#[test]
fn dockerfile_latest_tag() {
    let text = "FROM node:20 AS build\nRUN x\nFROM ubuntu\nFROM build\nFROM alpine:latest\nFROM scratch\nFROM debian@sha256:abc\n";
    assert_eq!(
        run_rule("df_latest_tag", "Dockerfile", text),
        [
            ("df_latest_tag".to_owned(), 3, Severity::Medium),
            ("df_latest_tag".to_owned(), 5, Severity::Medium)
        ]
    );
}

#[test]
fn suppression_comment_silences_a_finding() {
    let text = "# rusty-ignore: df_latest_tag\nFROM ubuntu\n";
    assert!(run_rule("df_latest_tag", "Dockerfile", text).is_empty());
}

#[test]
fn github_actions_unpinned() {
    let text = "on: push\njobs:\n  b:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4\n      - uses: acme/tool@main\n      - uses: acme/other@0123456789abcdef0123456789abcdef01234567\n      - uses: ./local\n";
    assert_eq!(
        run_rule("gha_unpinned_action", ".github/workflows/ci.yml", text),
        [
            ("gha_unpinned_action".to_owned(), 6, Severity::Low),
            ("gha_unpinned_action".to_owned(), 7, Severity::High)
        ]
    );
}

#[test]
fn terraform_unpinned_provider() {
    let text = "terraform {\n  required_providers {\n    aws = {\n      source = \"hashicorp/aws\"\n    }\n    null = {\n      source  = \"hashicorp/null\"\n      version = \"~> 3.0\"\n    }\n  }\n}\n";
    assert_eq!(
        run_rule("tf_unpinned_provider", "main.tf", text),
        [("tf_unpinned_provider".to_owned(), 3, Severity::Medium)]
    );
}

#[test]
fn broken_files_do_not_panic() {
    for (path, text) in [("main.tf", "resource \"x\" {"), ("ci.yml", "a: [")] {
        let _ = analyse(
            path,
            text,
            &InfraContext::default(),
            &InfraConfig::default(),
        );
    }
    let _ = analyse(
        ".github/workflows/x.yml",
        "jobs: [",
        &InfraContext::default(),
        &InfraConfig::default(),
    );
}

#[test]
fn every_rule_has_a_summary_and_scores() {
    let config = harness_infra_analysis::scoring_config();
    for rule in harness_infra_analysis::registry() {
        assert!(!rule.summary().is_empty());
        assert!(config.defect.detectors.iter().any(|d| d == rule.id()));
    }
}

fn lines(rule: &str, path: &str, text: &str) -> Vec<u32> {
    run_rule(rule, path, text)
        .into_iter()
        .map(|f| f.1)
        .collect()
}

#[test]
fn dockerfile_rules() {
    let text = "FROM node:20
ADD app.tar.gz /app
ADD src/ /src
COPY . .
RUN npm ci
RUN apt-get install -y curl
ENV API_TOKEN=abc123
ENV NAME=$FOO
CMD [\"node\"]\n";
    assert_eq!(lines("df_missing_user", "Dockerfile", text), [1]);
    assert_eq!(lines("df_add_instead_of_copy", "Dockerfile", text), [3]);
    assert_eq!(lines("df_cache_order", "Dockerfile", text), [4]);
    assert_eq!(lines("df_apt_no_cleanup", "Dockerfile", text), [6]);
    assert_eq!(lines("df_secret_in_env", "Dockerfile", text), [7]);
    let ok = "FROM node:20
USER app
";
    assert!(lines("df_missing_user", "Dockerfile", ok).is_empty());
}

#[test]
fn github_actions_rules() {
    let text = "on: pull_request_target\njobs:\n  build:\n    runs-on: ubuntu-20.04\n    steps:\n      - uses: actions/checkout@v4\n        with:\n          ref: ${{ github.event.pull_request.head.sha }}\n      - run: echo ${{ github.event.pull_request.title }}\n";
    let path = ".github/workflows/x.yml";
    assert_eq!(lines("gha_pull_request_target_checkout", path, text), [6]);
    assert_eq!(lines("gha_script_injection", path, text), [9]);
    assert_eq!(lines("gha_deprecated_runner", path, text), [4]);
    assert_eq!(lines("gha_missing_timeout", path, text), [3]);
    assert_eq!(lines("gha_broad_permissions", path, text), [1]);
    let wide = "permissions: write-all\njobs: {}\n";
    assert_eq!(lines("gha_broad_permissions", path, wide), [1]);
}

#[test]
fn terraform_rules() {
    let text = "module \"a\" {\n  source = \"git::https://x/y.git\"\n}\nmodule \"b\" {\n  source = \"hashicorp/consul/aws\"\n}\nmodule \"c\" {\n  source = \"./local\"\n}\nvariable \"v\" {\n  type = string\n}\noutput \"o\" {\n  value = 1\n}\nresource \"aws_security_group\" \"sg\" {\n  ingress {\n    cidr_blocks = [\"0.0.0.0/0\"]\n  }\n}\nresource \"null_resource\" \"n\" {\n  provisioner \"local-exec\" {\n    command = \"x\"\n  }\n}\n";
    assert_eq!(lines("tf_unpinned_module", "main.tf", text), [2, 5]);
    assert_eq!(lines("tf_variable_hygiene", "main.tf", text), [10]);
    assert_eq!(lines("tf_output_hygiene", "main.tf", text), [13]);
    assert_eq!(lines("sec_open_ingress", "main.tf", text), [17]);
    assert_eq!(lines("tf_provisioner_use", "main.tf", text), [22]);
}

#[test]
fn kubernetes_rules() {
    let text = "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: web\nspec:\n  template:\n    spec:\n      containers:\n        - name: app\n          image: nginx\n          securityContext:\n            privileged: true\n";
    assert_eq!(lines("k8s_latest_tag", "k/d.yaml", text), [10]);
    assert_eq!(lines("k8s_privileged", "k/d.yaml", text), [12]);
    assert_eq!(lines("k8s_no_limits", "k/d.yaml", text), [9]);
    assert_eq!(lines("k8s_no_probes", "k/d.yaml", text), [9]);
}

fn with_files(rule: &str, path: &str, text: &str, files: &[String]) -> Vec<u32> {
    let context = InfraContext {
        repo_files: Some(files),
        graph: None,
    };
    analyse(path, text, &context, &InfraConfig::default())
        .unwrap()
        .1
        .into_iter()
        .filter(|f| f.detector == rule)
        .map(|f| f.start_line)
        .collect()
}

#[test]
fn terragrunt_rules() {
    let text = "terraform {\n  source = \"git::https://x/y.git//m\"\n}\ninputs = {\n  account = \"123456789012\"\n}\n";
    let path = "live/terragrunt.hcl";
    assert_eq!(lines("tg_unpinned_source", path, text), [2]);
    assert_eq!(lines("iac_hardcoded_account", path, text), [4]);
    assert_eq!(
        lines("tg_missing_remote_state", "root.hcl", "locals {}\n"),
        [1]
    );
    assert!(lines("tg_missing_remote_state", path, "locals {}\n").is_empty());
    let pinned = "terraform {\n  source = \"git::https://x/y.git//m?ref=v1.0.0\"\n}\n";
    assert!(lines("tg_unpinned_source", path, pinned).is_empty());
}

#[test]
fn gitlab_circleci_rules() {
    let gitlab = "image: node:latest\ninclude:\n  - remote: https://x/y.yml\n  - project: g/p\n    file: a.yml\nbuild:\n  image: alpine:3.19\n  script:\n    - echo\nvariables:\n  DB_PASSWORD: hunter2\n";
    assert_eq!(lines("ci_latest_image", ".gitlab-ci.yml", gitlab), [1]);
    assert_eq!(
        lines("gl_unpinned_include", ".gitlab-ci.yml", gitlab),
        [3, 4]
    );
    assert_eq!(lines("sec_secret_literal", ".gitlab-ci.yml", gitlab), [11]);
    let circle = "version: 2.1\norbs:\n  node: circleci/node@volatile\n  ok: circleci/ok@1.2.3\njobs:\n  b:\n    docker:\n      - image: cimg/base\n";
    assert_eq!(
        lines("cci_unpinned_orb", ".circleci/config.yml", circle),
        [3]
    );
    assert_eq!(
        lines("ci_latest_image", ".circleci/config.yml", circle),
        [8]
    );
}

#[test]
fn compose_rules() {
    let text = "services:\n  web:\n    image: nginx\n    privileged: true\n    environment:\n      - SECRET_KEY=abc\n  db:\n    image: postgres:16\n    network_mode: host\n";
    let path = "docker-compose.yml";
    assert_eq!(lines("compose_latest_image", path, text), [3]);
    assert_eq!(lines("compose_privileged", path, text), [4, 9]);
    assert_eq!(lines("sec_secret_literal", path, text), [6]);
}

#[test]
fn ansible_rules() {
    let text = "- hosts: all\n  tasks:\n    - name: ok\n      apt: name=x\n    - shell: echo hi\n    - name: run\n      command: ls\n      args:\n        creates: /x\n    - name: blk\n      block:\n        - copy: src=a dest=b\n";
    let path = "playbooks/site.yml";
    assert_eq!(lines("ans_unnamed_task", path, text), [5, 12]);
    assert_eq!(lines("ans_non_idempotent", path, text), [5]);
}

#[test]
fn shell_rules() {
    let text =
        "#!/bin/bash\ncd /tmp\ncurl -s https://x.sh | bash\necho $HOME\nrm -rf $DIR\na\nb\nc\n";
    assert_eq!(lines("sh_missing_strict_mode", "deploy.sh", text), [1]);
    assert_eq!(lines("sh_curl_pipe_sh", "deploy.sh", text), [3]);
    assert_eq!(lines("sh_unquoted_expansion", "deploy.sh", text), [4, 5]);
    let strict = "#!/bin/bash\nset -euo pipefail\necho \"$HOME\"\na\nb\nc\nd\ne\n";
    assert!(lines("sh_missing_strict_mode", "x.sh", strict).is_empty());
    assert!(lines("sh_unquoted_expansion", "x.sh", strict).is_empty());
}

#[test]
fn makefile_rules() {
    let text = "CC := gcc\nall: build\n\techo\nbuild:\n\techo\n.PHONY: build\nbuild:\n\techo2\n";
    assert_eq!(lines("mk_missing_phony", "Makefile", text), [2]);
    assert_eq!(lines("mk_duplicate_target", "Makefile", text), [7]);
}

#[test]
fn repository_hygiene_rules() {
    let files = vec![
        "main.tf".to_owned(),
        "src/a.rs".to_owned(),
        "docs/x.md".to_owned(),
    ];
    assert_eq!(
        with_files(
            "repo_gitignore_gaps",
            ".gitignore",
            "node_modules\n",
            &files
        ),
        [1, 1, 1]
    );
    assert!(with_files(
        "repo_gitignore_gaps",
        ".gitignore",
        ".env\n.terraform/\n*.tfstate\n",
        &files
    )
    .is_empty());
    assert_eq!(
        with_files(
            "repo_codeowners_stale",
            "CODEOWNERS",
            "/src/ @a\n/gone/ @b\n*.rs @c\n*.py @d\n",
            &files
        ),
        [2, 4]
    );
}

#[test]
fn committed_secret_files() {
    assert_eq!(lines("sec_committed_secret_file", ".env", "A=1\n"), [1]);
    assert_eq!(
        lines("sec_committed_secret_file", "terraform.tfstate", "{}"),
        [1]
    );
    assert_eq!(
        lines(
            "sec_committed_secret_file",
            "k.pem",
            "-----BEGIN RSA PRIVATE KEY-----\n"
        ),
        [1]
    );
    assert!(lines("sec_committed_secret_file", "c.pem", "CERTIFICATE\n").is_empty());
    assert_eq!(classify(".env.example", ""), None);
}

/// Runs `rule` on `target` after building the graph from `files`.
fn with_graph(rule: &str, target: &str, files: &[(&str, &str)], extra: &[&str]) -> Vec<u32> {
    let mut all: Vec<String> = files.iter().map(|f| f.0.to_owned()).collect();
    all.extend(extra.iter().map(|e| (*e).to_owned()));
    let graph = harness_infra_analysis::InfraGraph::build(files, &all);
    let context = InfraContext {
        repo_files: Some(&all),
        graph: Some(&graph),
    };
    let text = files.iter().find(|f| f.0 == target).unwrap().1;
    analyse(target, text, &context, &InfraConfig::default())
        .unwrap()
        .1
        .into_iter()
        .filter(|f| f.detector == rule)
        .map(|f| f.start_line)
        .collect()
}

#[test]
fn terraform_unused_declarations_use_the_whole_module() {
    let main = "variable \"a\" {}\nvariable \"b\" {}\nlocals {\n  x = var.a\n  y = 1\n}\ndata \"aws_x\" \"d\" {}\noutput \"o\" { value = local.x }\n";
    let other = "variable \"c\" {}\n# var.c in a comment does not count\n";
    let files = [("m/main.tf", main), ("m/variables.tf", other)];
    assert_eq!(
        with_graph("tf_unused_variable", "m/main.tf", &files, &[]),
        [2]
    );
    assert_eq!(
        with_graph("tf_unused_variable", "m/variables.tf", &files, &[]),
        [1]
    );
    assert_eq!(with_graph("tf_unused_local", "m/main.tf", &files, &[]), [5]);
    assert_eq!(
        with_graph("tf_unused_data_source", "m/main.tf", &files, &[]),
        [7]
    );
    // A variable read from a sibling file is used.
    let user = "output \"c\" { value = var.c }\n";
    let files = [
        ("m/main.tf", main),
        ("m/variables.tf", other),
        ("m/out.tf", user),
    ];
    assert!(with_graph("tf_unused_variable", "m/variables.tf", &files, &[]).is_empty());
}

#[test]
fn terragrunt_dependencies_and_includes() {
    let a = "dependency \"b\" {\n  config_path = \"../b\"\n}\n";
    let b = "dependency \"a\" {\n  config_path = \"../a\"\n}\ndependency \"x\" {\n  config_path = \"../x\"\n}\n";
    let files = [("live/a/terragrunt.hcl", a), ("live/b/terragrunt.hcl", b)];
    assert_eq!(
        with_graph("tg_dependency_cycle", "live/a/terragrunt.hcl", &files, &[]),
        [2]
    );
    assert_eq!(
        with_graph("tg_dependency_cycle", "live/b/terragrunt.hcl", &files, &[]),
        [2]
    );
    assert_eq!(
        with_graph(
            "tg_missing_dependency",
            "live/b/terragrunt.hcl",
            &files,
            &[]
        ),
        [5]
    );
    assert!(with_graph(
        "tg_missing_dependency",
        "live/a/terragrunt.hcl",
        &files,
        &[]
    )
    .is_empty());

    let hop = |next: &str| format!("include \"x\" {{\n  path = \"{next}\"\n}}\n");
    let unit = hop("../a.terragrunt.hcl");
    let (a, b, c) = (
        hop("./b.terragrunt.hcl"),
        hop("./c.terragrunt.hcl"),
        hop("./d.terragrunt.hcl"),
    );
    let files = [
        ("u/terragrunt.hcl", unit.as_str()),
        ("a.terragrunt.hcl", a.as_str()),
        ("b.terragrunt.hcl", b.as_str()),
        ("c.terragrunt.hcl", c.as_str()),
        ("d.terragrunt.hcl", "locals {}\n"),
    ];
    assert_eq!(
        with_graph("tg_include_depth", "u/terragrunt.hcl", &files, &[]),
        [1]
    );

    let nearest = "include \"root\" {\n  path = find_in_parent_folders()\n}\n";
    let files = [
        ("live/prod/app/terragrunt.hcl", nearest),
        ("live/root.hcl", "locals {}\n"),
    ];
    let all: Vec<String> = files.iter().map(|f| f.0.to_owned()).collect();
    let graph = harness_infra_analysis::InfraGraph::build(&files, &all);
    assert_eq!(
        graph.include_edges["live/prod/app/terragrunt.hcl"],
        ["live/root.hcl"]
    );
}

#[test]
fn local_sources_must_exist() {
    let text =
        "module \"m\" {\n  source = \"./mods/x\"\n}\nmodule \"n\" {\n  source = \"./present\"\n}\n";
    let files = [("main.tf", text)];
    assert_eq!(
        with_graph(
            "iac_missing_local_source",
            "main.tf",
            &files,
            &["present/main.tf"]
        ),
        [2]
    );
}

#[test]
fn workflow_needs_and_local_references() {
    let text =
        "jobs:\n  a:\n    needs: b\n    steps: []\n  b:\n    needs: [a, ghost]\n    steps: []\n";
    let path = ".github/workflows/x.yml";
    assert_eq!(lines("gha_unknown_needs", path, text), [6]);
    assert_eq!(lines("gha_needs_cycle", path, text), [3, 6]);
    let uses = "jobs:\n  a:\n    uses: ./.github/workflows/gone.yml\n  b:\n    uses: ./.github/workflows/there.yml\n";
    let files = vec!["a/b".to_owned(), ".github/workflows/there.yml".to_owned()];
    assert_eq!(
        with_files("gha_missing_local_reference", path, uses, &files),
        [3]
    );
}

#[test]
fn compose_depends_on_cycle() {
    let text = "services:\n  a:\n    depends_on: [b]\n  b:\n    depends_on:\n      - a\n";
    assert_eq!(
        lines("compose_depends_cycle", "docker-compose.yml", text),
        [3, 5]
    );
}

#[test]
fn graph_overview_counts_units_and_cycles() {
    let a = "dependency \"b\" {\n  config_path = \"../b\"\n}\n";
    let b = "dependency \"a\" {\n  config_path = \"../a\"\n}\n";
    let files = [
        ("l/a/terragrunt.hcl", a),
        ("l/b/terragrunt.hcl", b),
        ("m/main.tf", ""),
    ];
    let all: Vec<String> = files.iter().map(|f| f.0.to_owned()).collect();
    let overview = harness_infra_analysis::InfraGraph::build(&files, &all).overview();
    assert_eq!(
        (
            overview.terragrunt_units,
            overview.terraform_modules,
            overview.unit_dependencies,
            overview.units_in_cycles
        ),
        (2, 1, 2, 2)
    );
}
