//! Stories (XEP-0501): who has one up, which are unseen, how the viewer
//! steps and times them, and how reactions (XEP-0470) and the account's
//! private "seen" state (kept on its own PEP node) are kept in step.
//!
//! No GTK here. `ui/stories.rs` draws it and `session_feed.rs` fetches it.

use std::collections::{HashMap, HashSet};

use waddle_xmpp_client_ffi::{WaddleStory, WaddleStoryReactions, WaddleStoryRead};

/// How long a picture stays up in the viewer before the next story.
pub const PICTURE_MS: i64 = 6_000;
/// The most reactions one member may leave on a story.
pub const REACTION_LIMIT: usize = 12;
/// A story's picture is read by the whole community, so it is uploaded as
/// it is; the web client's cap applies.
pub const MAX_MEDIA_BYTES: u64 = crate::attachments::MAX_UPLOAD_BYTES;
/// How long a "seen" is remembered. Stories live a day by default, so two
/// days outlasts them.
pub const READ_LIFETIME_MS: i64 = 48 * 60 * 60 * 1000;
/// The most "seen" entries kept.
pub const READ_LIMIT: usize = 5000;
/// What the viewer offers to react with.
pub const QUICK_EMOJI: [&str; 6] = ["❤️", "😂", "😮", "😢", "👏", "🔥"];

/// What a story shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Picture,
    /// Played by the browser or phone apps; opened outside the app here.
    Video,
}

/// One story.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Story {
    pub id: String,
    pub body: Option<String>,
    pub media_url: String,
    pub media_type: Option<String>,
    pub author: Option<String>,
    pub posted_ms: Option<i64>,
    /// `None` never expires.
    pub expires_ms: Option<i64>,
}

impl Story {
    pub fn from_wire(story: WaddleStory) -> Self {
        Self {
            id: story.id,
            body: story.body.filter(|body| !body.trim().is_empty()),
            media_url: story.media_url,
            media_type: story.media_type,
            author: story.author.filter(|author| !author.trim().is_empty()),
            posted_ms: story.posted_ms,
            expires_ms: story.expires_ms,
        }
    }

    /// By the declared type, or by the file's extension when the author's
    /// client declared none.
    pub fn kind(&self) -> MediaKind {
        if let Some(media_type) = &self.media_type {
            return if media_type.to_ascii_lowercase().starts_with("video/") {
                MediaKind::Video
            } else {
                MediaKind::Picture
            };
        }
        let path = self.media_url.split(['?', '#']).next().unwrap_or("");
        let path = path.to_ascii_lowercase();
        if [".mp4", ".m4v", ".mov", ".webm"]
            .iter()
            .any(|extension| path.ends_with(extension))
        {
            MediaKind::Video
        } else {
            MediaKind::Picture
        }
    }

    pub fn is_active(&self, now_ms: i64) -> bool {
        self.expires_ms.is_none_or(|expires| expires > now_ms)
    }
}

/// The stories still up at `now_ms`, in the order the server sent them.
pub fn active(stories: Vec<WaddleStory>, now_ms: i64) -> Vec<Story> {
    stories
        .into_iter()
        .map(Story::from_wire)
        .filter(|story| story.is_active(now_ms))
        .collect()
}

/// Put a story this device just made in the list, unless it has it.
pub fn prepend(stories: &mut Vec<Story>, story: Story) {
    stories.retain(|existing| existing.id != story.id);
    stories.insert(0, story);
}

/// Which stories the account has seen, and when (epoch milliseconds).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reads(HashMap<String, i64>);

impl Reads {
    pub fn from_wire(reads: Vec<WaddleStoryRead>) -> Self {
        Self(
            reads
                .into_iter()
                .map(|read| (read.story_id, read.read_at_ms))
                .collect(),
        )
    }

    pub fn is_read(&self, id: &str) -> bool {
        self.0.contains_key(id)
    }

    /// Returns whether this was news.
    pub fn mark(&mut self, id: &str, at_ms: i64) -> bool {
        if self.0.contains_key(id) {
            return false;
        }
        self.0.insert(id.to_string(), at_ms);
        true
    }

