//! Read models for the workspace. Draws reuse these across frames.

use crate::link_worker::{ConnectView, Status};
use crate::model::{peer_label, Model};
use makepad_work_core::entity::{field, kind};
use makepad_work_core::event::{Actor, Id};
use makepad_work_core::fold::{addresses_my_ai, project_entities};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Seat {
    You,
    YourAi,
    Peer,
    PeerAi,
}

impl Seat {
    fn from_author(mine: bool, actor: Actor) -> Self {
        match (mine, actor) {
            (true, Actor::Human) => Self::You,
            (true, Actor::Ai) => Self::YourAi,
            (false, Actor::Human) => Self::Peer,
            (false, Actor::Ai) => Self::PeerAi,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::You => "You",
            Self::YourAi => "Your AI",
            Self::Peer => "Peer",
            Self::PeerAi => "Peer’s AI",
        }
    }
}

pub struct Card {
    pub number: String,
    pub title: String,
    pub seat: Seat,
}

pub struct DocumentRow {
    pub id: Id,
    pub title: String,
    pub editor: Seat,
}

pub struct Project {
    pub id: Id,
    pub name: String,
    pub goal: String,
    pub columns: [Vec<Card>; 4],
    pub authors: Vec<Seat>,
    pub docs: Vec<DocumentRow>,
}

pub struct Message {
    pub seat: Seat,
    pub name: String,
    pub time: String,
    pub text: String,
    pub addressed: bool,
}

pub struct RemovedProject {
    pub project: Id,
    pub event: Id,
    pub index: usize,
    pub title: String,
}

#[derive(Default)]
pub struct Workspace {
    index: std::collections::HashMap<Id, usize>,
    pub writable: bool,
    pub removable: Option<Id>,
    pub removed: Option<RemovedProject>,
    pub project_name_draft: Option<String>,
    pub projects: Vec<Project>,
    pub current: Option<Id>,
    pub messages: Vec<Message>,
    pub listening: bool,
    pub connect: AiCommands,
}

#[derive(Default)]
pub struct AiCommands {
    pub terminal: String,
    pub mcp: String,
}

impl Workspace {
    pub fn read(model: &Model, listening: bool, clock: &LocalClock) -> Self {
        let me = model.me();
        let mut projects = Vec::new();
        let mut index = std::collections::HashMap::new();
        for entity in project_entities(&model.state, None, Some(kind::PROJECT)) {
            index.insert(entity.id, projects.len());
            projects.push(Project {
                id: entity.id,
                name: entity.text(field::TITLE).to_owned(),
                goal: entity.text(field::TEXT).to_owned(),
                columns: Default::default(),
                authors: Vec::new(),
                docs: Vec::new(),
            });
        }
        for doc in model.state.entities.values().filter(|e| e.kind == kind::DOC && !e.flag(field::DELETED) && !e.flag(field::ARCHIVED)) {
            if let Some(&i) = doc.project.as_ref().and_then(|id| index.get(id)) {
                let (author, actor) = model.document_author(doc);
                projects[i].docs.push(DocumentRow { id: doc.id, title: doc.text(field::TITLE).into(), editor: Seat::from_author(author == me, actor) });
            }
        }
        for project in &mut projects {
            project.docs.sort_by(|a, b| a.title.cmp(&b.title).then(a.id.cmp(&b.id)));
        }
        let mut tasks: Vec<_> = model
            .state
            .entities
            .values()
            .filter(|e| e.kind == kind::TASK && !e.flag(field::DELETED) && !e.flag(field::ARCHIVED))
            .collect();
        tasks.sort_unstable_by_key(|e| (std::cmp::Reverse(e.last_activity), e.id));
        for task in tasks {
            let Some(project) = task.project.and_then(|id| index.get(&id)).map(|&i| &mut projects[i]) else {
                continue;
            };
            let seat = Seat::from_author(task.author == me, task.actor);
            if !project.authors.contains(&seat) {
                project.authors.push(seat);
            }
            // L0 shows every task in Open. The remaining columns already consume their own lists.
            project.columns[0].push(Card {
                number: format!("#{}", model.state.task_numbers[&task.id]),
                title: task.text(field::TITLE).to_owned(),
                seat,
            });
        }
        let mut names = std::collections::BTreeMap::new();
        let messages = model
            .state
            .chat
            .iter()
            .map(|m| {
                let addressed = addresses_my_ai(m, &me);
                let name = names.entry((m.author, m.actor))
                    .or_insert_with(|| model.author_label(&m.author, m.actor));
                Message {
                    seat: Seat::from_author(m.author == me, m.actor),
                    name: name.clone(),
                    time: clock.hm(m.wall_ms),
                    text: if addressed {
                        format!("@ai {}", m.chat.text)
                    } else {
                        m.chat.text.clone()
                    },
                    addressed,
                }
            })
            .collect();
        let endpoint = model.home.endpoint_path();
        let current = model.current_project().map(|p| p.id);
        Self {
            index,
            writable: model.state.active_member(&me),
            removable: current.filter(|id| model.can_remove_project(*id)),
            removed: None,
            project_name_draft: None,
            current,
            projects,
            messages,
            listening,
            connect: AiCommands {
                terminal: format!("makepad-remote app {} call work.next", endpoint.display()),
                mcp: format!("makepad-remote mcp {}", endpoint.display()),
            },
        }
    }

