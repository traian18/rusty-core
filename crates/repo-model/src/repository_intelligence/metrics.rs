//! Timing and counts for one refresh. Never contains paths, source text, or manifest values.
use std::time::Duration;

use crate::import_graph::ImportMetrics;

/// Where one [`RepositoryGraph::refresh`](super::RepositoryGraph::refresh) spent its time.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RefreshMetrics {
    pub discovery: Duration,
    pub identity: Duration,
    pub imports: Duration,
    pub groups: Duration,
    pub frameworks: Duration,
    pub publication: Duration,
    pub total: Duration,
    /// Present when import support is registered.
    pub import_details: Option<ImportMetrics>,
}
