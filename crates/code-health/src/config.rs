//! Thresholds and severity steps for every detector. Defaults follow the published reference
//! values (see `docs/design/code-health-reference-analysis.md`); every field can be overridden,
//! and the digest of the whole configuration goes into anything computed from it.
use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NestedComplexityConfig {
    /// Nesting depth that fires (medium).
    pub threshold: u32,
    pub high: u32,
    pub critical: u32,
}

impl Default for NestedComplexityConfig {
    fn default() -> Self {
        Self {
            threshold: 4,
            high: 5,
            critical: 7,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ComplexConditionalConfig {
    /// Boolean operators in one expression that fire (low).
    pub threshold: u32,
    pub medium: u32,
    pub high: u32,
    pub critical: u32,
}

impl Default for ComplexConditionalConfig {
    fn default() -> Self {
        Self {
            threshold: 3,
            medium: 4,
            high: 5,
            critical: 6,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GodClassConfig {
    pub min_nloc: u32,
    pub min_methods: u32,
    /// A method this long and this complex is the "brain" a god class hides.
    pub brain_nloc: u32,
    pub brain_ccn: u32,
    /// High when either is reached.
    pub high_nloc: u32,
    pub high_methods: u32,
    /// Critical when both are reached.
    pub critical_nloc: u32,
    pub critical_methods: u32,
}

impl Default for GodClassConfig {
    fn default() -> Self {
        Self {
            min_nloc: 200,
            min_methods: 15,
            brain_nloc: 70,
            brain_ccn: 9,
            high_nloc: 300,
            high_methods: 20,
            critical_nloc: 400,
            critical_methods: 25,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BrainMethodConfig {
    pub min_nloc: u32,
    pub min_ccn: u32,
    /// The importers a file needs: `min(max_floor, max(repo p80, min_floor))`, or `max_floor`
    /// when the repository percentile is unknown.
    pub max_floor: u32,
    pub min_floor: u32,
    /// High when either is reached.
    pub high_ccn: u32,
    pub high_nloc: u32,
    /// Critical when both are reached.
    pub critical_ccn: u32,
    pub critical_nloc: u32,
}

impl Default for BrainMethodConfig {
    fn default() -> Self {
        Self {
            min_nloc: 70,
            min_ccn: 9,
            max_floor: 8,
            min_floor: 3,
            high_ccn: 14,
            high_nloc: 120,
            critical_ccn: 20,
            critical_nloc: 150,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LowCohesionConfig {
    pub min_lcom4: u32,
    pub min_methods: u32,
    /// High when either is reached.
    pub high_lcom4: u32,
    pub high_methods: u32,
    /// Critical when both are reached.
    pub critical_lcom4: u32,
    pub critical_methods: u32,
}

impl Default for LowCohesionConfig {
    fn default() -> Self {
        Self {
            min_lcom4: 2,
            min_methods: 5,
            high_lcom4: 3,
            high_methods: 20,
            critical_lcom4: 4,
            critical_methods: 15,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BumpyRoadConfig {
    /// Bumps that fire (low).
    pub threshold: u32,
    /// A function simpler than this is not a road worth flattening.
    pub min_ccn: u32,
    pub medium: u32,
    pub high: u32,
}

impl Default for BumpyRoadConfig {
    fn default() -> Self {
        Self {
            threshold: 3,
            min_ccn: 5,
            medium: 4,
            high: 5,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ComplexMethodConfig {
    /// CCN that fires (medium).
    pub threshold: u32,
    pub high: u32,
    pub critical: u32,
}

impl Default for ComplexMethodConfig {
    fn default() -> Self {
        Self {
            threshold: 9,
            high: 15,
            critical: 25,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LargeMethodConfig {
    /// NLOC that fires (low).
    pub threshold: u32,
    /// Straight-line code (tables, builders) below this CCN is long but not hard.
    pub min_ccn: u32,
    pub medium: u32,
    pub high: u32,
    pub critical: u32,
}

impl Default for LargeMethodConfig {
    fn default() -> Self {
        Self {
            threshold: 60,
            min_ccn: 3,
            medium: 90,
            high: 120,
            critical: 200,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SwallowedErrorConfig {
    /// Swallowed errors in one function that make it medium, then high.
    pub medium: u32,
    pub high: u32,
}

impl Default for SwallowedErrorConfig {
    fn default() -> Self {
        Self { medium: 2, high: 4 }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PanicProneConfig {
    /// Calls that abort instead of handling, in one function, that fire (low), then medium, high.
    pub threshold: u32,
    pub medium: u32,
    pub high: u32,
}

impl Default for PanicProneConfig {
    fn default() -> Self {
        Self {
            threshold: 5,
            medium: 10,
            high: 20,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DuplicatedCodeConfig {
    /// Functions smaller than this (lines of code, syntax nodes) are not compared.
    pub min_nloc: u32,
    pub min_nodes: u32,
    pub medium_nloc: u32,
    pub high_nloc: u32,
    /// Places a function exists in (itself included) that raise the severity one step.
    pub many_copies: u32,
}

impl Default for DuplicatedCodeConfig {
    fn default() -> Self {
        Self {
            min_nloc: 6,
            min_nodes: 40,
            medium_nloc: 15,
            high_nloc: 30,
            many_copies: 3,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UntestedCodeConfig {
    /// A file this big, or with this much branching in total, should have tests.
    pub min_nloc: u32,
    pub min_ccn: u32,
    /// Silent when the repository has fewer test files than this: no tests is no signal.
    pub min_repo_tests: u32,
}

impl Default for UntestedCodeConfig {
    fn default() -> Self {
        Self {
            min_nloc: 50,
            min_ccn: 20,
            min_repo_tests: 3,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TestWithoutAssertionsConfig {
    /// Tests shorter than this are smoke tests and are left alone.
    pub min_nloc: u32,
    pub medium_nloc: u32,
}

impl Default for TestWithoutAssertionsConfig {
    fn default() -> Self {
        Self {
            min_nloc: 3,
            medium_nloc: 10,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SkippedTestsConfig {
    /// Skipped or ignored tests in one file that fire (low), then medium.
    pub threshold: u32,
    pub medium: u32,
}

impl Default for SkippedTestsConfig {
    fn default() -> Self {
        Self {
            threshold: 3,
            medium: 8,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PrimitiveObsessionConfig {
    /// Parameters that fire (low); medium two above, high four above.
    pub threshold: u32,
    /// Extra parameters a constructor may take first.
    pub constructor_grace: u32,
    /// Files smaller than this are utilities whose wide signatures are idiomatic.
    pub min_file_nloc: u32,
}

impl Default for PrimitiveObsessionConfig {
    fn default() -> Self {
        Self {
            threshold: 5,
            constructor_grace: 2,
            min_file_nloc: 60,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DetectorConfig {
    pub nested_complexity: NestedComplexityConfig,
    pub complex_conditional: ComplexConditionalConfig,
    pub god_class: GodClassConfig,
    pub brain_method: BrainMethodConfig,
    pub low_cohesion: LowCohesionConfig,
    pub bumpy_road: BumpyRoadConfig,
    pub complex_method: ComplexMethodConfig,
    pub large_method: LargeMethodConfig,
    pub primitive_obsession: PrimitiveObsessionConfig,
    pub change_entropy: ChangeEntropyConfig,
    pub ownership_risk: OwnershipRiskConfig,
    pub churn_risk: ChurnRiskConfig,
    pub prior_defect: PriorDefectConfig,
    pub developer_congestion: DeveloperCongestionConfig,
    pub knowledge_loss: KnowledgeLossConfig,
    pub co_change_scatter: CoChangeScatterConfig,
    pub swallowed_error: SwallowedErrorConfig,
    pub panic_prone: PanicProneConfig,
    pub duplicated_code: DuplicatedCodeConfig,
    pub untested_code: UntestedCodeConfig,
    pub test_without_assertions: TestWithoutAssertionsConfig,
    pub skipped_tests: SkippedTestsConfig,
    /// Detector ids switched off.
    pub disabled: BTreeSet<String>,
}

impl DetectorConfig {
    /// Stable digest of every setting.
    pub fn digest(&self) -> String {
        use sha2::{Digest, Sha256};
        let bytes = serde_json::to_vec(self).unwrap_or_default();
        format!("{:x}", Sha256::digest(&bytes))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChangeEntropyConfig {
    /// Entropy percentile (thousandths) that fires (medium).
    pub percentile: u32,
    pub high_percentile: u32,
    /// Critical when also a hotspot.
    pub critical_percentile: u32,
    pub min_recent_commits: u32,
}

impl Default for ChangeEntropyConfig {
    fn default() -> Self {
        Self {
            percentile: 800,
            high_percentile: 900,
            critical_percentile: 950,
            min_recent_commits: 3,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OwnershipRiskConfig {
    pub min_commits: u32,
    /// Minor contributors (each under 5% of commits) that fire (medium).
    pub minor_contributors: u32,
    /// Or a main author below this share (thousandths) fires (low).
    pub owner_share_floor: u32,
    pub high_minor: u32,
    /// Critical when also a hotspot.
    pub critical_minor: u32,
    /// Files smaller than this stay low.
    pub small_nloc: u32,
}

impl Default for OwnershipRiskConfig {
    fn default() -> Self {
        Self {
            min_commits: 5,
            minor_contributors: 3,
            owner_share_floor: 400,
            high_minor: 5,
            critical_minor: 6,
            small_nloc: 40,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChurnRiskConfig {
    pub min_recent_commits: u32,
    /// Churn percentile (thousandths).
    pub percentile: u32,
    /// Recent lines changed per line of code, in thousandths: 1000 rewrote its own size.
    pub relative: u32,
    pub medium_relative: u32,
    pub high_relative: u32,
    /// Critical when also a hotspot.
    pub critical_relative: u32,
}

impl Default for ChurnRiskConfig {
    fn default() -> Self {
        Self {
            min_recent_commits: 5,
            percentile: 750,
            relative: 1000,
            medium_relative: 1500,
            high_relative: 2500,
            critical_relative: 4000,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PriorDefectConfig {
    /// Fix-count percentile (thousandths) the file must reach.
    pub percentile: u32,
    pub medium: u32,
    /// High, or critical on a hotspot.
    pub high: u32,
    pub critical: u32,
}

impl Default for PriorDefectConfig {
    fn default() -> Self {
        Self {
            percentile: 800,
            medium: 2,
            high: 3,
            critical: 5,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeveloperCongestionConfig {
    /// Recent authors that fire (low).
    pub contributors: u32,
    pub percentile: u32,
    pub min_recent_commits: u32,
    /// No main author at or above this share (thousandths).
    pub owner_share: u32,
    /// Medium when either is reached.
    pub medium_contributors: u32,
    pub medium_commits: u32,
    /// High when both are reached.
    pub high_contributors: u32,
    pub high_commits: u32,
}

impl Default for DeveloperCongestionConfig {
    fn default() -> Self {
        Self {
            contributors: 5,
            percentile: 750,
            min_recent_commits: 3,
            owner_share: 500,
            medium_contributors: 7,
            medium_commits: 12,
            high_contributors: 10,
            high_commits: 20,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KnowledgeLossConfig {
    /// The main author's share of recent commits (thousandths) below which they count as quiet.
    pub quiet_share: u32,
    /// Teams this small (active authors) keep the finding low unless the file is a hotspot.
    pub small_team: u32,
}

impl Default for KnowledgeLossConfig {
    fn default() -> Self {
        Self {
            quiet_share: 200,
            small_team: 3,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CoChangeScatterConfig {
    /// Co-change partner percentile (thousandths) that fires (medium).
    pub percentile: u32,
    pub high_percentile: u32,
    pub min_recent_commits: u32,
}

impl Default for CoChangeScatterConfig {
    fn default() -> Self {
        Self {
            percentile: 800,
            high_percentile: 950,
            min_recent_commits: 3,
        }
    }
}
