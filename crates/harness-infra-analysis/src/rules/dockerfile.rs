use harness_code_health::{Category, Finding, Severity};

use crate::{
    doc::dockerfile::Instruction,
    file::InfraFile,
    kind::InfraKind,
    rule::{code, finding, rule, InfraConfig, InfraContext, InfraRule},
};

/// `FROM image` or `FROM image:latest`: the base moves under the build.
pub struct LatestTag;

impl InfraRule for LatestTag {
    fn id(&self) -> &'static str {
        "df_latest_tag"
    }
    fn version(&self) -> u32 {
        1
    }
    fn category(&self) -> Category {
        Category::Infrastructure
    }
    fn summary(&self) -> &'static str {
        "A base image is untagged or tagged latest, so builds change without a code change."
    }
    fn applies(&self, kind: InfraKind) -> bool {
        kind == InfraKind::Dockerfile
    }

    fn check(&self, file: &InfraFile<'_>, _: &InfraContext<'_>, _: &InfraConfig) -> Vec<Finding> {
        let Some(docker) = &file.dockerfile else {
            return Vec::new();
        };
        let mut stages: Vec<String> = Vec::new();
        let mut out = Vec::new();
        for instruction in docker.instructions.iter().filter(|i| i.keyword == "FROM") {
            let mut words = instruction
                .args
                .split_whitespace()
                .filter(|w| !w.starts_with("--"));
            let Some(image) = words.next() else {
                continue;
            };
            if words.next().is_some_and(|w| w.eq_ignore_ascii_case("as")) {
                if let Some(alias) = words.next() {
                    stages.push(alias.to_ascii_lowercase());
                }
            }
            let is_stage = stages.iter().any(|s| *s == image.to_ascii_lowercase());
            if image == "scratch" || image.contains('$') || image.contains('@') || is_stage {
                continue;
            }
            let tag = image
                .rsplit_once(':')
                .filter(|(name, _)| !name.ends_with('/'));
            let floating = match tag {
                None => true,
                Some((_, tag)) => tag.eq_ignore_ascii_case("latest"),
            };
            if floating {
                out.push(finding(
                    self,
                    file,
                    Severity::Medium,
                    (instruction.line, instruction.end_line),
                    image,
                    format!(
                        "Base image {} has no fixed tag, so the build changes when the image does; pin a version or digest.",
                        code(image)
                    ),
                ));
            }
        }
        out
    }
}

fn instructions<'a>(file: &'a InfraFile<'_>) -> &'a [Instruction] {
    file.dockerfile
        .as_ref()
        .map_or(&[], |d| d.instructions.as_slice())
}

rule!(
    MissingUser,
    "df_missing_user",
    Category::Security,
    "The image runs as root because no USER is set in the final stage.",
    [Dockerfile],
    |this, file, _config| {
        let all = instructions(file);
        let last_from = all.iter().rposition(|i| i.keyword == "FROM");
        let Some(start) = last_from else {
            return Vec::new();
        };
        let stage = &all[start..];
        let user = stage.iter().rev().find(|i| i.keyword == "USER");
        let root = user.is_none_or(|u| {
            let name = u.args.split(':').next().unwrap_or("").trim();
            name == "root" || name == "0"
        });
        if !root {
            return Vec::new();
        }
        let at = user.unwrap_or(&all[start]);
        vec![finding(
            this,
            file,
            Severity::Medium,
            (at.line, at.end_line),
            "",
            "The final stage runs as root; add a `USER` with a non-root account before `CMD`/`ENTRYPOINT`.".to_owned(),
        )]
    }
);