    /// Typing changes the document author. No chat, project or task list needs
    /// rebuilding for a plain text edit.
    pub fn update_document_author(&mut self, model: &Model, id: Id) {
        let doc = &model.state.entities[&id];
        let Some(&i) = doc.project.as_ref().and_then(|project| self.index.get(project)) else { return };
        if let Some(row) = self.projects[i].docs.iter_mut().find(|row| row.id == id) {
            let (author, actor) = model.document_author(doc);
            row.editor = Seat::from_author(author == model.me(), actor);
        }
    }

    pub fn selected(&self) -> Option<&Project> {
        self.index.get(&self.current?).map(|&i| &self.projects[i])
    }

    pub fn project_index(&self, id: Id) -> Option<usize> {
        self.index.get(&id).copied()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceStatus {
    NotConnected,
    Offline(String),
    Reaching(u64),
    /// The device refused the last try: it may have been reset, or removed this link.
    NotProven,
    Connected,
}

impl DeviceStatus {
    pub fn text(&self) -> String {
        match self {
            Self::NotConnected => "not connected".into(),
            Self::Offline(when) => format!("last seen {when}"),
            Self::Reaching(secs) => format!("Reaching… {secs} s"),
            Self::NotProven => "not proven".into(),
            Self::Connected => "connected".into(),
        }
    }
}

/// The device for which today's link worker and known-device store expose a presence signal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TablePeer {
    pub name: String,
    pub ai_name: String,
    pub status: DeviceStatus,
    /// When a local day or zone change can change the displayed date.
    pub refresh_after_secs: Option<u64>,
}

impl TablePeer {
    /// `reaching`: a known device a worker is reaching with no words, since when (unix seconds),
    /// and whether it refused the last try.
    pub fn read(linked_name: Option<&str>, reaching: Option<(&str, u64, bool)>, last_link: Option<(&str, u64)>, clock: &LocalClock, now: u64) -> Self {
        let (name, status, refresh_after_secs) = if let Some(name) = linked_name {
            (name, DeviceStatus::Connected, None)
        } else if let Some((name, since, refused)) = reaching {
            (name, if refused { DeviceStatus::NotProven } else { DeviceStatus::Reaching(now.saturating_sub(since)) }, None)
        } else if let Some((name, last)) = last_link {
            let (day, time) = clock.local_day_time(last);
            let (today, now_time) = clock.local_day_time(now);
            let when = if day == today { hours_minutes(time) } else { calendar_date(day) };
            let refresh = (day >= today).then(|| clock.next_day_check(now, now_time));
            (name, DeviceStatus::Offline(when), refresh)
        } else {
            ("", DeviceStatus::NotConnected, None)
        };
        Self {
            name: peer_label(name, Actor::Human),
            ai_name: peer_label(name, Actor::Ai),
            status,
            refresh_after_secs,
        }
    }