    /// Add what another device saw; the later time wins for a story both
    /// saw.
    pub fn merge(&mut self, remote: &Reads) {
        for (id, at) in &remote.0 {
            let entry = self.0.entry(id.clone()).or_insert(*at);
            *entry = (*entry).max(*at);
        }
    }

    /// Drop what is past [`READ_LIFETIME_MS`], then all but the newest
    /// [`READ_LIMIT`].
    pub fn pruned(&self, now_ms: i64) -> Reads {
        let cutoff = now_ms.saturating_sub(READ_LIFETIME_MS);
        let mut fresh: Vec<(&String, &i64)> =
            self.0.iter().filter(|(_, at)| **at >= cutoff).collect();
        if fresh.len() > READ_LIMIT {
            fresh.sort_by(|a, b| (b.1, b.0).cmp(&(a.1, a.0)));
            fresh.truncate(READ_LIMIT);
        }
        Reads(
            fresh
                .into_iter()
                .map(|(id, at)| (id.clone(), *at))
                .collect(),
        )
    }

    /// What to publish: the whole pruned state, in a stable order.
    pub fn to_wire(&self, now_ms: i64) -> Vec<WaddleStoryRead> {
        let mut reads: Vec<WaddleStoryRead> = self
            .pruned(now_ms)
            .0
            .into_iter()
            .map(|(story_id, read_at_ms)| WaddleStoryRead {
                story_id,
                read_at_ms,
            })
            .collect();
        reads.sort_by(|a, b| a.story_id.cmp(&b.story_id));
        reads
    }

    /// Whether publishing would add to `remote`. The server holds one item
    /// for every device, so this is published only when this device adds
    /// to it, and never over a copy that could not be fetched.
    pub fn adds_to(&self, remote: &Reads, now_ms: i64) -> bool {
        self.pruned(now_ms) != remote.pruned(now_ms)
    }
}

/// One person's stories as the rail shows them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ring {
    pub author: String,
    /// Oldest first, the order they are watched in.
    pub stories: Vec<Story>,
    /// At least one story not seen yet.
    pub unseen: bool,
    pub own: bool,
}

impl Ring {
    fn latest_ms(&self) -> i64 {
        self.stories
            .iter()
            .filter_map(|story| story.posted_ms)
            .max()
            .unwrap_or(i64::MIN)
    }

    /// Where watching starts: the first story not seen yet, else the first.
    pub fn first_to_watch(&self, reads: &Reads) -> usize {
        self.stories
            .iter()
            .position(|story| !reads.is_read(&story.id))
            .unwrap_or(0)
    }
}

/// The rail along the top of the feed: one ring per author, the account's
/// own first, then those with something unseen, each group newest first.
/// Stories without an author have no one to show and are left out.
pub fn rings(stories: &[Story], reads: &Reads, own: &str) -> Vec<Ring> {
    let mut by_author: Vec<Ring> = Vec::new();
    let mut at: HashMap<String, usize> = HashMap::new();
    for story in stories {
        let Some(author) = story.author.as_deref() else {
            continue;
        };
        let key = author.to_lowercase();
        let index = *at.entry(key.clone()).or_insert_with(|| {
            by_author.push(Ring {
                author: author.to_string(),
                stories: Vec::new(),
                unseen: false,
                own: key == own.to_lowercase(),
            });
            by_author.len() - 1
        });
        by_author[index].stories.push(story.clone());
    }
    for ring in &mut by_author {
        ring.stories
            .sort_by(|a, b| (a.posted_ms, &a.id).cmp(&(b.posted_ms, &b.id)));
        ring.unseen = ring.stories.iter().any(|story| !reads.is_read(&story.id));
    }
    by_author.sort_by(|a, b| {
        (
            !a.own,
            !a.unseen,
            std::cmp::Reverse(a.latest_ms()),
            &a.author,
        )
            .cmp(&(
                !b.own,
                !b.unseen,
                std::cmp::Reverse(b.latest_ms()),
                &b.author,
            ))
    });
    by_author
}