rule!(
    AddInsteadOfCopy,
    "df_add_instead_of_copy",
    Category::Infrastructure,
    "ADD is used where COPY is enough, hiding remote fetches and archive extraction.",
    [Dockerfile],
    |this, file, _config| {
        instructions(file)
            .iter()
            .filter(|i| i.keyword == "ADD")
            .filter(|i| {
                let source = i
                    .args
                    .split_whitespace()
                    .find(|w| !w.starts_with("--"))
                    .unwrap_or("");
                let archive = [".tar", ".tar.gz", ".tgz", ".tar.bz2", ".tar.xz"]
                    .iter()
                    .any(|e| source.ends_with(e));
                !archive && !source.starts_with("http://") && !source.starts_with("https://")
            })
            .map(|i| {
                finding(
                    this,
                    file,
                    Severity::Low,
                    (i.line, i.end_line),
                    "",
                    "`ADD` of a local file; use `COPY`, which does nothing surprising.".to_owned(),
                )
            })
            .collect()
    }
);

rule!(
    AptNoCleanup,
    "df_apt_no_cleanup",
    Category::Infrastructure,
    "An apt-get install leaves the package lists in the image layer.",
    [Dockerfile],
    |this, file, _config| {
        instructions(file)
            .iter()
            .filter(|i| i.keyword == "RUN")
            .filter(|i| {
                i.args.contains("apt-get install") || i.args.contains("apt install")
            })
            .filter(|i| !i.args.contains("/var/lib/apt/lists"))
            .map(|i| {
                finding(
                    this,
                    file,
                    Severity::Low,
                    (i.line, i.end_line),
                    "",
                    "apt packages are installed without removing `/var/lib/apt/lists/*` in the same `RUN`, so the layer keeps the lists.".to_owned(),
                )
            })
            .collect()
    }
);

const SECRET_WORDS: [&str; 6] = [
    "PASSWORD",
    "SECRET",
    "TOKEN",
    "API_KEY",
    "APIKEY",
    "PRIVATE_KEY",
];

rule!(
    SecretInEnv,
    "df_secret_in_env",
    Category::Security,
    "A secret-looking ENV or ARG carries a literal value baked into the image.",
    [Dockerfile],
    |this, file, _config| {
        let mut out = Vec::new();
        for i in instructions(file)
            .iter()
            .filter(|i| matches!(i.keyword.as_str(), "ENV" | "ARG"))
        {
            for pair in i.args.split_whitespace() {
                let Some((name, value)) = pair.split_once('=') else {
                    continue;
                };
                let upper = name.to_ascii_uppercase();
                let literal = !value.is_empty() && !value.starts_with('$');
                if literal && SECRET_WORDS.iter().any(|w| upper.contains(w)) {
                    out.push(finding(
                        this,
                        file,
                        Severity::High,
                        (i.line, i.end_line),
                        name,
                        format!(
                            "{} is set to a literal in the image, where anyone who pulls it can read it; pass it at run time or as a build secret.",
                            code(name)
                        ),
                    ));
                }
            }
        }
        out
    }
);

const INSTALLERS: [&str; 6] = [
    "npm install",
    "npm ci",
    "pip install",
    "yarn install",
    "bundle install",
    "go mod download",
];

rule!(
    CacheOrder,
    "df_cache_order",
    Category::Infrastructure,
    "All sources are copied before dependencies are installed, so every edit reinstalls them.",
    [Dockerfile],
    |this, file, _config| {
        let all = instructions(file);
        let mut out = Vec::new();
        for (index, copy) in all.iter().enumerate() {
            let everything = copy.keyword == "COPY"
                && copy
                    .args
                    .split_whitespace()
                    .find(|w| !w.starts_with("--"))
                    .is_some_and(|source| source == "." || source == "./");
            if !everything {
                continue;
            }
            let later = all[index + 1..]
                .iter()
                .take_while(|i| i.keyword != "FROM")
                .find(|i| i.keyword == "RUN" && INSTALLERS.iter().any(|c| i.args.contains(c)));
            if later.is_some() {
                out.push(finding(
                    this,
                    file,
                    Severity::Low,
                    (copy.line, copy.end_line),
                    "",
                    "Everything is copied before dependencies are installed; copy only the manifest and lockfile first so the install layer stays cached.".to_owned(),
                ));
            }
        }
        out
    }
);
