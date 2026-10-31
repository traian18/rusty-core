#![warn(clippy::all)]

//! Code health: deterministic detectors over a file's measurements, each finding with a severity,
//! a category, the numbers it was judged on, and a reason a developer can read.
//!
//! | Module | Responsibility |
//! |---|---|
//! | `model` | `Finding`, `Severity`, `Category` |
//! | `context` | What a detector is given about one file |
//! | `config` | Thresholds and severity steps, per detector, with defaults |
//! | `detector` | The `Detector` trait and helpers shared by detectors |
//! | `detectors` | One module per detector, and the registry |
//! | `scoring` | Findings to defect and maintainability scores |
//! | `rollup` | Bands, repository and folder numbers from file scores |
//!
//! No model is used. Detectors only say what they observed; `scoring` turns findings into scores.
mod config;
mod context;
mod detector;
pub mod detectors;
pub mod duplication;
mod model;
pub mod rollup;
pub mod scoring;
pub mod trend;

pub use config::{
    BrainMethodConfig, BumpyRoadConfig, ChangeEntropyConfig, ChurnRiskConfig,
    CoChangeScatterConfig, ComplexConditionalConfig, ComplexMethodConfig, DetectorConfig,
    DeveloperCongestionConfig, DuplicatedCodeConfig, GodClassConfig, KnowledgeLossConfig,
    LargeMethodConfig, LowCohesionConfig, NestedComplexityConfig, OwnershipRiskConfig,
    PanicProneConfig, PrimitiveObsessionConfig, PriorDefectConfig, SkippedTestsConfig,
    SwallowedErrorConfig, TestWithoutAssertionsConfig, UntestedCodeConfig,
};
pub use context::{Duplicate, DuplicateSite, FileContext, TestLinks};
pub use detector::Detector;
pub use detectors::{detect_file, registry};
pub use model::{Category, Finding, Severity};
pub use rollup::{
    folder_rollups, repository_health, self_check, Band, FileHealth, FolderHealth,
    RepositoryHealth, RollupConfig, SelfCheck, SelfCheckConfig,
};
pub use scoring::{score_file, DimensionScore, FileScore, ScoringConfig};
