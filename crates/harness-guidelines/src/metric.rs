//! Size and complexity limits over a file's measurements.
use harness_code_analysis::metrics::FileMetrics;

use crate::config::MetricLimits;

/// One limit a file or function goes over.
pub(crate) struct Excess {
    /// `None` for the file as a whole.
    pub function: Option<String>,
    pub start_line: u32,
    pub end_line: u32,
    pub what: &'static str,
    pub value: u32,
    pub limit: u32,
}

pub(crate) fn excesses(
    limits: &MetricLimits,
    line_count: u32,
    metrics: Option<&FileMetrics>,
) -> Vec<Excess> {
    let mut out = Vec::new();
    if let Some(limit) = limits.max_file_lines.filter(|limit| line_count > *limit) {
        out.push(Excess {
            function: None,
            start_line: 1,
            end_line: line_count,
            what: "lines in the file",
            value: line_count,
            limit,
        });
    }
    let Some(metrics) = metrics else {
        return out;
    };
    for function in metrics.functions.iter().filter(|f| !f.test_code) {
        let checks = [
            ("lines of code", function.nloc, limits.max_function_lines),
            ("cyclomatic complexity", function.ccn, limits.max_complexity),
            ("nesting depth", function.max_nesting, limits.max_nesting),
            ("parameters", function.params, limits.max_params),
        ];
        for (what, value, limit) in checks {
            if let Some(limit) = limit.filter(|limit| value > *limit) {
                out.push(Excess {
                    function: Some(function.qualified_name.clone()),
                    start_line: function.start_line,
                    end_line: function.end_line,
                    what,
                    value,
                    limit,
                });
            }
        }
    }
    out
}
