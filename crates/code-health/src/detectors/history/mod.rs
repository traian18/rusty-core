//! Detectors over how a file has been changed. Each reads the file's Git signals and stays
//! silent without them: no history is "no signal", never "no risk".
mod change_entropy;
mod churn_risk;
mod co_change_scatter;
mod developer_congestion;
mod knowledge_loss;
mod ownership_risk;
mod prior_defect;

pub use change_entropy::ChangeEntropy;
pub use churn_risk::ChurnRisk;
pub use co_change_scatter::CoChangeScatter;
pub use developer_congestion::DeveloperCongestion;
pub use knowledge_loss::KnowledgeLoss;
pub use ownership_risk::OwnershipRisk;
pub use prior_defect::PriorDefect;

/// The common header of every history detector.
macro_rules! history_detector {
    ($ty:ident, $id:literal, $summary:literal) => {
        impl crate::detector::Detector for $ty {
            fn id(&self) -> &'static str {
                $id
            }
            fn version(&self) -> u32 {
                1
            }
            fn category(&self) -> crate::model::Category {
                crate::model::Category::Organizational
            }
            fn summary(&self) -> &'static str {
                $summary
            }
            fn detect(
                &self,
                context: &crate::context::FileContext<'_>,
                config: &crate::config::DetectorConfig,
            ) -> Vec<crate::model::Finding> {
                let Some(signals) = context.history else {
                    return Vec::new();
                };
                self.judge(signals, context, config)
                    .map(|draft| vec![crate::detector::finding(self, context, draft)])
                    .unwrap_or_default()
            }
        }
    };
}
pub(crate) use history_detector;
