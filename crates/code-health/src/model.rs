use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Low,
    Medium,
    High,
    Critical,
}

/// What a finding is about; scores cap deductions per category.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    /// How the code is shaped: nesting, conditions, cohesion, oversized classes.
    StructuralComplexity,
    /// How big and branchy single functions are.
    SizeAndComplexity,
    Duplication,
    TestCoverage,
    /// How the file has been changed: churn, fixes, ownership.
    Organizational,
    ErrorHandling,
    TestQuality,
    /// How infrastructure files are written: pinning, hygiene, drift, duplication.
    Infrastructure,
    /// Exposure in infrastructure and pipeline files: open access, secrets, broad permissions.
    Security,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// `detector:path:line:name`, stable while the code it points at does not move.
    pub id: String,
    pub detector: String,
    pub detector_version: u32,
    pub category: Category,
    pub severity: Severity,
    pub path: String,
    /// Qualified function name, when the finding is about a function.
    pub function: Option<String>,
    /// Class name, when the finding is about a class.
    pub class: Option<String>,
    /// 1-based, inclusive.
    pub start_line: u32,
    pub end_line: u32,
    /// The numbers the detector judged, by name (for example `ccn`, `limit`).
    pub measured: BTreeMap<String, u32>,
    /// One sentence a developer can act on.
    pub reason: String,
}
