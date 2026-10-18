//! Where work is happening: the files changing most in the recent window, the folders that hold
//! them, how much of the code is steady, and the code-health findings on the busy files.
use std::collections::BTreeMap;

use harness_code_analysis::metrics::measure_file;
use harness_code_health::{detect_file, score_file, DetectorConfig, FileContext, ScoringConfig};
use harness_git_analysis::GitSnapshot;
use harness_workspace::repository_intelligence::RootRelativePath;

use super::{evidence::Evidence, excerpts::language_of};
use crate::facts::Facts;

const HOT_FILES: usize = 8;
const HOT_FOLDERS: usize = 5;
/// A file untouched this many days counts as steady.
const STEADY_DAYS: u32 = 90;

pub struct HotFile {
    pub path: String,
    pub commits_recent: u32,
    pub contributors: u32,
    pub owner: Option<String>,
    pub fixes: u32,
    pub hotspot: bool,
    pub health: Option<f64>,
    pub issues: Vec<String>,
}

pub struct Landscape {
    pub recent_days: u32,
    pub hot: Vec<HotFile>,
    pub folders: Vec<(String, u32)>,
    pub steady: usize,
    pub with_history: usize,
}

pub fn build(git: &GitSnapshot, facts: &Facts<'_>, evidence: &mut Evidence<'_>) -> Landscape {
    let signals = &git.index.signals;
    let mut ranked: Vec<_> = signals.iter().filter(|s| s.commits_recent > 0).collect();
    ranked.sort_by(|a, b| {
        b.commits_recent
            .cmp(&a.commits_recent)
            .then(b.decayed_churn_milli.cmp(&a.decayed_churn_milli))
            .then(a.file.cmp(&b.file))
    });
    let mut folders: BTreeMap<String, u32> = BTreeMap::new();
    for s in &ranked {
        let path = s.file.as_path().display().to_string();
        let folder = path.rsplit_once('/').map_or(".", |(d, _)| d).to_owned();
        *folders.entry(folder).or_default() += s.commits_recent;
    }
    let mut folders: Vec<(String, u32)> = folders.into_iter().collect();
    folders.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    folders.truncate(HOT_FOLDERS);
    let hot = ranked
        .into_iter()
        .take(HOT_FILES)
        .map(|s| {
            let path = s.file.as_path().display().to_string();
            let (health, issues) = health_of(&path, s, &git.index.signal_summary, facts, evidence);
            evidence.mention(&path);
            HotFile {
                commits_recent: s.commits_recent,
                contributors: s.recent_contributors,
                owner: s.recent_owner.as_ref().map(|o| o.name.clone()),
                fixes: s.fix_commits,
                hotspot: s.hotspot,
                health,
                issues,
                path,
            }
        })
        .collect();
    Landscape {
        recent_days: git.index.signal_summary.recent_days,
        hot,
        folders,
        steady: signals
            .iter()
            .filter(|s| s.idle_days >= STEADY_DAYS)
            .count(),
        with_history: signals.len(),
    }
}

fn health_of(
    path: &str,
    signals: &harness_git_analysis::FileSignals,
    summary: &harness_git_analysis::SignalSummary,
    facts: &Facts<'_>,
    evidence: &mut Evidence<'_>,
) -> (Option<f64>, Vec<String>) {
    let Some(language) = language_of(path) else {
        return (None, Vec::new());
    };
    let Some(text) = evidence.read(path) else {
        return (None, Vec::new());
    };
    let Ok(metrics) = measure_file(&language, path, text.as_bytes()) else {
        return (None, Vec::new());
    };
    let importers = RootRelativePath::new(path)
        .map(|p| facts.dependent_files(&p).len() as u32)
        .unwrap_or(0);
    let context = FileContext::new(path, &metrics)
        .with_importers(importers, None)
        .with_history(signals, summary);
    let findings = detect_file(&context, &DetectorConfig::default());
    let score = score_file(&findings, &ScoringConfig::default());
    let mut costly: Vec<_> = findings
        .iter()
        .map(|f| {
            (
                score.impacts.get(&f.id).copied().unwrap_or(0.0),
                f.reason.clone(),
            )
        })
        .collect();
    costly.sort_by(|a, b| b.0.total_cmp(&a.0));
    (
        Some(score.defect.score),
        costly.into_iter().take(2).map(|(_, r)| r).collect(),
    )
}

impl Landscape {
    pub fn render(&self) -> String {
        let mut out = format!("Window: the last {} days of history.\n\nBusiest folders (commits touching files in them):\n", self.recent_days);
        for (folder, commits) in &self.folders {
            out.push_str(&format!("- `{folder}`: {commits}\n"));
        }
        out.push_str("\nBusiest files:\n");
        for f in &self.hot {
            out.push_str(&format!(
                "- `{}`: {} recent commits by {} people{}{}{}{}\n",
                f.path,
                f.commits_recent,
                f.contributors,
                f.owner
                    .as_ref()
                    .map(|o| format!(", mostly {o}"))
                    .unwrap_or_default(),
                if f.fixes > 0 {
                    format!(", {} bug fixes", f.fixes)
                } else {
                    String::new()
                },
                if f.hotspot {
                    ", a hotspot (changes often and a lot)"
                } else {
                    ""
                },
                f.health
                    .map(|h| format!(", code health {h:.1} of 10"))
                    .unwrap_or_default(),
            ));
            for issue in &f.issues {
                out.push_str(&format!("  - health finding: {issue}\n"));
            }
        }
        out.push_str(&format!(
            "\nSteady files: {} of {} files with history have not changed in {STEADY_DAYS} days.\n",
            self.steady, self.with_history
        ));
        out
    }
}
