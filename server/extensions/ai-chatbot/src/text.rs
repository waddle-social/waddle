pub(crate) fn truncate_context_line(input: &str, limit: usize) -> String {
    const SUFFIX: &str = " [truncated]";
    if input.len() <= limit {
        return input.to_string();
    }
    if limit <= SUFFIX.len() {
        return SUFFIX[..limit].to_string();
    }
    let content_limit = limit.saturating_sub(SUFFIX.len());
    let mut out = String::new();
    for ch in input.chars() {
        if out.len() + ch.len_utf8() > content_limit {
            break;
        }
        out.push(ch);
    }
    out.push_str(SUFFIX);
    out
}
