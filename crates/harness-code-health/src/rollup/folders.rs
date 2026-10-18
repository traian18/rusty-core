use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{
    bands::Band,
    repository::{round2, weighted, worst, FileHealth, WorstFile},
};

/// Share of a folder's scored files that make up its worst slice.
const WORST_SLICE: usize = 10;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FolderHealth {
    /// The folder path, or `.` for files at the root.
    pub path: String,
    pub files: usize,
    pub nloc: u64,
    pub average: Option<f64>,
    pub band: Option<Band>,
    /// Mean of the worst tenth of its scored files (at least one): a mean alone hides bad files.
    pub worst_decile: Option<f64>,
    pub worst_file: Option<WorstFile>,
    pub hotspots: usize,
    pub unanalysed: usize,
}

/// The folder a file belongs to when the tree is cut at `depth` levels (`depth` 1: top-level
/// folders). Files above the cut belong to their own folder, root files to `.`.
fn folder_of(path: &str, depth: usize) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    let folders = &parts[..parts.len().saturating_sub(1)];
    if folders.is_empty() {
        ".".to_owned()
    } else {
        folders[..folders.len().min(depth.max(1))].join("/")
    }
}

/// One roll-up per folder at `depth`, worst average first (unscored folders last).
pub fn folder_rollups(files: &[FileHealth], depth: usize) -> Vec<FolderHealth> {
    let mut groups: BTreeMap<String, Vec<&FileHealth>> = BTreeMap::new();
    for file in files {
        groups
            .entry(folder_of(&file.path, depth))
            .or_default()
            .push(file);
    }
    let mut out: Vec<FolderHealth> = groups
        .into_iter()
        .map(|(path, members)| {
            let mut scores: Vec<f64> = members.iter().filter_map(|f| f.defect()).collect();
            scores.sort_by(f64::total_cmp);
            let slice = scores.len().div_ceil(WORST_SLICE).max(1).min(scores.len());
            let worst_decile = (!scores.is_empty())
                .then(|| round2(scores[..slice].iter().sum::<f64>() / slice as f64));
            let average = weighted(members.iter().copied(), FileHealth::defect);
            FolderHealth {
                files: members.len(),
                nloc: members.iter().map(|f| u64::from(f.nloc)).sum(),
                band: average.map(Band::of),
                average,
                worst_decile,
                worst_file: worst(members.iter().copied()),
                hotspots: members.iter().filter(|f| f.hotspot).count(),
                unanalysed: members.iter().filter(|f| f.score.is_none()).count(),
                path,
            }
        })
        .collect();
    out.sort_by(|a, b| match (a.average, b.average) {
        (Some(x), Some(y)) => x.total_cmp(&y).then(a.path.cmp(&b.path)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.path.cmp(&b.path),
    });
    out
}
