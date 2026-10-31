//! Reading the status a final answer declares.

/// The status a final answer declares: the value of its last `Status:`
/// line, or of the first line under its last `Status` heading, or the
/// `status` field of an answer that is a JSON object. Lower-cased, stripped
/// of Markdown emphasis, cut at the first separator and with `_` read as a
/// space, so `**Status:** Not done — tests remain` gives `not done` and
/// `blocked_user` gives `blocked user`. Empty when the answer has no status.
pub fn final_status(answer: &str) -> String {
    let unfenced = answer
        .trim()
        .trim_start_matches("```json")
        .trim_matches('`')
        .trim();
    let json_status = serde_json::from_str::<serde_json::Value>(unfenced)
        .ok()
        .and_then(|reply| reply.get("status")?.as_str().map(str::to_string));
    let Some(value) = json_status.or_else(|| status_line(answer)) else {
        return String::new();
    };
    let value = value.to_lowercase();
    let cut = ["—", "–", " - ", ":", ";", ",", ".", "("]
        .iter()
        .filter_map(|separator| value.find(separator))
        .min()
        .unwrap_or(value.len());
    value[..cut]
        .split(|c: char| c.is_whitespace() || matches!(c, '*' | '_' | '`' | '"'))
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The raw value of the last `Status:` line or `Status` heading.
fn status_line(answer: &str) -> Option<String> {
    fn bare(line: &str) -> &str {
        line.trim_matches(|c: char| {
            c.is_whitespace() || matches!(c, '#' | '*' | '_' | '`' | '>' | '-' | '"')
        })
    }
    let lines: Vec<&str> = answer.lines().collect();
    for (index, line) in lines.iter().enumerate().rev() {
        let line = bare(line);
        let Some(rest) = line
            .get(..6)
            .filter(|head| head.eq_ignore_ascii_case("status"))
            .map(|_| bare(&line[6..]))
        else {
            continue;
        };
        if let Some(after) = rest.strip_prefix(':').map(bare) {
            if !after.is_empty() {
                return Some(after.to_string());
            }
        }
        if rest.is_empty() || rest == ":" {
            // A `## Status` heading (or a bare `Status:`): the value is the
            // next non-empty line.
            if let Some(next) = lines[index + 1..]
                .iter()
                .map(|line| bare(line))
                .find(|line| !line.is_empty())
            {
                return Some(next.to_string());
            }
        }
    }
    None
}
