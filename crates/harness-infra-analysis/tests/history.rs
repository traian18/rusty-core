use harness_git_analysis::{Contributor, ContributorId, FileSignals, SignalSummary};
use harness_infra_analysis::history_findings;

fn signals() -> FileSignals {
    FileSignals {
        file: harness_workspace::repository_intelligence::RootRelativePath::new(
            ".github/workflows/ci.yml",
        )
        .unwrap(),
        commits: 30,
        commits_recent: 20,
        lines_changed_recent: 0,
        fix_commits: 4,
        fix_examples: Vec::new(),
        decayed_churn_milli: 0,
        churn_percentile_per_mille: 0,
        hotspot: false,
        change_entropy_milli: 0,
        change_entropy_percentile_per_mille: 0,
        contributors: 1,
        owner: Some(Contributor {
            id: ContributorId("a".into()),
            name: "A".into(),
        }),
        owner_share_per_mille: 1000,
        recent_owner: None,
        recent_owner_share_per_mille: 1000,
        minor_contributors: 0,
        bus_factor: 1,
        age_days: 100,
        idle_days: 1,
        fix_percentile_per_mille: 950,
        recent_contributors: 1,
        owner_recent_share_per_mille: 1000,
        co_change_partners: 0,
        co_change_percentile_per_mille: 0,
    }
}

fn summary() -> SignalSummary {
    SignalSummary {
        version: 1,
        fix_rule_version: 1,
        as_of: Some(0),
        recent_days: 90,
        fix_window_days: 180,
        hotspots: 0,
        active_contributors_recent: 10,
        fix_commits: 0,
    }
}

#[test]
fn a_pipeline_that_keeps_getting_fixed_is_reported() {
    let findings = history_findings(".github/workflows/ci.yml", 80, &signals(), &summary());
    let ids: Vec<&str> = findings.iter().map(|f| f.detector.as_str()).collect();
    assert!(ids.contains(&"prior_defect"), "{ids:?}");
    // No functions, so no structural finding.
    assert!(findings
        .iter()
        .all(|f| f.category == harness_infra_analysis::Category::Organizational));
}

#[test]
fn a_quiet_file_has_no_history_findings() {
    let mut quiet = signals();
    quiet.fix_commits = 0;
    quiet.fix_percentile_per_mille = 0;
    assert!(history_findings("Dockerfile", 20, &quiet, &summary()).is_empty());
}