/// What a step did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Moved,
    /// Back from the first story.
    Unchanged,
    /// On from the last story: the viewer closes.
    Finished,
}

/// The stories the viewer steps through, and the clock of the one shown.
#[derive(Debug, Clone)]
pub struct Playthrough {
    stories: Vec<Story>,
    index: usize,
    elapsed_ms: i64,
    paused: bool,
}

impl Playthrough {
    /// Watch from ring `start`, through every ring after it. `None` when
    /// there is nothing to watch.
    pub fn open(rings: &[Ring], start: usize, reads: &Reads) -> Option<Self> {
        let ring = rings.get(start)?;
        let first = ring.first_to_watch(reads);
        let mut stories: Vec<Story> = ring.stories.clone();
        let index = first.min(stories.len().saturating_sub(1));
        for later in rings.iter().skip(start + 1) {
            stories.extend(later.stories.iter().cloned());
        }
        (!stories.is_empty()).then_some(Self {
            stories,
            index,
            elapsed_ms: 0,
            paused: false,
        })
    }

    pub fn current(&self) -> &Story {
        &self.stories[self.index]
    }

    #[cfg(test)]
    pub fn position(&self) -> usize {
        self.index
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.stories.len()
    }

    /// Move by `offset`; the clock starts over on a story that changes.
    pub fn step(&mut self, offset: isize) -> Step {
        let next = self.index.saturating_add_signed(offset);
        if offset > 0 && next >= self.stories.len() {
            return Step::Finished;
        }
        if offset < 0 && self.index.checked_add_signed(offset).is_none() {
            // Back from the first story starts it again.
            self.elapsed_ms = 0;
            return Step::Unchanged;
        }
        if next == self.index {
            return Step::Unchanged;
        }
        self.index = next;
        self.elapsed_ms = 0;
        Step::Moved
    }

    /// Time passed. A picture moves on after [`PICTURE_MS`]; a video story
    /// is opened outside the app and waits for the viewer to step.
    pub fn tick(&mut self, delta_ms: i64) -> Option<Step> {
        if self.paused || self.current().kind() != MediaKind::Picture {
            return None;
        }
        self.elapsed_ms += delta_ms.max(0);
        (self.elapsed_ms >= PICTURE_MS).then(|| self.step(1))
    }

    /// The positions of the stories by the shown story's author that sit
    /// next to it: what the viewer's progress bars stand for.
    pub fn same_author(&self) -> std::ops::Range<usize> {
        let author = &self.current().author;
        let mut start = self.index;
        while start > 0 && &self.stories[start - 1].author == author {
            start -= 1;
        }
        let mut end = self.index + 1;
        while end < self.stories.len() && &self.stories[end].author == author {
            end += 1;
        }
        start..end
    }

    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    #[cfg(test)]
    pub fn paused(&self) -> bool {
        self.paused
    }

    /// How full the bar of the story at `position` is, 0 to 1: full for
    /// those before the one shown, empty after it, and the shown one's
    /// share of its time.
    pub fn fill(&self, position: usize) -> f64 {
        use std::cmp::Ordering::{Equal, Greater, Less};
        match position.cmp(&self.index) {
            Less => 1.0,
            Greater => 0.0,
            Equal if self.current().kind() == MediaKind::Picture => {
                (self.elapsed_ms as f64 / PICTURE_MS as f64).clamp(0.0, 1.0)
            }
            Equal => 1.0,
        }
    }
}

/// The reactions on one story.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reactions {
    /// Emoji and how many members left each, in the order the server gave.
    pub counts: Vec<(String, u32)>,
    /// The ones this account left.
    pub mine: Vec<String>,
}

impl Reactions {
    pub fn from_wire(reactions: WaddleStoryReactions) -> Self {
        Self {
            counts: reactions
                .reactions
                .into_iter()
                .map(|reaction| (reaction.emoji, reaction.count))
                .collect(),
            mine: reactions.mine,
        }
    }

    pub fn count_of(&self, emoji: &str) -> u32 {
        self.counts
            .iter()
            .find(|(candidate, _)| candidate == emoji)
            .map_or(0, |(_, count)| *count)
    }

