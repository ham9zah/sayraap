//! Size limits. Smallness is enforced by code, not asked of the model.

use similar::{DiffOp, TextDiff};

/// Starting value for programming projects; adjustable from settings later.
pub const DEFAULT_MAX_CHANGED_LINES: usize = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Most lines one task may add, remove or modify.
    pub max_changed_lines: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self { max_changed_lines: DEFAULT_MAX_CHANGED_LINES }
    }
}

/// How many lines differ between two versions of a file.
/// A modified line counts once, not as one removal plus one addition.
pub fn changed_lines(old: &str, new: &str) -> usize {
    TextDiff::from_lines(old, new)
        .ops()
        .iter()
        .map(|op| match *op {
            DiffOp::Equal { .. } => 0,
            DiffOp::Delete { old_len, .. } => old_len,
            DiffOp::Insert { new_len, .. } => new_len,
            DiffOp::Replace { old_len, new_len, .. } => old_len.max(new_len),
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_added_removed_and_modified_lines() {
        assert_eq!(changed_lines("a\nb\nc\n", "a\nb\nc\n"), 0);
        assert_eq!(changed_lines("a\nb\nc\n", "a\nB\nc\n"), 1);
        assert_eq!(changed_lines("a\nc\n", "a\nb1\nb2\nc\n"), 2);
        assert_eq!(changed_lines("a\nb\nc\n", "a\n"), 2);
        assert_eq!(changed_lines("", "x\ny\n"), 2);
    }
}
