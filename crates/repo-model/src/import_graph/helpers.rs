//! Small building blocks every language crate needs: building facts and running a per-fact
//! resolution over the units of some profiles.
use super::model::{
    ByteSpan, ImportBuildError, ImportFact, ImportFactId, ImportForm, ImportKind, LanguageProfile,
    PathAttribute, Resolution, ResolveContext, ResolveOutput, ResolvedImport, SourceUnit,
    Staticness,
};
use crate::repository_intelligence::RootRelativePath;

/// Builds [`ImportFact`]s for one file with the defaults most languages share (static, runtime,
/// literal, no scope). The id is `path:start:end` plus an optional discriminator, so several facts
/// from one statement (`from m import a, b`) stay distinct.
pub struct FactBuilder<'a> {
    pub path: &'a RootRelativePath,
    pub profile: LanguageProfile,
}

impl FactBuilder<'_> {
    pub fn fact(
        &self,
        span: ByteSpan,
        discriminator: &str,
        spelling: String,
        form: ImportForm,
    ) -> ImportFact {
        ImportFact {
            id: ImportFactId(format!(
                "{}:{}:{}{}{}",
                self.path.as_path().display(),
                span.start,
                span.end,
                if discriminator.is_empty() { "" } else { ":" },
                discriminator
            )),
            importing_file: self.path.clone(),
            spelling,
            form,
            span,
            profile: self.profile,
            staticness: Staticness::Static,
            kind: ImportKind::Runtime,
            scope: Vec::new(),
            imported_name: None,
            alias: None,
            glob: false,
            reexport: false,
            literal_specifier: true,
            path_attribute: PathAttribute::None,
        }
    }
}

/// What a language's rule decided for one fact.
pub struct Decision {
    pub resolution: Resolution,
    pub rule: &'static str,
    pub evidence: Vec<RootRelativePath>,
}

impl Decision {
    pub fn new(resolution: Resolution, rule: &'static str) -> Self {
        Self {
            resolution,
            rule,
            evidence: Vec::new(),
        }
    }
}

/// Runs `decide` over every fact of every unit with one of `profiles`, in unit order, observing
/// cancellation. The language crate keeps all its knowledge inside `decide`.
pub fn resolve_units(
    context: &ResolveContext<'_>,
    profiles: &[LanguageProfile],
    mut decide: impl FnMut(&SourceUnit, &ImportFact) -> Decision,
) -> Result<ResolveOutput, ImportBuildError> {
    let mut output = ResolveOutput::default();
    for unit in context
        .units
        .iter()
        .filter(|u| profiles.contains(&u.profile))
    {
        if context.cancel.is_cancelled() {
            return Err(ImportBuildError::Cancelled);
        }
        for fact in &unit.extracted.facts {
            let decision = decide(unit, fact);
            output.resolved.push(ResolvedImport {
                fact: fact.id.clone(),
                resolution: decision.resolution,
                rule: decision.rule.to_owned(),
                evidence: decision.evidence,
            });
        }
    }
    Ok(output)
}