    /// The reactions once this account adds `emoji`, or takes it back if
    /// it is already its own. `None` when that would pass the limit.
    pub fn toggled(&self, emoji: &str) -> Option<Self> {
        let was_mine = self.mine.iter().any(|mine| mine == emoji);
        let mut mine: Vec<String> = self
            .mine
            .iter()
            .filter(|mine| *mine != emoji)
            .cloned()
            .collect();
        if !was_mine {
            mine.push(emoji.to_string());
        }
        if mine.len() > REACTION_LIMIT {
            return None;
        }
        let mut counts = self.counts.clone();
        match counts.iter().position(|(candidate, _)| candidate == emoji) {
            Some(at) => {
                let count = if was_mine {
                    counts[at].1.saturating_sub(1)
                } else {
                    counts[at].1 + 1
                };
                if count == 0 {
                    counts.remove(at);
                } else {
                    counts[at].1 = count;
                }
            }
            None if !was_mine => counts.push((emoji.to_string(), 1)),
            None => {}
        }
        Some(Self { counts, mine })
    }
}

/// Keeps reaction writes in order. Each write carries all of the account's
/// emoji for a story, so quick taps must end as the last one left them:
/// one write per story is in flight at a time, and what was chosen while
/// it ran goes out after it.
#[derive(Debug, Default)]
pub struct ReactionWrites {
    /// What the server last agreed to, for stories with a write pending.
    confirmed: HashMap<String, Reactions>,
    wanted: HashMap<String, Vec<String>>,
    writing: HashSet<String>,
}

impl ReactionWrites {
    /// The account now wants `mine` on `story`, which showed `before`.
    /// True when the caller must start writing (none is in flight).
    pub fn want(&mut self, story: &str, before: &Reactions, mine: Vec<String>) -> bool {
        self.confirmed
            .entry(story.to_string())
            .or_insert_with(|| before.clone());
        self.wanted.insert(story.to_string(), mine);
        self.writing.insert(story.to_string())
    }

    /// A write for `story` is in flight or waiting.
    pub fn pending(&self, story: &str) -> bool {
        self.writing.contains(story)
    }

    /// The next emoji set to send for `story`, if one is waiting.
    pub fn next(&mut self, story: &str) -> Option<Vec<String>> {
        self.wanted.remove(story)
    }

    /// The write of `sent` ended: write again when something newer is
    /// waiting, otherwise done, or back to what the server last agreed to
    /// when it failed.
    pub fn finished(&mut self, story: &str, sent: &[String], ok: bool) -> Finish {
        if ok {
            if let Some(confirmed) = self.confirmed.get_mut(story) {
                confirmed.mine = sent.to_vec();
            }
        }
        if self.wanted.contains_key(story) {
            return Finish::WriteAgain;
        }
        self.writing.remove(story);
        let confirmed = self.confirmed.remove(story);
        if ok {
            Finish::Done
        } else {
            Finish::Revert(confirmed.unwrap_or_default())
        }
    }
}

/// How a reaction write left things.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finish {
    Done,
    WriteAgain,
    /// Back to what the server last agreed to.
    Revert(Reactions),
}

/// Why a picture cannot be shared as a story.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaError {
    NotAPicture,
    TooLarge,
    Empty,
}

impl MediaError {
    pub fn message(self) -> &'static str {
        match self {
            Self::NotAPicture => "Choose a picture (PNG, JPEG, GIF or WebP).",
            Self::TooLarge => "The picture is larger than 10 MB.",
            Self::Empty => "The picture is empty.",
        }
    }
}

/// Whether `media_type` of `size` bytes may be shared as a story. Pictures
/// only: stories with video are watched in the other apps.
pub fn check_media(media_type: &str, size: u64) -> Result<(), MediaError> {
    if !matches!(
        media_type.to_ascii_lowercase().as_str(),
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    ) {
        return Err(MediaError::NotAPicture);
    }
    if size == 0 {
        return Err(MediaError::Empty);
    }
    if size > MAX_MEDIA_BYTES {
        return Err(MediaError::TooLarge);
    }
    Ok(())
}

