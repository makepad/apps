//! What the search box has already been asked for (donor `recent_searches`,
//! deleted in ef48f3d9): Up in the search box steps back through the
//! queries that ran, newest first, and Down steps forward again, back to the
//! text that was being typed before the walk began.
//!
//! One list per catalogue (a lane: music, video, the web recordings), since
//! a query that belongs to one is noise in another. A repeat moves to the
//! front instead of taking a second place. Pure: the host keeps the text.

/// One lane's queries, newest first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RecentSearches {
    queries: Vec<String>,
}

impl RecentSearches {
    pub fn queries(&self) -> &[String] {
        &self.queries
    }

    /// Remember a query that ran. Trimmed; empty is not a query.
    pub fn note(&mut self, query: &str) -> bool {
        let query = query.trim();
        if query.is_empty() || self.queries.first().is_some_and(|first| first == query) {
            return false;
        }
        self.queries.retain(|held| held != query);
        self.queries.insert(0, query.to_string());
        true
    }
}

/// Where a walk through `len` queries goes next from `at` (`None` is the
/// text being typed). Back stops on the oldest; forward off the newest
/// returns to the typed text.
pub fn step(at: Option<usize>, len: usize, older: bool) -> Option<usize> {
    if len == 0 {
        return None;
    }
    match (at, older) {
        (None, true) => Some(0),
        (Some(at), true) => Some((at + 1).min(len - 1)),
        (None, false) | (Some(0), false) => None,
        (Some(at), false) => Some(at - 1),
    }
}

/// Every lane's list: `lane query` a line, newest first within a lane.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Recalls {
    lanes: Vec<(String, RecentSearches)>,
}

impl Recalls {
    pub fn lane(&self, name: &str) -> &[String] {
        self.lanes.iter().find(|(held, _)| held == name).map_or(&[], |(_, list)| list.queries())
    }

    pub fn lane_mut(&mut self, name: &str) -> &mut RecentSearches {
        let at = match self.lanes.iter().position(|(held, _)| held == name) {
            Some(at) => at,
            None => {
                self.lanes.push((name.to_string(), RecentSearches::default()));
                self.lanes.len() - 1
            }
        };
        &mut self.lanes[at].1
    }

    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for (lane, list) in &self.lanes {
            for query in list.queries() {
                out.push_str(&format!("{lane} {}\n", query.replace(['\n', '\r'], " ")));
            }
        }
        out
    }

    /// Only the first space parts the lane from the query: a query may
    /// hold spaces.
    pub fn from_text(text: &str) -> Recalls {
        let mut out = Recalls::default();
        for line in text.lines() {
            let Some((lane, query)) = line.trim_end_matches('\r').split_once(' ') else { continue };
            if lane.is_empty() || query.trim().is_empty() {
                continue;
            }
            let list = out.lane_mut(lane);
            if !list.queries.iter().any(|held| held == query) {
                list.queries.push(query.to_string());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repeat_moves_to_the_front_and_blank_is_not_a_query() {
        let mut list = RecentSearches::default();
        for query in ["drums", "  ", "vocal", " drums "] {
            list.note(query);
        }
        assert_eq!(list.queries(), ["drums", "vocal"]);
    }

    #[test]
    fn the_walk_stops_at_the_oldest_and_comes_back_to_the_typed_text() {
        assert_eq!(step(None, 3, true), Some(0));
        assert_eq!(step(Some(2), 3, true), Some(2), "no wrap");
        assert_eq!(step(Some(1), 3, false), Some(0));
        assert_eq!(step(Some(0), 3, false), None, "the typed text again");
        assert_eq!(step(None, 0, true), None);
    }

    #[test]
    fn lanes_round_trip_and_a_query_keeps_its_spaces() {
        let mut recalls = Recalls::default();
        recalls.lane_mut("music").note("older one");
        recalls.lane_mut("music").note("deep house");
        recalls.lane_mut("video").note("rain");
        let back = Recalls::from_text(&recalls.to_text());
        assert_eq!(back, recalls);
        assert_eq!(back.lane("music"), ["deep house", "older one"]);
        assert!(back.lane("web").is_empty());
        assert_eq!(Recalls::from_text("nolane\n music x\nmusic  \n").lane("music"), Vec::<String>::new().as_slice());
    }
}
