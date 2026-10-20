use harness_code_health::{Category, Severity};

use crate::rule::{code, finding, rule};

fn name_of(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn ignored(text: &str, pattern: &str) -> bool {
    text.lines().map(str::trim).any(|l| {
        let l = l.trim_start_matches('/').trim_end_matches('/');
        l == pattern || l == format!("{pattern}*") || l == format!("**/{pattern}")
    })
}

rule!(
    GitignoreGaps,
    "repo_gitignore_gaps",
    Category::Security,
    ".gitignore does not exclude files that hold secrets or generated state.",
    [RepoConfig],
    |this, file, _config| {
        // Only the root file: nested ones add to it.
        if file.path != ".gitignore" {
            return Vec::new();
        }
        let repo = file.text;
        let mut missing = vec![(".env", "environment files with credentials")];
        let has_terraform = file.repo_files.iter().any(|f| f.ends_with(".tf"));
        if has_terraform {
            missing.push((".terraform", "downloaded providers and modules"));
            missing.push(("*.tfstate", "Terraform state with plain-text secrets"));
        }
        missing
            .into_iter()
            .filter(|(pattern, _)| {
                !(ignored(repo, pattern) || *pattern == ".env" && ignored(repo, ".env*"))
            })
            .map(|(pattern, why)| {
                finding(
                    this,
                    file,
                    Severity::Medium,
                    (1, 1),
                    pattern,
                    format!("`.gitignore` does not list {} ({why}).", code(pattern)),
                )
            })
            .collect()
    }
);

/// Whether a CODEOWNERS pattern matches any file, by the rules the file uses (simplified).
fn matches_any(pattern: &str, files: &[String]) -> bool {
    let anchored = pattern.starts_with('/');
    let p = pattern.trim_start_matches('/');
    let directory = p.ends_with('/');
    let p = p.trim_end_matches('/');
    if p.is_empty() || p == "*" || p == "**" {
        return true;
    }
    files.iter().any(|f| {
        if let Some(ext) = p.strip_prefix("*.") {
            return f.ends_with(&format!(".{ext}")) && (!anchored || !f.contains('/'));
        }
        let p = p.trim_end_matches("/**").trim_end_matches("/*");
        let hit = |candidate: &str| {
            candidate == p
                || candidate.starts_with(&format!("{p}/"))
                || (directory && candidate.contains(&format!("/{p}/")))
        };
        if anchored {
            hit(f)
        } else {
            hit(f) || f.contains(&format!("/{p}/")) || f.ends_with(&format!("/{p}"))
        }
    })
}

rule!(
    StaleCodeowners,
    "repo_codeowners_stale",
    Category::Infrastructure,
    "A CODEOWNERS rule matches no file in the repository.",
    [RepoConfig],
    |this, file, _config| {
        if !name_of(file.path).eq_ignore_ascii_case("codeowners") || file.repo_files.is_empty() {
            return Vec::new();
        }
        file.text
            .lines()
            .enumerate()
            .filter_map(|(index, line)| {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    return None;
                }
                let pattern = line.split_whitespace().next()?;
                (!pattern.contains('[') && !matches_any(pattern, file.repo_files)).then(|| {
                    let n = index as u32 + 1;
                    finding(
                        this,
                        file,
                        Severity::Low,
                        (n, n),
                        pattern,
                        format!(
                            "CODEOWNERS pattern {} matches no file; remove it or fix the path.",
                            code(pattern)
                        ),
                    )
                })
            })
            .collect()
    }
);
