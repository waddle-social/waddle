use crate::constants::{AI_COMMAND, WADDLE_MENTION};

pub(crate) fn clean_prompt(prompt: &str) -> String {
    let trimmed = prompt.trim();
    if let Some(without_command) = strip_ai_command(trimmed) {
        return strip_leading_waddle_mention(without_command)
            .unwrap_or(without_command)
            .trim()
            .to_string();
    }
    strip_leading_waddle_mention(trimmed)
        .unwrap_or(trimmed)
        .trim()
        .to_string()
}

fn strip_ai_command(trimmed: &str) -> Option<&str> {
    (has_ai_command_prefix(trimmed)
        && is_command_boundary(trimmed.as_bytes().get(AI_COMMAND.len())))
    .then(|| trimmed.get(AI_COMMAND.len()..).unwrap_or(""))
}

fn has_ai_command_prefix(trimmed: &str) -> bool {
    trimmed
        .get(..AI_COMMAND.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(AI_COMMAND))
}

fn strip_leading_waddle_mention(value: &str) -> Option<&str> {
    let trimmed = value.trim_start();
    trimmed
        .get(..WADDLE_MENTION.len())
        .is_some_and(|mention| mention.eq_ignore_ascii_case(WADDLE_MENTION))
        .then_some(trimmed)
        .filter(|trimmed| is_word_boundary(trimmed.as_bytes().get(WADDLE_MENTION.len())))
        .map(|trimmed| trimmed.get(WADDLE_MENTION.len()..).unwrap_or(""))
}

fn is_command_boundary(next: Option<&u8>) -> bool {
    matches!(next, None | Some(b' ' | b'\t' | b'\r' | b'\n'))
}

fn is_word_boundary(next: Option<&u8>) -> bool {
    !matches!(next, Some(b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_'))
}
