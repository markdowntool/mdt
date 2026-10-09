//! Markdown outline indentation.
pub fn indent_depth(line: &str) -> usize {
    line.chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .map(|c| if c == '\t' { 2 } else { 1 })
        .sum::<usize>()
        / 2
}
