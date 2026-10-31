use crate::{
    config::DetectorConfig,
    context::FileContext,
    detector::{code, finding, step, Detector, Draft},
    model::{Category, Finding, Severity},
};

/// Wide signatures of raw values, which usually want a value object.
///
/// Not raised for signatures another declaration sets (trait implementations, overrides), in
/// small files (where a wide entry point is idiomatic), or when most typed parameters are already
/// rich types. Constructors may take a few more. An untyped signature is judged on its count.
pub struct PrimitiveObsession;

impl Detector for PrimitiveObsession {
    fn id(&self) -> &'static str {
        "primitive_obsession"
    }
    fn version(&self) -> u32 {
        1
    }
    fn category(&self) -> Category {
        Category::SizeAndComplexity
    }
    fn summary(&self) -> &'static str {
        "Functions taking many scalar or string parameters."
    }
    fn detect(&self, context: &FileContext<'_>, config: &DetectorConfig) -> Vec<Finding> {
        let c = &config.primitive_obsession;
        if context.metrics.nloc < c.min_file_nloc {
            return Vec::new();
        }
        context
            .metrics
            .functions
            .iter()
            .filter(|f| !f.inherited_signature)
            .filter_map(|f| {
                let limit = c.threshold + if f.constructor { c.constructor_grace } else { 0 };
                if f.params < limit {
                    return None;
                }
                // When types are declared, most of them must be scalars or strings.
                if f.typed_params > 0 && f.primitive_params * 2 <= f.typed_params {
                    return None;
                }
                let severity = step(f.params, Severity::Low, &[(limit + 2, Severity::Medium), (limit + 4, Severity::High)]);
                Some(finding(self, context, Draft {
                    severity,
                    function: Some(f),
                    class: None,
                    lines: (f.start_line, f.start_line),
                    measured: vec![("params", f.params), ("primitive_params", f.primitive_params), ("typed_params", f.typed_params), ("limit", limit)],
                    reason: format!(
                        "{} takes {} parameters{}; group the ones that travel together into a type.",
                        code(&f.qualified_name),
                        f.params,
                        if f.typed_params > 0 { format!(", {} of them scalars or strings", f.primitive_params) } else { String::new() }
                    ),
                }))
            })
            .collect()
    }
}
