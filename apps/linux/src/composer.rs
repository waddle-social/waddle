//! What the composer understands beyond plain text: `@` mentions and
//! slash commands, matching the Apple client's built-ins.
//!
//! Commands add no wire shapes. `/me` sends an XEP-0245 body, `/shrug`
//! ordinary text, and `/away`, `/active` and `/dnd` set RFC 6121 presence
//! `show`. Mentions stay body text; the send builder turns each typed
//! `@label` it can resolve into an XEP-0372 reference.
//!
//! Positions are in characters, as `gtk::Editable` counts them.

/// XEP-0245: a receiver matches exactly this at the start of the body.
pub const ME_PREFIX: &str = "/me ";
const SHRUG: &str = "¯\\_(ツ)_/¯";
/// The most suggestions the popover lists at once.
const MAX_SUGGESTIONS: usize = 8;

/// Room broadcasts, as the send builder names them.
const BROADCASTS: [(&str, &str); 2] = [
    ("everyone", "Notify everyone in this room"),
    ("here", "Notify everyone online now"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Me,
    Shrug,
    Away,
    Active,
    Dnd,
}

impl Command {
    pub const ALL: [Self; 5] = [Self::Me, Self::Shrug, Self::Away, Self::Active, Self::Dnd];

    pub fn name(self) -> &'static str {
        match self {
            Self::Me => "me",
            Self::Shrug => "shrug",
            Self::Away => "away",
            Self::Active => "active",
            Self::Dnd => "dnd",
        }
    }

    fn usage(self) -> String {
        match self {
            Self::Me => "/me <action>".to_string(),
            Self::Shrug => "/shrug [message]".to_string(),
            Self::Away | Self::Active | Self::Dnd => format!("/{}", self.name()),
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Me => "Send an action message, e.g. /me waves",
            Self::Shrug => "Append ¯\\_(ツ)_/¯ to your message",
            Self::Away => "Set your status to Away",
            Self::Active => "Set your status to Available",
            Self::Dnd => "Set your status to Do Not Disturb",
        }
    }

    /// Case-insensitive lookup by name.
    fn named(word: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|command| command.name().eq_ignore_ascii_case(word))
    }

    fn action(self, trailing: &str) -> CommandAction {
        let argument = trailing.trim();
        match self {
            // Canonical prefix, so `/ME  waves` sends `/me waves`.
            Self::Me if argument.is_empty() => CommandAction::Incomplete,
            Self::Me => CommandAction::Send(format!("{ME_PREFIX}{argument}")),
            Self::Shrug if argument.is_empty() => CommandAction::Send(SHRUG.to_string()),
            Self::Shrug => CommandAction::Send(format!("{argument} {SHRUG}")),
            Self::Away => CommandAction::SetAvailability(Availability::Away),
            Self::Active => CommandAction::SetAvailability(Availability::Available),
            Self::Dnd => CommandAction::SetAvailability(Availability::DoNotDisturb),
        }
    }
}

/// The user's manual presence.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Availability {
    #[default]
    Available,
    Away,
    DoNotDisturb,
}

impl Availability {
    /// RFC 6121 §4.7.2.1 `<show/>`; none means available.
    pub fn show(self) -> Option<&'static str> {
        match self {
            Self::Available => None,
            Self::Away => Some("away"),
            Self::DoNotDisturb => Some("dnd"),
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Available => "Available",
            Self::Away => "Away",
            Self::DoNotDisturb => "Do Not Disturb",
        }
    }
}

/// What submitting the composer's text does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandAction {
    Send(String),
    /// The command needs more text before it can run.
    Incomplete,
    SetAvailability(Availability),
}

/// The action for a body that starts with a built-in command, or `None`
/// to send the body as written (including unknown `/words`).
pub fn command_action(body: &str) -> Option<(Command, CommandAction)> {
    let rest = body.strip_prefix('/')?;
    let word_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let (word, trailing) = rest.split_at(word_end);
    let command = Command::named(word)?;
    Some((command, command.action(trailing)))
}

/// Someone a room message can mention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mentionable {
    /// The room nick, typed after `@`.
    pub nick: String,
    /// The real bare JID, when the room exposes it.
    pub jid: String,
}

/// One row in the suggestion popover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub title: String,
    pub subtitle: String,
    /// Replaces the characters `start..end` of the composer text.
    pub replacement: String,
    pub start: usize,
    pub end: usize,
}

/// Suggestions for the token being typed at `cursor` (in characters):
/// commands while the first word starts with `/`, and in a room, the
/// occupants and broadcasts an `@word` could name.
pub fn suggestions(text: &str, cursor: usize, room: Option<&[Mentionable]>) -> Vec<Suggestion> {
    let before: Vec<char> = text.chars().take(cursor).collect();
    if before.len() != cursor {
        return Vec::new();
    }
    if before.first() == Some(&'/') && !before.iter().any(|c| c.is_whitespace()) {
        let query: String = before[1..].iter().collect();
        return command_suggestions(&query.to_lowercase(), cursor);
    }
    let Some(people) = room else {
        return Vec::new();
    };
    let Some(at) = before.iter().rposition(|c| *c == '@') else {
        return Vec::new();
    };
    let starts_word = at == 0 || before[at - 1].is_whitespace() || before[at - 1] == '(';
    let query: String = before[at + 1..].iter().collect();
    if !starts_word || query.chars().any(char::is_whitespace) {
        return Vec::new();
    }
    mention_suggestions(&query.to_lowercase(), people, at, cursor)
}

