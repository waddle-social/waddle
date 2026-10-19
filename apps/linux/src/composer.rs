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
    /// What is typed after `@`: the room nick of an occupant, or the
    /// username of someone found in the XEP-0055 user directory.
    pub nick: String,
    /// The real bare JID.
    pub jid: String,
    /// A directory entry's display name, shown beside the JID.
    pub display_name: Option<String>,
}

/// Whom an `@` in a room can name.
#[derive(Debug, Clone, Copy, Default)]
pub struct RoomPeople<'a> {
    /// Present occupants with a known real JID.
    pub occupants: &'a [Mentionable],
    /// XEP-0055 directory results for the query being typed.
    pub directory: &'a [Mentionable],
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
/// occupants, directory users and broadcasts an `@word` could name.
pub fn suggestions(text: &str, cursor: usize, room: Option<RoomPeople<'_>>) -> Vec<Suggestion> {
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
    let Some((at, query)) = mention_token(&before) else {
        return Vec::new();
    };
    mention_suggestions(&query.to_lowercase(), people, at, cursor)
}

/// The `@query` being typed at `cursor`, without the `@`, or `None`
/// when the cursor is not in a mention token.
pub fn mention_query(text: &str, cursor: usize) -> Option<String> {
    let before: Vec<char> = text.chars().take(cursor).collect();
    let command = before.first() == Some(&'/') && !before.iter().any(|c| c.is_whitespace());
    if before.len() != cursor || command {
        return None;
    }
    mention_token(&before).map(|(_, query)| query)
}