    fn same_phase(&self, other: &Self) -> bool {
        self.name == other.name && std::mem::discriminant(&self.status) == std::mem::discriminant(&other.status)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LinkPrompt {
    #[default]
    None,
    Linking,
    CheckWords,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Presence {
    pub peer: TablePeer,
    pub listening: bool,
    pub prompt: LinkPrompt,
}

impl Presence {
    /// `linked` and `reaching` carry the names this device keeps for the other one: a reconnect's
    /// proof carries none.
    pub fn read(connect: &ConnectView, linked: Option<&str>, reaching: Option<(&str, u64, bool)>, last: Option<(&str, u64)>,
        clock: &LocalClock, now: u64, listening: bool, checked_words: bool) -> Self {
        let mut peer = TablePeer::read(linked, reaching, last, clock, now);
        let prompt = if linked.is_some() {
            if checked_words { LinkPrompt::None } else { LinkPrompt::CheckWords }
        } else if connect.open {
            // A known device being reached again keeps its name; only a first contact is nameless.
            if let (None, Some(Status::Reaching { secs })) = (reaching, &connect.status) {
                let secs = *secs;
                peer = TablePeer::read(None, None, None, clock, now);
                peer.status = DeviceStatus::Reaching(secs);
            }
            LinkPrompt::Linking
        } else {
            LinkPrompt::None
        };
        Self { peer, listening, prompt }
    }
}

/// Hold each changing signal independently. Elapsed counts and calendar rollovers update
/// within the same phase without restarting its timer.
struct Held<T> {
    shown: Option<T>,
    pending: Option<(T, u64)>,
}

impl<T> Default for Held<T> {
    fn default() -> Self { Self { shown: None, pending: None } }
}

impl<T: Clone> Held<T> {
    fn sample(&mut self, next: T, now: u64, immediate: bool, same: fn(&T, &T) -> bool) -> T {
        if immediate || self.shown.as_ref().is_none_or(|shown| same(shown, &next)) {
            self.pending = None;
        } else {
            let deadline = self.pending.as_ref().filter(|(waiting, _)| same(waiting, &next))
                .map_or_else(|| now.saturating_add(1000), |(_, deadline)| *deadline);
            if now < deadline {
                self.pending = Some((next, deadline));
                return self.shown.as_ref().unwrap().clone();
            }
            self.pending = None;
        }
        self.shown = Some(next.clone());
        next
    }

    fn deadline(&self) -> Option<u64> { self.pending.as_ref().map(|(_, deadline)| *deadline) }
}

#[derive(Default)]
pub struct PresenceHold {
    peer: Held<TablePeer>,
    listening: Held<bool>,
    prompt: Held<LinkPrompt>,
}

impl PresenceHold {
    pub fn listening(&self) -> bool { self.listening.shown.unwrap_or(false) }

    pub fn update(&mut self, next: Presence, now: u64) -> Presence {
        let connected = next.peer.status == DeviceStatus::Connected;
        let dropped = !connected && self.peer.shown.as_ref().is_some_and(|p| p.status == DeviceStatus::Connected);
        Presence {
            peer: self.peer.sample(next.peer, now, connected || dropped, TablePeer::same_phase),
            listening: self.listening.sample(next.listening, now, false, PartialEq::eq),
            prompt: self.prompt.sample(next.prompt, now, connected || dropped, PartialEq::eq),
        }
    }

    pub fn deadline(&self) -> Option<u64> {
        [self.peer.deadline(), self.listening.deadline(), self.prompt.deadline()].into_iter().flatten().min()
    }
}

fn hours_minutes(time: i64) -> String {
    format!("{:02}:{:02}", time / 3600, time / 60 % 60)
}

// Howard Hinnant's civil_from_days, as in platform's script clock. Day zero is 1970-01-01.
fn calendar_date(day: i64) -> String {
    let z = day + 719468;
    let era = z.div_euclid(146097);
    let day_of_era = z.rem_euclid(146097);
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let march_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * march_month + 2) / 5 + 1;
    let month = if march_month < 10 { march_month + 3 } else { march_month - 9 };
    let year = era * 400 + year_of_era + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

// The host offset is the fallback on hosts without a system zone file. Unix zone files also
// retain historical offset changes, so yesterday's chat keeps its own daylight-saving time.
#[derive(Default)]
pub struct LocalClock {
    initial: Option<i32>,
    changes: Vec<(i64, i32)>,
}

impl LocalClock {
    pub fn load() -> Self {
        let path = match std::env::var_os("TZ") {
            Some(zone) => {
                let zone = zone.to_string_lossy();
                std::path::Path::new("/usr/share/zoneinfo").join(zone.trim_start_matches(':'))
            }
            None => std::path::PathBuf::from("/etc/localtime"),
        };
        std::fs::read(path)
            .ok()
            .and_then(|bytes| Self::from_zone(&bytes))
            .unwrap_or_default()
    }

    fn hm(&self, ms: u64) -> String {
        hours_minutes(self.local_day_time(ms / 1000).1)
    }

    fn transition_index(&self, secs: u64) -> usize {
        let instant = i64::try_from(secs).unwrap_or(i64::MAX);
        self.changes.partition_point(|&(at, _)| at <= instant)
    }

    fn local_day_time(&self, secs: u64) -> (i64, i64) {
        let index = self.transition_index(secs);
        let offset = index.checked_sub(1).map(|i| self.changes[i].1).or(self.initial);
        let offset = offset
            .map(i64::from)
            .unwrap_or_else(makepad_widgets::makepad_platform::script::timer::script_local_utc_offset_secs);
        let time = (secs % 86_400) as i64 + offset;
        ((secs / 86_400) as i64 + time.div_euclid(86_400), time.rem_euclid(86_400))
    }

    fn next_day_check(&self, now: u64, time: i64) -> u64 {
        let midnight = (86_400 - time) as u64;
        self.changes.get(self.transition_index(now)).map_or(midnight, |&(at, _)| midnight.min(at as u64 - now))
    }

    fn from_zone(bytes: &[u8]) -> Option<Self> {
        // TZif v2 and later lead with a legacy 32-bit block before the 64-bit table.
        let mut data = bytes;
        for width in [4, 8] {
            if data.get(..4)? != b"TZif" {
                return None;
            }
            let mut fields = data
                .get(20..44)?
                .chunks_exact(4)
                .map(|v| u32::from_be_bytes(v.try_into().unwrap()) as usize);
            let (ut, std, leaps, times, types, chars) = (
                fields.next()?,
                fields.next()?,
                fields.next()?,
                fields.next()?,
                fields.next()?,
                fields.next()?,
            );
            let mut block = data.get(44..)?;
            let mut take = |n: usize| {
                let (prefix, rest) = block.split_at_checked(n)?;
                block = rest;
                Some(prefix)
            };
            let instants = take(times.checked_mul(width)?)?;
            let indices = take(times)?;
            let offsets = take(types.checked_mul(6)?)?;
            if width == 4 && data[4] != 0 {
                take(chars)?;
                take(leaps.checked_mul(width + 4)?)?;
                take(std)?;
                take(ut)?;
                data = block;
                continue;
            }
            let offset = |kind: usize| -> Option<i32> {
                let i = kind * 6;
                Some(i32::from_be_bytes(offsets.get(i..i + 4)?.try_into().ok()?))
            };
            let mut changes = Vec::with_capacity(times);
            for (instant, &kind) in instants.chunks_exact(width).zip(indices) {
                let at = if width == 4 {
                    i64::from(i32::from_be_bytes(instant.try_into().ok()?))
                } else {
                    i64::from_be_bytes(instant.try_into().ok()?)
                };
                changes.push((at, offset(usize::from(kind))?));
            }
            return Some(Self {
                initial: offset(0),
                changes,
            });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link_worker::Linked;

    #[test]
    fn the_table_follows_the_link_and_the_local_day() {
        let clock = LocalClock { initial: Some(3600), changes: Vec::new() };
        let last = Some(("Earlier", 86_100));
        let table = |live, last, now| TablePeer::read(live, None, last, &clock, now);
        let live = table(Some("Renée"), last, 86_500);
        assert_eq!(live.status, DeviceStatus::Connected, "a current link shows the present tile and the good dot");
        assert_eq!((live.name.as_str(), live.status.text().as_str(), live.ai_name.as_str()), ("Renée", "connected", "Renée’s AI"),
            "the live proof name takes precedence over the most recent pin");
        assert_eq!(table(Some(" yOu "), last, 86_500).name, "Peer", "the live tile also guards the local seat name");

        let known = table(None, last, 86_500);
        assert!(matches!(known.status, DeviceStatus::Offline(_)), "an earlier link uses the absent tile");
        assert_eq!((known.name.as_str(), known.ai_name.as_str()), ("Earlier", "Earlier’s AI"), "both absent seats show the last pin");
        assert_eq!(known.status.text(), "last seen 00:55", "links on the same local day show the chat time across UTC midnight");
        assert_eq!(known.refresh_after_secs, Some(169_200 - 86_500), "the time changes to a date at local midnight");
        let yesterday = table(None, last, 169_200);
        assert_eq!(yesterday.status.text(), "last seen 1970-01-02", "an older link shows its local calendar date");
        assert!(yesterday.refresh_after_secs.is_none(), "an older date needs no midnight refresh");

        let reaching = TablePeer::read(None, Some(("Renée", 86_480, false)), last, &clock, 86_500);
        assert_eq!((reaching.name.as_str(), reaching.status.clone()), ("Renée", DeviceStatus::Reaching(20)),
            "a device being reached again keeps its name and says for how long, not when it was last linked");
        let refused = TablePeer::read(None, Some(("Renée", 86_480, true)), last, &clock, 86_500);
        assert_eq!(refused.status.text(), "not proven", "a refused gate shows on the seat; its plain line stays in the panel");

        let unknown = table(None, None, 86_500);
        assert_eq!(unknown.status, DeviceStatus::NotConnected, "an unknown device has no presence");
        assert_eq!((unknown.name.as_str(), unknown.status.text().as_str(), unknown.ai_name.as_str()), ("Peer", "not connected", "Peer’s AI"),
            "unoccupied seats keep their original labels");
    }

    #[test]
    fn caption_tracks_first_contact_waits_and_drops_independently() {
        let clock = LocalClock { initial: Some(0), changes: Vec::new() };
        let mut link = ConnectView::default();
        let read = |link: &ConnectView, listening, checked| {
            let linked = link.linked.as_ref().map(|p| p.peer_name.as_str());
            Presence::read(link, linked, None, Some(("Sam", 31_080)), &clock, 36_000, listening, checked)
        };
        let mut hold = PresenceHold::default();
        let idle = hold.update(read(&link, false, false), 0);
        assert_eq!(idle.peer.status.text(), "last seen 08:38");
        assert_eq!((idle.prompt, idle.listening), (LinkPrompt::None, false));

        link.open = true;
        link.status = Some(Status::WaitingForWords);
        assert_eq!(hold.update(read(&link, true, false), 10), idle, "brief signals leave the caption alone");
        link.status = Some(Status::Reaching { secs: 0 });
        assert_eq!(hold.update(read(&link, false, false), 250), idle, "a short AI wait is cancelled");
        assert_eq!(hold.deadline(), Some(1010));
        let linking = hold.update(read(&link, true, false), 1010);
        assert_eq!((linking.prompt, linking.peer.status), (LinkPrompt::Linking, idle.peer.status));
        link.status = Some(Status::Reaching { secs: 1 });
        let reaching = hold.update(read(&link, true, false), 1250);
        assert_eq!(reaching.peer.status, DeviceStatus::Reaching(1), "an elapsed tick doesn't postpone reaching");
        assert!(!reaching.listening);
        assert!(hold.update(read(&link, true, false), 2010).listening);

        link.linked = Some(Linked { peer_name: "Sam".into(), own_words: "first".into(), peer_words: "second".into() });
        let live = hold.update(read(&link, true, false), 2011);
        assert_eq!((live.peer.status, live.prompt), (DeviceStatus::Connected, LinkPrompt::CheckWords));
        assert_eq!(hold.update(read(&link, true, true), 2012).prompt, LinkPrompt::None);
        let dropped = hold.update(read(&ConnectView::default(), false, false), 2013);
        assert_eq!(dropped.peer.status.text(), "last seen 08:38", "a dropped device is immediate");
        assert_eq!(dropped.prompt, LinkPrompt::None);
        assert!(dropped.listening, "the device change doesn't bypass the independent AI hold");
        assert!(!hold.update(read(&ConnectView::default(), false, false), 3013).listening);
        let again = ConnectView { open: true, status: Some(Status::Reaching { secs: 3 }), ..ConnectView::default() };
        let known = Presence::read(&again, None, Some(("Sam", 35_990, false)), None, &clock, 36_000, false, false);
        assert_eq!((known.peer.name.as_str(), known.peer.status), ("Sam", DeviceStatus::Reaching(10)),
            "a known device's reconnect punch keeps its name and counts from when reaching began");
        assert_eq!(hold.deadline(), None, "idle presence has no ticking timer");
    }

    #[test]
    fn device_words_check_follows_the_pin_across_reopen_and_forget() {
        use crate::link_worker::FromWorker;
        use makepad_work_core::{home::Home, peers::{How, Peers}};

        let mut nonce = [0; 16];
        makepad_network::tls::os_random(&mut nonce).unwrap();
        let dir = std::env::temp_dir().join(format!("makepad-work-checked-{}", makepad_work_core::hex(&nonce)));
        let home = Home::open(&dir).unwrap();
        let mut peers = Peers::open(&home).unwrap();
        let (first, mut second) = ([4; 65], [4; 65]);
        second[1] = 5;
        let prompt = |peers: &mut Peers, key, reconnected| {
            let mut link = ConnectView::default();
            if reconnected { link.apply(&FromWorker::Reconnecting { device: key, since: 2 }); }
            link.apply(&FromWorker::Linked { peer_device: key, peer_name: "Peer".into(), version: 1,
                own_words: "one".into(), peer_words: "two".into(), reconnected });
            peers.linked(key, How::Words, 1, "Peer", 3).unwrap();
            let device = peers.device(link.peer.as_ref().unwrap()).unwrap();
            Presence::read(&link, Some(&device.name), None, None, &LocalClock::default(), 3, false, device.checked).prompt
        };
        assert_eq!(prompt(&mut peers, first, false), LinkPrompt::CheckWords);
        peers.set_checked(&first, true).unwrap();
        drop((peers, home));
        let home = Home::open(&dir).unwrap();
        let mut peers = Peers::open(&home).unwrap();
        assert_eq!(prompt(&mut peers, first, true), LinkPrompt::None, "a reconnect keeps this pin's check");
        assert_eq!(prompt(&mut peers, second, false), LinkPrompt::CheckWords, "a different key with the same name needs a check");
        let mut fresh = Peers::forget(&home, 4).unwrap();
        assert_eq!(prompt(&mut fresh, first, false), LinkPrompt::CheckWords, "forgetting clears the check");
        drop(home);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn chat_times_follow_the_offset_at_the_message() {
        let mut zone = b"TZif\0".to_vec();
        zone.resize(20, 0);
        for count in [0u32, 0, 0, 1, 2, 8] {
            zone.extend(count.to_be_bytes());
        }
        zone.extend(3600i32.to_be_bytes());
        zone.push(1);
        for (offset, flags) in [(3600i32, [0, 0]), (7200, [1, 4])] {
            zone.extend(offset.to_be_bytes());
            zone.extend(flags);
        }
        zone.extend(b"STD\0DST\0");
        let clock = LocalClock::from_zone(&zone).unwrap();
        assert_eq!(clock.hm(3_599_000), "01:59");
        assert_eq!(clock.hm(3_600_000), "03:00");
        assert!(LocalClock::from_zone(&zone[..48]).is_none(), "a truncated transition is unreadable");
    }
}