fn command_suggestions(query: &str, cursor: usize) -> Vec<Suggestion> {
    Command::ALL
        .into_iter()
        .filter(|command| command.name().starts_with(query))
        .map(|command| Suggestion {
            title: command.usage(),
            subtitle: command.description().to_string(),
            replacement: format!("/{} ", command.name()),
            start: 0,
            end: cursor,
        })
        .collect()
}

fn mention_suggestions(
    query: &str,
    people: &[Mentionable],
    start: usize,
    end: usize,
) -> Vec<Suggestion> {
    let mention = |label: &str, subtitle: &str| Suggestion {
        title: format!("@{label}"),
        subtitle: subtitle.to_string(),
        replacement: format!("@{label} "),
        start,
        end,
    };
    let mut people: Vec<&Mentionable> = people
        .iter()
        .filter(|person| person.nick.to_lowercase().contains(query))
        .collect();
    // Prefix matches first, then alphabetical.
    people.sort_by_key(|person| {
        let nick = person.nick.to_lowercase();
        (!nick.starts_with(query), nick)
    });
    let broadcasts = BROADCASTS
        .into_iter()
        .filter(|(label, _)| label.starts_with(query))
        .map(|(label, subtitle)| mention(label, subtitle));
    people
        .into_iter()
        .map(|person| mention(&person.nick, &person.jid))
        .chain(broadcasts)
        .take(MAX_SUGGESTIONS)
        .collect()
}

/// The text with `suggestion` applied, and where the cursor goes.
pub fn apply(text: &str, suggestion: &Suggestion) -> (String, usize) {
    let chars: Vec<char> = text.chars().collect();
    let start = suggestion.start.min(chars.len());
    let end = suggestion.end.clamp(start, chars.len());
    let mut applied: String = chars[..start].iter().collect();
    applied.push_str(&suggestion.replacement);
    let cursor = applied.chars().count();
    // Skip the space the replacement already ends with.
    let rest = &chars[end..];
    let rest = match (suggestion.replacement.ends_with(' '), rest.first()) {
        (true, Some(' ')) => &rest[1..],
        _ => rest,
    };
    applied.extend(rest);
    (applied, cursor)
}

/// The action an XEP-0245 `/me` body describes, or `None`.
pub fn me_action(body: &str) -> Option<&str> {
    body.strip_prefix(ME_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn people() -> Vec<Mentionable> {
        ["bob", "Ann", "annette"]
            .into_iter()
            .map(|nick| Mentionable {
                nick: nick.into(),
                jid: format!("{}@waddle.social", nick.to_lowercase()),
            })
            .collect()
    }

    fn titles(suggestions: &[Suggestion]) -> Vec<&str> {
        suggestions.iter().map(|s| s.title.as_str()).collect()
    }

    #[test]
    fn slash_in_the_first_word_lists_matching_commands() {
        assert_eq!(titles(&suggestions("/", 1, None)).len(), Command::ALL.len());
        assert_eq!(titles(&suggestions("/A", 2, None)), ["/away", "/active"]);
        assert!(suggestions("/me waves", 9, None).is_empty());
        assert!(suggestions("a /me", 5, None).is_empty());
    }

    #[test]
    fn at_lists_occupants_prefix_first_then_broadcasts_in_rooms_only() {
        let people = people();
        let found = suggestions("hi @an", 6, Some(&people));
        assert_eq!(titles(&found), ["@Ann", "@annette"]);
        assert_eq!((found[0].start, found[0].end), (3, 6));
        assert_eq!(
            titles(&suggestions("@e", 2, Some(&people))),
            ["@annette", "@everyone"]
        );
        assert!(suggestions("hi @an", 6, None).is_empty());
        assert!(suggestions("mail@an", 7, Some(&people)).is_empty());
        assert!(suggestions("@ann lee", 8, Some(&people)).is_empty());
    }

    #[test]
    fn applying_replaces_the_token_and_places_the_cursor_after_it() {
        let people = people();
        let pick = &suggestions("hi @bo there", 6, Some(&people))[0];
        assert_eq!(apply("hi @bo there", pick), ("hi @bob there".into(), 8));
        let pick = &suggestions("/sh", 3, None)[0];
        assert_eq!(apply("/sh", pick), ("/shrug ".into(), 7));
        let pick = &suggestions("ñ @b", 4, Some(&people))[0];
        assert_eq!(apply("ñ @b", pick), ("ñ @bob ".into(), 7));
    }

    #[test]
    fn commands_send_canonical_bodies_or_set_presence() {
        assert_eq!(
            command_action("/ME  waves "),
            Some((Command::Me, CommandAction::Send("/me waves".into())))
        );
        assert_eq!(
            command_action("/me"),
            Some((Command::Me, CommandAction::Incomplete))
        );
        assert_eq!(
            command_action("/shrug fine"),
            Some((
                Command::Shrug,
                CommandAction::Send("fine ¯\\_(ツ)_/¯".into())
            ))
        );
        assert_eq!(
            command_action("/dnd"),
            Some((
                Command::Dnd,
                CommandAction::SetAvailability(Availability::DoNotDisturb)
            ))
        );
        assert_eq!(command_action("/unknown thing"), None);
        assert_eq!(command_action("/meow"), None);
        assert_eq!(command_action("hello"), None);
    }

    #[test]
    fn me_bodies_match_the_exact_prefix() {
        assert_eq!(me_action("/me waves"), Some("waves"));
        assert_eq!(me_action("/ME waves"), None);
        assert_eq!(me_action(" /me waves"), None);
        assert_eq!(me_action("/meshrugs"), None);
    }
}
