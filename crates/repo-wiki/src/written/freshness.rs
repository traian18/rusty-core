//! How far the code has moved since a page was written.
use super::model::{Freshness, InputFile};

/// Fraction of inputs that must be unchanged for a moved page to stay readable.
const KEEP_FRACTION: f64 = 0.5;

/// `Fresh` when the facts digest is unchanged and every input file is as it was; `Stale` when
/// something moved but at least half of the inputs still check out; `Expired` otherwise.
pub fn freshness(
    stored_digest: &str,
    current_digest: &str,
    inputs: &[InputFile],
    hash_of: &dyn Fn(&str) -> Option<String>,
) -> Freshness {
    let unchanged = inputs
        .iter()
        .filter(|i| hash_of(&i.file).as_deref() == Some(i.content_hash.as_str()))
        .count();
    if stored_digest == current_digest && unchanged == inputs.len() {
        return Freshness::Fresh;
    }
    if inputs.is_empty() || (unchanged as f64) / (inputs.len() as f64) >= KEEP_FRACTION {
        Freshness::Stale
    } else {
        Freshness::Expired
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(file: &str, hash: &str) -> InputFile {
        InputFile {
            file: file.into(),
            content_hash: hash.into(),
        }
    }

    fn lookup(file: &str) -> Option<String> {
        match file {
            "a" => Some("1".into()),
            "b" => Some("2".into()),
            _ => None,
        }
    }

    #[test]
    fn unchanged_digest_and_inputs_are_fresh() {
        let inputs = [input("a", "1"), input("b", "2")];
        assert_eq!(freshness("d", "d", &inputs, &lookup), Freshness::Fresh);
    }

    #[test]
    fn a_moved_digest_with_most_inputs_intact_is_stale() {
        let inputs = [input("a", "1"), input("b", "2")];
        assert_eq!(freshness("d", "e", &inputs, &lookup), Freshness::Stale);
        let inputs = [input("a", "1"), input("b", "changed")];
        assert_eq!(freshness("d", "d", &inputs, &lookup), Freshness::Stale);
    }

    #[test]
    fn mostly_changed_or_missing_inputs_expire_the_page() {
        let inputs = [input("a", "old"), input("b", "old"), input("gone", "x")];
        assert_eq!(freshness("d", "d", &inputs, &lookup), Freshness::Expired);
    }
}
