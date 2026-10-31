//! The Markdown layout itself: headings, and splitting a reply into its sections.

pub(super) fn heading(level: usize, title: &str) -> String {
    format!("{} {}", "#".repeat(level), title)
}

// ---------------------------------------------------------------- inputs

/// `# `-style heading: (level, title).
pub(super) fn heading_of(line: &str) -> Option<(usize, &str)> {
    let trimmed = line.trim_start();
    let level = trimmed.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = trimmed[level..].strip_prefix(' ')?;
    Some((level, rest.trim()))
}

/// Heading lines outside code fences: (line index, level, title).
pub(super) fn headings(text: &str) -> Vec<(usize, usize, String)> {
    let mut fenced = false;
    let mut found = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if !fenced {
            if let Some((level, title)) = heading_of(line) {
                found.push((index, level, title.to_owned()));
            }
        }
    }
    found
}

pub(super) fn normalize(name: &str) -> String {
    name.trim()
        .trim_matches(|c| matches!(c, '*' | '`' | ':' | '#' | ' '))
        .to_lowercase()
        .replace([' ', '-'], "_")
}

/// Sections opened by headings of exactly `level`: (title, body).
pub(super) fn sections(text: &str, level: usize) -> Vec<(String, String)> {
    let lines: Vec<&str> = text.lines().collect();
    let marks: Vec<_> = headings(text)
        .into_iter()
        .filter(|(_, found, _)| *found == level)
        .collect();
    marks
        .iter()
        .enumerate()
        .map(|(position, (start, _, title))| {
            let end = marks.get(position + 1).map_or(lines.len(), |next| next.0);
            (title.clone(), lines[start + 1..end].join("\n"))
        })
        .collect()
}
