//! Dockerfile instructions, read line by line (the tree-sitter grammar targets an older
//! tree-sitter than the workspace uses).

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instruction {
    /// Upper-case keyword: `FROM`, `RUN`, ...
    pub keyword: String,
    /// Everything after the keyword, continuations joined with a space.
    pub args: String,
    /// 1-based first and last line.
    pub line: u32,
    pub end_line: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Dockerfile {
    pub instructions: Vec<Instruction>,
}

pub fn parse(text: &str) -> Dockerfile {
    let mut out = Vec::new();
    let mut lines = text.lines().enumerate().peekable();
    while let Some((index, raw)) = lines.next() {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let mut joined = String::new();
        let mut end = index;
        let mut current = trimmed.to_owned();
        loop {
            let continues = current.ends_with('\\');
            if continues {
                current.pop();
            }
            joined.push_str(current.trim());
            joined.push(' ');
            if !continues {
                break;
            }
            // Comments and blank lines inside a continuation are skipped.
            let mut next = None;
            for (i, l) in lines.by_ref() {
                end = i;
                let t = l.trim();
                if t.is_empty() || t.starts_with('#') {
                    continue;
                }
                next = Some(t.to_owned());
                break;
            }
            match next {
                Some(t) => current = t,
                None => break,
            }
        }
        let joined = joined.trim();
        let (keyword, args) = joined
            .split_once(char::is_whitespace)
            .unwrap_or((joined, ""));
        out.push(Instruction {
            keyword: keyword.to_ascii_uppercase(),
            args: args.trim().to_owned(),
            line: index as u32 + 1,
            end_line: end as u32 + 1,
        });
    }
    Dockerfile { instructions: out }
}