/// Where the `@` of the token ending at the end of `before` sits, and
/// what follows it.
fn mention_token(before: &[char]) -> Option<(usize, String)> {
    let at = before.iter().rposition(|c| *c == '@')?;
    let starts_word = at == 0 || before[at - 1].is_whitespace() || before[at - 1] == '(';
    let query: String = before[at + 1..].iter().collect();
    (starts_word && !query.chars().any(char::is_whitespace)).then_some((at, query))
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

/// Matching occupants, then directory users not already listed, then
/// broadcasts.
fn mention_suggestions(
    query: &str,
    people: RoomPeople<'_>,
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
    let mut occupants: Vec<&Mentionable> = people
        .occupants
        .iter()
        .filter(|person| person.nick.to_lowercase().contains(query))
        .collect();
    // Prefix matches first, then alphabetical.
    occupants.sort_by_key(|person| {
        let nick = person.nick.to_lowercase();
        (!nick.starts_with(query), nick)
    });
    // The directory matched the query itself; keep its order. A directory
    // user whose label an occupant already holds is left out: picking it
    // would insert `@label`, which `mention_targets` resolves to the occupant.
    let occupant_labels: Vec<String> = people
        .occupants
        .iter()
        .map(|p| p.nick.trim().to_lowercase())
        .collect();
    let mut listed: Vec<String> = occupants.iter().map(|p| p.jid.to_lowercase()).collect();
    let directory = people.directory.iter().filter(|person| {
        let jid = person.jid.to_lowercase();
        let new =
            !listed.contains(&jid) && !occupant_labels.contains(&person.nick.trim().to_lowercase());
        listed.push(jid);
        new
    });
    let broadcasts = BROADCASTS
        .into_iter()
        .filter(|(label, _)| label.starts_with(query))
        .map(|(label, subtitle)| mention(label, subtitle));
    occupants
        .into_iter()
        .chain(directory)
        .map(|person| mention(&person.nick, &person_subtitle(person)))
        .chain(broadcasts)
        .take(MAX_SUGGESTIONS)
        .collect()
}

fn person_subtitle(person: &Mentionable) -> String {
    match person.display_name.as_deref().map(str::trim) {
        Some(name) if !name.is_empty() && name != person.nick => {
            format!("{name} · {}", person.jid)
        }
        _ => person.jid.clone(),
    }
}

/// Whom a room send's typed `@label`s resolve to: the occupants, then
/// the directory users seen in this room's searches, except a directory
/// user whose label an occupant already uses, so no label is ambiguous.
pub fn mention_targets(occupants: Vec<Mentionable>, directory: &[Mentionable]) -> Vec<Mentionable> {
    let mut labels: Vec<String> = occupants
        .iter()
        .map(|person| person.nick.trim().to_lowercase())
        .collect();
    let mut targets = occupants;
    for person in directory {
        let label = person.nick.trim().to_lowercase();
        if !labels.contains(&label) {
            labels.push(label);
            targets.push(person.clone());
        }
    }
    targets
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
                display_name: None,
            })
            .collect()
    }

    fn room(occupants: &[Mentionable]) -> Option<RoomPeople<'_>> {
        Some(RoomPeople {
            occupants,
            directory: &[],
        })
    }

    fn user(username: &str, jid: &str, display_name: Option<&str>) -> Mentionable {
        Mentionable {
            nick: username.into(),
            jid: jid.into(),
            display_name: display_name.map(Into::into),
        }
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
        let found = suggestions("hi @an", 6, room(&people));
        assert_eq!(titles(&found), ["@Ann", "@annette"]);
        assert_eq!((found[0].start, found[0].end), (3, 6));
        assert_eq!(
            titles(&suggestions("@e", 2, room(&people))),
            ["@annette", "@everyone"]
        );
        assert!(suggestions("hi @an", 6, None).is_empty());
        assert!(suggestions("mail@an", 7, room(&people)).is_empty());
        assert!(suggestions("@ann lee", 8, room(&people)).is_empty());
    }

    #[test]
    fn directory_users_follow_occupants_without_duplicates_before_broadcasts() {
        let people = people();
        let directory = [
            user("ann", "ANN@waddle.social", None),
            user("anna", "anna@waddle.social", Some("Anna Lee")),
            user("anna2", "anna@waddle.social", None),
        ];
        let room = Some(RoomPeople {
            occupants: &people,
            directory: &directory,
        });
        let found = suggestions("hi @an", 6, room);
        assert_eq!(titles(&found), ["@Ann", "@annette", "@anna"]);
        assert_eq!(found[2].subtitle, "Anna Lee · anna@waddle.social");
        assert_eq!(found[2].replacement, "@anna ");
        assert_eq!(
            titles(&suggestions("@e", 2, room)),
            ["@annette", "@anna", "@everyone"],
            "the server matched the query; one entry per JID and per occupant label"
        );
        let clash = [user("ann", "other@waddle.social", None)];
        let clashing = Some(RoomPeople {
            occupants: &people,
            directory: &clash,
        });
        assert_eq!(
            titles(&suggestions("@an", 3, clashing)),
            ["@Ann", "@annette"],
            "`@ann` would resolve to the occupant, so the other account is not offered"
        );
        let many: Vec<Mentionable> = (0..20)
            .map(|n| user(&format!("e{n}"), &format!("e{n}@waddle.social"), None))
            .collect();
        let crowded = Some(RoomPeople {
            occupants: &people,
            directory: &many,
        });
        assert_eq!(suggestions("@e", 2, crowded).len(), MAX_SUGGESTIONS);
    }

    #[test]
    fn the_mention_query_is_the_at_token_before_the_cursor() {
        assert_eq!(mention_query("hi @An", 6).as_deref(), Some("An"));
        assert_eq!(mention_query("hi @", 4).as_deref(), Some(""));
        assert_eq!(mention_query("hi @an there", 12), None);
        assert_eq!(mention_query("mail@an", 7), None);
        assert_eq!(mention_query("/@an", 4), None);
        assert_eq!(mention_query("/me @an", 7).as_deref(), Some("an"));
        assert_eq!(mention_query("@an", 9), None);
    }

    #[test]
    fn send_targets_add_directory_users_whose_labels_occupants_do_not_use() {
        let occupants = vec![user("Ann", "ann@waddle.social", None)];
        let directory = [
            user("ann", "other@waddle.social", None),
            user("bob", "bob@waddle.social", None),
            user("ann2", "ann@waddle.social", None),
        ];
        let targets = mention_targets(occupants, &directory);
        let labels: Vec<(&str, &str)> = targets
            .iter()
            .map(|t| (t.nick.as_str(), t.jid.as_str()))
            .collect();
        assert_eq!(
            labels,
            [
                ("Ann", "ann@waddle.social"),
                ("bob", "bob@waddle.social"),
                ("ann2", "ann@waddle.social"),
            ]
        );
    }

    #[test]
    fn applying_replaces_the_token_and_places_the_cursor_after_it() {
        let people = people();
        let pick = &suggestions("hi @bo there", 6, room(&people))[0];
        assert_eq!(apply("hi @bo there", pick), ("hi @bob there".into(), 8));
        let pick = &suggestions("/sh", 3, None)[0];
        assert_eq!(apply("/sh", pick), ("/shrug ".into(), 7));
        let pick = &suggestions("ñ @b", 4, room(&people))[0];
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
