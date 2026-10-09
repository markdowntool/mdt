//! Unified source diff for retained observations.
pub fn diff(before: &str, after: &str) -> String {
    similar::TextDiff::from_lines(before, after)
        .unified_diff()
        .context_radius(3)
        .header("before", "after")
        .to_string()
}
