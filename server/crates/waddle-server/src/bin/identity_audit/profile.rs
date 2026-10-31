use super::model::Finding;

/// A deliberately narrow diagnostic candidate, not an RFC address validator.
/// No Unicode preparation, aliases, A-label decoding or automatic lowercasing.
pub fn candidate_findings(localpart: &str, domain: &str) -> Vec<Finding> {
    let mut findings = Vec::new();
    if !localpart.is_ascii() || !domain.is_ascii() {
        findings.push(Finding::UnicodeInput);
    }
    if localpart.is_empty()
        || localpart.len() > 64
        || !localpart
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"._-".contains(&c))
    {
        findings.push(Finding::CandidateLocalpart);
    }
    if domain.ends_with('.') {
        findings.push(Finding::CandidateTrailingDot);
    }
    if domain
        .split('.')
        .any(|label| label.to_ascii_lowercase().starts_with("xn--"))
    {
        findings.push(Finding::CandidateALabel);
    }
    if domain.len() > 253
        || !domain.contains('.')
        || !domain.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        })
    {
        findings.push(Finding::CandidateDomain);
    }
    findings
}
