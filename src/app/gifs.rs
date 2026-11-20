//! GIF picker state: trending GIFs on open, a debounced search as the user
//! types. Picking one sends its Tenor URL, which Discord embeds as a GIF.

use iced::Point;

use crate::net::model::Gif;

/// Wait this long after the last keystroke before searching.
pub const SEARCH_DEBOUNCE_MS: u64 = 350;

#[derive(Debug, Clone)]
pub struct GifPicker {
    /// Where the composer button was clicked (the popup opens above it).
    pub at: Point,
    pub query: String,
    pub results: Vec<Gif>,
    pub loading: bool,
    pub error: Option<String>,
    /// Bumped per keystroke / request so stale timers and responses are
    /// dropped.
    pub generation: u64,
}

impl GifPicker {
    /// A picker that is loading the trending GIFs; returns it with the
    /// generation to tag that request with.
    pub fn open(at: Point) -> (Self, u64) {
        let p = Self {
            at,
            query: String::new(),
            results: Vec::new(),
            loading: true,
            error: None,
            generation: 1,
        };
        (p, 1)
    }

    /// The search text changed; returns the generation of the debounce
    /// timer to start.
    pub fn set_query(&mut self, q: String) -> u64 {
        self.query = q;
        self.generation += 1;
        self.generation
    }

    /// A debounce timer fired: returns the query to search for if no
    /// keystroke came after it.
    pub fn due(&mut self, generation: u64) -> Option<String> {
        if generation != self.generation {
            return None;
        }
        self.loading = true;
        self.error = None;
        Some(self.query.clone())
    }

    /// Applies a response; stale generations are ignored.
    pub fn finish(&mut self, generation: u64, result: Result<Vec<Gif>, String>) {
        if generation != self.generation {
            return;
        }
        self.loading = false;
        match result {
            Ok(g) => self.results = g,
            Err(e) => self.error = Some(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gif(n: u32) -> Gif {
        Gif {
            url: format!("https://tenor.com/view/{n}"),
            preview: format!("https://media.tenor.com/{n}.png"),
            width: 200,
            height: 100,
        }
    }

    #[test]
    fn open_loads_trending() {
        let (mut p, g) = GifPicker::open(Point::ORIGIN);
        assert!(p.loading && p.query.is_empty());
        p.finish(g, Ok(vec![gif(1)]));
        assert!(!p.loading);
        assert_eq!(p.results.len(), 1);
    }

    #[test]
    fn only_the_last_keystroke_searches() {
        let (mut p, _) = GifPicker::open(Point::ORIGIN);
        let first = p.set_query("ca".into());
        let last = p.set_query("cat".into());
        assert_eq!(p.due(first), None);
        assert_eq!(p.due(last).as_deref(), Some("cat"));
        assert!(p.loading);
    }

    #[test]
    fn stale_responses_are_dropped() {
        let (mut p, trending) = GifPicker::open(Point::ORIGIN);
        let g = p.set_query("cat".into());
        // Trending arrives after the user started typing: ignored.
        p.finish(trending, Ok(vec![gif(1)]));
        assert!(p.results.is_empty());
        assert!(p.due(g).is_some());
        p.finish(g, Err("offline".into()));
        assert_eq!(p.error.as_deref(), Some("offline"));
    }
}