/// A caption as published: trimmed, and none when blank.
pub fn caption(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire(id: &str, author: &str, posted: i64, expires: Option<i64>) -> WaddleStory {
        WaddleStory {
            id: id.to_string(),
            body: None,
            media_url: format!("https://up.test/{id}.png"),
            media_type: Some("image/png".to_string()),
            author: Some(author.to_string()),
            posted_ms: Some(posted),
            expires_ms: expires,
        }
    }

    fn story(id: &str, author: &str, posted: i64) -> Story {
        Story::from_wire(wire(id, author, posted, None))
    }

    #[test]
    fn expired_stories_are_left_out() {
        let kept = active(
            vec![
                wire("old", "a@x.test", 1, Some(100)),
                wire("live", "a@x.test", 2, Some(500)),
                wire("forever", "a@x.test", 3, None),
            ],
            200,
        );
        let ids: Vec<&str> = kept.iter().map(|story| story.id.as_str()).collect();
        assert_eq!(ids, ["live", "forever"]);
    }

    #[test]
    fn media_kind_follows_the_type_then_the_extension() {
        let mut story = story("a", "a@x.test", 1);
        assert_eq!(story.kind(), MediaKind::Picture);
        story.media_type = Some("video/mp4".to_string());
        assert_eq!(story.kind(), MediaKind::Video);
        story.media_type = None;
        story.media_url = "https://up.test/clip.MOV?x=1".to_string();
        assert_eq!(story.kind(), MediaKind::Video);
        story.media_url = "https://up.test/pic".to_string();
        assert_eq!(story.kind(), MediaKind::Picture);
    }

    #[test]
    fn seen_state_merges_and_the_later_time_wins() {
        let mut local = Reads::default();
        assert!(local.mark("a", 10));
        assert!(!local.mark("a", 99));
        let remote = Reads::from_wire(vec![
            WaddleStoryRead {
                story_id: "a".into(),
                read_at_ms: 50,
            },
            WaddleStoryRead {
                story_id: "b".into(),
                read_at_ms: 20,
            },
        ]);
        local.merge(&remote);
        assert!(local.is_read("b"));
        assert_eq!(local.0["a"], 50);
    }

    #[test]
    fn old_and_excess_reads_are_pruned() {
        let now = 10 * READ_LIFETIME_MS;
        let mut reads = Reads::default();
        reads.mark("stale", now - READ_LIFETIME_MS - 1);
        reads.mark("fresh", now - 1);
        assert!(!reads.pruned(now).is_read("stale"));
        assert!(reads.pruned(now).is_read("fresh"));

        let mut many = Reads::default();
        for at in 0..READ_LIMIT + 3 {
            many.mark(&format!("s{at}"), now - 1_000 + at as i64);
        }
        let kept = many.pruned(now);
        assert_eq!(kept.0.len(), READ_LIMIT);
        assert!(!kept.is_read("s0") && kept.is_read(&format!("s{}", READ_LIMIT + 2)));
    }

    #[test]
    fn reads_are_published_only_when_this_device_adds_to_them() {
        let now = 1_000;
        let mut local = Reads::default();
        let remote = Reads::from_wire(vec![WaddleStoryRead {
            story_id: "a".into(),
            read_at_ms: 900,
        }]);
        local.merge(&remote);
        assert!(!local.adds_to(&remote, now));
        local.mark("b", 950);
        assert!(local.adds_to(&remote, now));
        let wire = local.to_wire(now);
        assert_eq!(wire.len(), 2);
        assert_eq!(wire[0].story_id, "a");
    }

    #[test]
    fn the_rail_puts_you_first_then_unseen_then_newest() {
        let stories = vec![
            story("1", "ann@x.test", 100),
            story("2", "bob@x.test", 300),
            story("3", "me@x.test", 50),
            story("4", "cy@x.test", 200),
            story("5", "ann@x.test", 120),
        ];
        let mut reads = Reads::default();
        reads.mark("2", 1);
        let rings = rings(&stories, &reads, "ME@x.test");
        let order: Vec<&str> = rings.iter().map(|ring| ring.author.as_str()).collect();
        // Me, then unseen (ann newest 120, cy 200: cy first), then seen bob.
        assert_eq!(
            order,
            ["me@x.test", "cy@x.test", "ann@x.test", "bob@x.test"]
        );
        assert!(rings[0].own);
        assert!(rings[1].unseen && !rings[3].unseen);
        let ann = &rings[2];
        let ids: Vec<&str> = ann.stories.iter().map(|story| story.id.as_str()).collect();
        assert_eq!(ids, ["1", "5"]);
    }

    #[test]
    fn stories_without_an_author_have_no_ring() {
        let mut orphan = story("x", "a@x.test", 1);
        orphan.author = None;
        assert!(rings(&[orphan], &Reads::default(), "me@x.test").is_empty());
    }

    #[test]
    fn watching_starts_at_the_first_unseen_story_and_runs_on_through_the_rest() {
        let stories = vec![
            story("a1", "ann@x.test", 1),
            story("a2", "ann@x.test", 2),
            story("b1", "bob@x.test", 3),
        ];
        let mut reads = Reads::default();
        reads.mark("a1", 1);
        let rings = rings(&stories, &reads, "me@x.test");
        let ann = rings.iter().position(|r| r.author == "ann@x.test").unwrap();
        let watch = Playthrough::open(&rings, ann, &reads).unwrap();
        assert_eq!(watch.current().id, "a2");
        assert_eq!(watch.len() - watch.position(), 1 + rings[ann + 1..].len());
        assert!(Playthrough::open(&rings, 9, &reads).is_none());
    }

    fn walk() -> Playthrough {
        let stories = vec![story("1", "a@x.test", 1), story("2", "a@x.test", 2)];
        let reads = Reads::default();
        Playthrough::open(&rings(&stories, &reads, "me@x.test"), 0, &reads).unwrap()
    }

    #[test]
    fn stepping_moves_finishes_and_stays_at_the_start() {
        let mut watch = walk();
        assert_eq!(watch.step(-1), Step::Unchanged);
        assert_eq!(watch.position(), 0);
        assert_eq!(watch.step(1), Step::Moved);
        assert_eq!(watch.current().id, "2");
        assert_eq!(watch.step(1), Step::Finished);
        assert_eq!(watch.step(-1), Step::Moved);
        assert_eq!(watch.current().id, "1");
    }

    #[test]
    fn the_bars_stand_for_one_authors_neighbouring_stories() {
        let stories = vec![
            story("a1", "ann@x.test", 1),
            story("a2", "ann@x.test", 2),
            story("b1", "bob@x.test", 3),
        ];
        let reads = Reads::default();
        let rings = rings(&stories, &reads, "me@x.test");
        let mut watch = Playthrough::open(&rings, 0, &reads).unwrap();
        // Ring 0 is bob (newest, all unseen); ann follows.
        assert_eq!(watch.same_author(), 0..1);
        watch.step(1);
        assert_eq!(watch.same_author(), 1..3);
        watch.step(1);
        assert_eq!(watch.same_author(), 1..3);
    }

    #[test]
    fn a_picture_advances_when_its_time_is_up() {
        let mut watch = walk();
        assert_eq!(watch.tick(PICTURE_MS - 1), None);
        assert!(watch.fill(0) > 0.99 && watch.fill(0) < 1.0);
        assert_eq!(watch.tick(1), Some(Step::Moved));
        assert_eq!(watch.fill(0), 1.0);
        assert_eq!(watch.fill(1), 0.0);
        assert_eq!(watch.tick(PICTURE_MS), Some(Step::Finished));
    }

    #[test]
    fn pausing_and_videos_stop_the_clock() {
        let mut watch = walk();
        watch.set_paused(true);
        assert_eq!(watch.tick(PICTURE_MS * 2), None);
        assert!(watch.paused());
        watch.set_paused(false);
        watch.stories[0].media_type = Some("video/mp4".to_string());
        assert_eq!(watch.tick(PICTURE_MS * 2), None);
        assert_eq!(watch.fill(0), 1.0);
    }

    #[test]
    fn stepping_restarts_the_clock() {
        let mut watch = walk();
        watch.tick(PICTURE_MS / 2);
        watch.step(1);
        assert_eq!(watch.fill(1), 0.0);
    }

    #[test]
    fn toggling_a_reaction_adds_then_takes_it_back() {
        let none = Reactions::default();
        let added = none.toggled("🔥").unwrap();
        assert_eq!(added.mine, ["🔥"]);
        assert_eq!(added.count_of("🔥"), 1);
        let more = Reactions {
            counts: vec![("🔥".into(), 3)],
            mine: vec![],
        }
        .toggled("🔥")
        .unwrap();
        assert_eq!(more.count_of("🔥"), 4);
        let removed = added.toggled("🔥").unwrap();
        assert!(removed.mine.is_empty() && removed.counts.is_empty());
    }

    #[test]
    fn reactions_stop_at_the_limit() {
        let full = Reactions {
            counts: Vec::new(),
            mine: (0..REACTION_LIMIT).map(|n| format!("e{n}")).collect(),
        };
        assert!(full.toggled("new").is_none());
        assert!(full.toggled("e0").is_some());
    }

    #[test]
    fn quick_taps_end_as_the_last_one_left_them() {
        let mut writes = ReactionWrites::default();
        let before = Reactions::default();
        assert!(writes.want("s", &before, vec!["a".into()]));
        // A second tap while the first is on its way does not start a writer.
        assert!(!writes.want("s", &before, vec!["a".into(), "b".into()]));
        assert_eq!(
            writes.next("s"),
            Some(vec!["a".to_string(), "b".to_string()])
        );
        // The writer finds nothing newer after it: done.
        assert_eq!(
            writes.finished("s", &["a".to_string(), "b".to_string()], true),
            Finish::Done
        );
        assert!(writes.want("s", &before, vec![]));
    }

    #[test]
    fn a_tap_during_a_write_is_sent_after_it() {
        let mut writes = ReactionWrites::default();
        let before = Reactions::default();
        writes.want("s", &before, vec!["a".into()]);
        let first = writes.next("s").unwrap();
        writes.want("s", &before, vec![]);
        assert_eq!(writes.finished("s", &first, true), Finish::WriteAgain);
        assert_eq!(writes.next("s"), Some(vec![]));
    }

    #[test]
    fn a_refused_write_goes_back_to_what_the_server_had() {
        let mut writes = ReactionWrites::default();
        let before = Reactions {
            counts: vec![("a".into(), 2)],
            mine: vec!["a".into()],
        };
        writes.want("s", &before, vec![]);
        let sent = writes.next("s").unwrap();
        assert_eq!(writes.finished("s", &sent, false), Finish::Revert(before));
    }

    #[test]
    fn a_confirmed_write_is_the_new_baseline_for_a_later_failure() {
        let mut writes = ReactionWrites::default();
        let before = Reactions::default();
        writes.want("s", &before, vec!["a".into()]);
        let first = writes.next("s").unwrap();
        writes.want("s", &before, vec!["a".into(), "b".into()]);
        assert_eq!(writes.finished("s", &first, true), Finish::WriteAgain);
        let second = writes.next("s").unwrap();
        match writes.finished("s", &second, false) {
            Finish::Revert(reverted) => assert_eq!(reverted.mine, ["a"]),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn only_small_pictures_are_shared() {
        assert_eq!(check_media("image/png", 10), Ok(()));
        assert_eq!(check_media("IMAGE/JPEG", 10), Ok(()));
        assert_eq!(check_media("video/mp4", 10), Err(MediaError::NotAPicture));
        assert_eq!(
            check_media("image/svg+xml", 10),
            Err(MediaError::NotAPicture)
        );
        assert_eq!(check_media("image/png", 0), Err(MediaError::Empty));
        assert_eq!(
            check_media("image/png", MAX_MEDIA_BYTES + 1),
            Err(MediaError::TooLarge)
        );
    }

    #[test]
    fn captions_are_trimmed_and_blank_ones_dropped() {
        assert_eq!(caption("  hi  "), Some("hi".to_string()));
        assert_eq!(caption(" \n "), None);
    }
}
