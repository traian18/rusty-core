//! `tsconfig.json` / `jsconfig.json` path mapping.
//!
//! Only `compilerOptions.baseUrl`, `compilerOptions.paths`, and relative `extends`
//! chains are read. Files are parsed as data (JSON with comments); nothing is run.
use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::Arc,
};

use harness_workspace::{
    import_graph::ConfigFile,
    repository_intelligence::{DiscoveryDiagnostic, RootRelativePath},
};
use serde_json::Value;

use crate::paths::join_normalized;

const MAX_EXTENDS_DEPTH: usize = 8;

/// The path-mapping options in effect for one config after applying `extends`.
#[derive(Clone, Debug, Default)]
pub(crate) struct CompilerOptions {
    base_url: Option<PathBuf>,
    mappings: Vec<PathMapping>,
    /// Directory of the config that declared `paths`; targets resolve here without a `baseUrl`.
    mappings_dir: PathBuf,
    /// Configs that contributed, nearest first; recorded as resolution evidence.
    pub chain: Vec<RootRelativePath>,
}

#[derive(Clone, Debug)]
struct PathMapping {
    pattern: String,
    targets: Vec<String>,
}

impl CompilerOptions {
    /// Candidate files for `specifier` from the best-matching `paths` pattern, if any matches.
    pub fn path_candidates(&self, specifier: &str) -> Option<Vec<RootRelativePath>> {
        let (mapping, captured) = self.best_mapping(specifier)?;
        let base = self.base_url.as_deref().unwrap_or(&self.mappings_dir);
        Some(
            mapping
                .targets
                .iter()
                .filter_map(|target| join_normalized(base, &target.replacen('*', captured, 1)))
                .collect(),
        )
    }

    /// The file `specifier` names when resolved against `baseUrl`.
    pub fn base_url_candidate(&self, specifier: &str) -> Option<RootRelativePath> {
        join_normalized(self.base_url.as_deref()?, specifier)
    }

    /// An exact pattern wins; otherwise the wildcard pattern with the longest prefix.
    fn best_mapping<'a>(&'a self, specifier: &'a str) -> Option<(&'a PathMapping, &'a str)> {
        self.mappings
            .iter()
            .filter_map(|mapping| {
                let captured = match_pattern(&mapping.pattern, specifier)?;
                Some((mapping, captured))
            })
            .max_by_key(|(mapping, _)| match mapping.pattern.find('*') {
                None => usize::MAX,
                Some(star) => star,
            })
    }
}

/// Returns the text captured by `*`, or an empty string for an exact match.
fn match_pattern<'a>(pattern: &str, specifier: &'a str) -> Option<&'a str> {
    match pattern.split_once('*') {
        None => (pattern == specifier).then_some(""),
        Some((prefix, suffix)) => {
            let rest = specifier.strip_prefix(prefix)?;
            rest.strip_suffix(suffix)
        }
    }
}

struct RawConfig {
    directory: PathBuf,
    base_url: Option<String>,
    paths: Option<Vec<PathMapping>>,
    extends: Vec<String>,
}

/// Effective options for every config that governs a directory, plus parse diagnostics.
pub(crate) struct TsConfigIndex {
    by_directory: HashMap<PathBuf, Arc<CompilerOptions>>,
    pub diagnostics: Vec<DiscoveryDiagnostic>,
}

impl TsConfigIndex {
    pub fn build(configs: &[ConfigFile]) -> Self {
        let mut diagnostics = Vec::new();
        let raw = configs
            .iter()
            .filter_map(|config| match parse_config(config) {
                Ok(parsed) => Some((config.path.clone(), parsed)),
                Err(message) => {
                    diagnostics.push(DiscoveryDiagnostic::for_path(&config.path, message));
                    None
                }
            })
            .collect::<HashMap<_, _>>();
        let mut by_directory = HashMap::new();
        for path in governing_configs(&raw) {
            let mut seen = Vec::new();
            let options = effective_options(&path, &raw, &mut seen, &mut diagnostics);
            by_directory.insert(path.directory().to_path_buf(), Arc::new(options));
        }
        diagnostics.sort_by(|a, b| a.path.cmp(&b.path).then(a.message.cmp(&b.message)));
        diagnostics.dedup();
        Self {
            by_directory,
            diagnostics,
        }
    }

    /// Options from the nearest `tsconfig.json` (or `jsconfig.json`) above `file`.
    pub fn options_for(&self, file: &RootRelativePath) -> Option<&Arc<CompilerOptions>> {
        file.as_path()
            .ancestors()
            .skip(1)
            .find_map(|directory| self.by_directory.get(directory))
    }
}

/// One governing config per directory: `tsconfig.json` wins over `jsconfig.json`.
fn governing_configs(raw: &HashMap<RootRelativePath, RawConfig>) -> Vec<RootRelativePath> {
    let mut chosen: BTreeMap<PathBuf, RootRelativePath> = BTreeMap::new();
    for path in raw.keys() {
        let name = path.as_path().file_name().and_then(|n| n.to_str());
        if !matches!(name, Some("tsconfig.json" | "jsconfig.json")) {
            continue;
        }
        let is_tsconfig = name == Some("tsconfig.json");
        chosen
            .entry(path.directory().to_path_buf())
            .and_modify(|existing| {
                if is_tsconfig {
                    *existing = path.clone();
                }
            })
            .or_insert_with(|| path.clone());
    }
    chosen.into_values().collect()
}

