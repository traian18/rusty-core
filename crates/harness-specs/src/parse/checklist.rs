//! `checklist.md`: the quality questions the Checklist step asks of the spec, one per line as
//! `- [ ] CHK001 Question? [Dimension, Spec §R2]`, ticked when the spec answers them. Headings,
//! notes and other lines are ignored.

/// One question of the checklist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChecklistItem {
    pub id: String,
    pub line: usize,
    pub done: bool,
}

pub fn parse(text: &str) -> Vec<ChecklistItem> {
    text.lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let rest = line.trim_start().strip_prefix("- [")?;
            let (mark, rest) = rest.split_once(']')?;
            let done = match mark {
                " " => false,
                "x" | "X" => true,
                _ => return None,
            };
            let id = rest.split_whitespace().next()?.trim_end_matches(':');
            let digits = id.strip_prefix("CHK")?;
            (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())).then(|| {
                ChecklistItem {
                    id: id.to_owned(),
                    line: index + 1,
                    done,
                }
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_are_read_with_their_state_and_other_lines_are_ignored() {
        let text = "# Checklist\n\n## Completeness\n- [ ] CHK001 Are limits given? [Gap]\n- [x] CHK002: Is the error shown? [Spec §R1]\n  - [ ] CHK003 nested still counts\n- [ ] T1 not a checklist item\n- [~] CHK004 unknown mark\nNote: done.\n";
        let items = parse(text);
        let ids: Vec<(&str, usize, bool)> = items
            .iter()
            .map(|i| (i.id.as_str(), i.line, i.done))
            .collect();
        assert_eq!(
            ids,
            [
                ("CHK001", 4, false),
                ("CHK002", 5, true),
                ("CHK003", 6, false)
            ]
        );
    }
}
