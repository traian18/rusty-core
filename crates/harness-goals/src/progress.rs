//! Whether a pass moved the measurement, and which way.
use crate::{contract::Targets, measurement::Measurement};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    Better,
    Same,
    Worse,
}

/// Score changes smaller than this are noise.
const SCORE_EPSILON: f64 = 0.05;

/// A check that passed fails now, or there are more guideline errors: worse whatever else moved.
/// A check that did not run proves nothing, so it never makes a pass worse.
pub fn broke(before: &Measurement, now: &Measurement) -> bool {
    let check = now
        .failing_checks()
        .filter(|c| c.ran)
        .any(|c| before.checks.iter().any(|b| b.name == c.name && b.passed));
    check || now.guideline_errors.len() > before.guideline_errors.len()
}

/// `now` against `before`. A check that passed and now fails, or more guideline errors, is worse
/// whatever the scores say; then fewer unmet targets, fewer failing checks or errors, and a
/// higher mean score count as better.
pub fn compare(before: &Measurement, now: &Measurement, targets: &Targets) -> Progress {
    if !before.fingerprint.is_empty() && before.fingerprint == now.fingerprint {
        return Progress::Same;
    }
    if broke(before, now) {
        return Progress::Worse;
    }
    let unmet = (before.unmet(targets).len(), now.unmet(targets).len());
    let failing = (
        before.failing_checks().count(),
        now.failing_checks().count(),
    );
    let errors = (before.guideline_errors.len(), now.guideline_errors.len());
    let work = (before.work(targets).len(), now.work(targets).len());
    if unmet.1 < unmet.0 || failing.1 < failing.0 || errors.1 < errors.0 || work.1 < work.0 {
        return Progress::Better;
    }
    // Over a large scope one file fixed hardly moves the mean; how far the files are below the
    // minimum, added up, does.
    if let Some(min) = targets.work_bar() {
        let shortfall = |m: &Measurement| m.scored().map(|(_, s)| (min - s).max(0.0)).sum::<f64>();
        if shortfall(now) < shortfall(before) - SCORE_EPSILON {
            return Progress::Better;
        }
    }
    let delta = match (before.mean_score(), now.mean_score()) {
        (Some(a), Some(b)) => b - a,
        _ => 0.0,
    };
    let worst = match (before.worst_score(), now.worst_score()) {
        (Some(a), Some(b)) => b - a,
        _ => 0.0,
    };
    if delta < -SCORE_EPSILON || worst < -SCORE_EPSILON {
        Progress::Worse
    } else if delta > SCORE_EPSILON || worst > SCORE_EPSILON {
        Progress::Better
    } else {
        let findings = |m: &Measurement| m.files.iter().map(|f| f.findings.len()).sum::<usize>();
        match findings(now).cmp(&findings(before)) {
            std::cmp::Ordering::Less => Progress::Better,
            std::cmp::Ordering::Equal => Progress::Same,
            std::cmp::Ordering::Greater => Progress::Worse,
        }
    }
}