fn effective_options(
    path: &RootRelativePath,
    raw: &HashMap<RootRelativePath, RawConfig>,
    visiting: &mut Vec<RootRelativePath>,
    diagnostics: &mut Vec<DiscoveryDiagnostic>,
) -> CompilerOptions {
    let Some(config) = raw.get(path) else {
        return CompilerOptions::default();
    };
    if visiting.contains(path) || visiting.len() >= MAX_EXTENDS_DEPTH {
        diagnostics.push(DiscoveryDiagnostic::for_path(
            path,
            "tsconfig extends chain is cyclic or too deep",
        ));
        return CompilerOptions::default();
    }
    visiting.push(path.clone());
    let mut options = CompilerOptions::default();
    let mut parent_chains = Vec::new();
    for extended in &config.extends {
        match locate_extended(config, extended, raw) {
            Some(parent) => {
                let inherited = effective_options(&parent, raw, visiting, diagnostics);
                parent_chains.extend(inherited.chain.iter().cloned());
                options.base_url = inherited.base_url.or(options.base_url);
                if !inherited.mappings.is_empty() {
                    options.mappings = inherited.mappings;
                    options.mappings_dir = inherited.mappings_dir;
                }
            }
            None => diagnostics.push(DiscoveryDiagnostic::for_path(
                path,
                format!(
                    "tsconfig extends `{extended}` is not a relative config in this repository"
                ),
            )),
        }
    }
    visiting.pop();
    if let Some(base_url) = &config.base_url {
        options.base_url = join_directory(&config.directory, base_url);
    }
    if let Some(mappings) = &config.paths {
        options.mappings = mappings.clone();
        options.mappings_dir = config.directory.clone();
    }
    options.chain = std::iter::once(path.clone()).chain(parent_chains).collect();
    options
}

/// Resolves a relative `extends` to an inventoried config path.
fn locate_extended(
    config: &RawConfig,
    extended: &str,
    raw: &HashMap<RootRelativePath, RawConfig>,
) -> Option<RootRelativePath> {
    if !(extended.starts_with("./") || extended.starts_with("../")) {
        return None;
    }
    let with_extension = if extended.ends_with(".json") {
        extended.to_owned()
    } else {
        format!("{extended}.json")
    };
    let path = join_normalized(&config.directory, &with_extension)?;
    raw.contains_key(&path).then_some(path)
}

/// `base_url` is "." for the config's own directory, which has no relative-path form.
fn join_directory(directory: &Path, relative: &str) -> Option<PathBuf> {
    if relative == "." || relative == "./" {
        return Some(directory.to_path_buf());
    }
    join_normalized(directory, relative).map(|p| p.as_path().to_path_buf())
}

fn parse_config(config: &ConfigFile) -> Result<RawConfig, String> {
    let value: Value = serde_json::from_str(&strip_jsonc(&config.text))
        .map_err(|error| format!("malformed {}: {error}", file_label(&config.path)))?;
    let options = value.get("compilerOptions");
    let paths = options
        .and_then(|options| options.get("paths"))
        .and_then(Value::as_object)
        .map(|object| {
            object
                .iter()
                .map(|(pattern, targets)| PathMapping {
                    pattern: pattern.clone(),
                    targets: targets
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|t| t.as_str().map(str::to_owned))
                        .collect(),
                })
                .collect()
        });
    let extends = match value.get("extends") {
        Some(Value::String(single)) => vec![single.clone()],
        Some(Value::Array(many)) => many
            .iter()
            .filter_map(|e| e.as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    };
    Ok(RawConfig {
        directory: config.path.directory().to_path_buf(),
        base_url: options
            .and_then(|options| options.get("baseUrl"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        paths,
        extends,
    })
}

fn file_label(path: &RootRelativePath) -> String {
    path.as_path()
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Removes `//` and `/* */` comments and trailing commas, leaving string contents untouched.
pub(crate) fn strip_jsonc(text: &str) -> String {
    let chars = text.chars().collect::<Vec<_>>();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < chars.len() {
        match chars[index] {
            '"' => index = copy_string(&chars, index, &mut out),
            '/' if chars.get(index + 1) == Some(&'/') => {
                while index < chars.len() && chars[index] != '\n' {
                    index += 1;
                }
            }
            '/' if chars.get(index + 1) == Some(&'*') => index = skip_block_comment(&chars, index),
            ',' if closes_container_next(&chars, index + 1) => index += 1,
            other => {
                out.push(other);
                index += 1;
            }
        }
    }
    out
}

/// Copies a JSON string literal verbatim and returns the index after it.
fn copy_string(chars: &[char], start: usize, out: &mut String) -> usize {
    out.push('"');
    let mut index = start + 1;
    while index < chars.len() {
        let character = chars[index];
        out.push(character);
        index += 1;
        match character {
            '\\' if index < chars.len() => {
                out.push(chars[index]);
                index += 1;
            }
            '"' => break,
            _ => {}
        }
    }
    index
}

fn skip_block_comment(chars: &[char], start: usize) -> usize {
    let mut index = start + 2;
    while index + 1 < chars.len() && !(chars[index] == '*' && chars[index + 1] == '/') {
        index += 1;
    }
    (index + 2).min(chars.len())
}

/// True when only whitespace and comments separate `from` and a closing `}` or `]`.
fn closes_container_next(chars: &[char], from: usize) -> bool {
    let mut index = from;
    while index < chars.len() {
        match chars[index] {
            c if c.is_whitespace() => index += 1,
            '/' if chars.get(index + 1) == Some(&'/') => {
                while index < chars.len() && chars[index] != '\n' {
                    index += 1;
                }
            }
            '/' if chars.get(index + 1) == Some(&'*') => index = skip_block_comment(chars, index),
            '}' | ']' => return true,
            _ => return false,
        }
    }
    false
}
