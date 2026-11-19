//! Message search state (results panel).

use crate::net::model::*;
use crate::net::rest::{SEARCH_PAGE, SearchScope};

#[derive(Debug, Clone, Default)]
pub struct SearchState {
    /// The search box in the header is open.
    pub open: bool,
    pub input: String,
    /// Query of the results shown (what was submitted).
    pub query: String,
    pub scope: Option<SearchScope>,
    pub offset: u32,
    pub total: u32,
    pub results: Vec<Message>,
    pub loading: bool,
    pub error: Option<String>,
    /// Bumped per request so late responses of old searches are dropped.
    pub generation: u64,
}

impl SearchState {
    /// Starts a search; returns `(generation, scope, query, offset)` to run.
    pub fn begin(
        &mut self,
        scope: SearchScope,
        offset: u32,
    ) -> Option<(u64, SearchScope, String, u32)> {
        let q = self.input.trim().to_owned();
        if q.is_empty() {
            return None;
        }
        self.generation += 1;
        if offset == 0 || self.query != q {
            self.results.clear();
            self.total = 0;
        }
        self.query.clone_from(&q);
        self.scope = Some(scope);
        self.offset = offset;
        self.loading = true;
        self.error = None;
        Some((self.generation, scope, q, offset))
    }

    /// Applies a response; stale generations are ignored.
    pub fn finish(&mut self, generation: u64, result: Result<SearchPage, String>) {
        if generation != self.generation {
            return;
        }
        self.loading = false;
        match result {
            Ok(page) => {
                self.total = page.total;
                self.results = page.messages;
            }
            Err(e) => self.error = Some(e),
        }
    }

    pub fn page(&self) -> u32 {
        self.offset / SEARCH_PAGE + 1
    }

    pub fn pages(&self) -> u32 {
        self.total.div_ceil(SEARCH_PAGE).max(1)
    }

    pub fn has_next(&self) -> bool {
        self.offset + SEARCH_PAGE < self.total
    }

    pub fn has_prev(&self) -> bool {
        self.offset > 0
    }

    /// Closes the panel and forgets results.
    pub fn close(&mut self) {
        *self = Self {
            generation: self.generation + 1,
            ..Default::default()
        };
    }

    /// Whether the results panel replaces the member list.
    pub fn panel_visible(&self) -> bool {
        self.scope.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_input(q: &str) -> SearchState {
        SearchState {
            input: q.into(),
            ..Default::default()
        }
    }

    fn page(total: u32, n: usize) -> SearchPage {
        SearchPage {
            total,
            messages: (0..n)
                .map(|i| {
                    crate::mock::text_message(
                        Snowflake(1),
                        Snowflake(i as u64 + 1),
                        crate::mock::user(2),
                        "x",
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn blank_query_does_nothing() {
        let mut s = with_input("   ");
        assert!(s.begin(SearchScope::Channel(Snowflake(1)), 0).is_none());
        assert!(!s.panel_visible());
    }

    #[test]
    fn stale_responses_are_dropped() {
        let mut s = with_input("rust");
        let (g1, ..) = s.begin(SearchScope::Guild(Snowflake(9)), 0).unwrap();
        s.input = "iced".into();
        let (g2, ..) = s.begin(SearchScope::Guild(Snowflake(9)), 0).unwrap();
        s.finish(g1, Ok(page(100, 3)));
        assert!(s.loading && s.results.is_empty());
        s.finish(g2, Ok(page(60, 3)));
        assert!(!s.loading);
        assert_eq!(s.total, 60);
        assert_eq!(s.results.len(), 3);
        assert_eq!(s.query, "iced");
    }

    #[test]
    fn paging() {
        let mut s = with_input("x");
        let (g, ..) = s.begin(SearchScope::Channel(Snowflake(1)), 0).unwrap();
        s.finish(g, Ok(page(60, 25)));
        assert_eq!((s.page(), s.pages()), (1, 3));
        assert!(s.has_next() && !s.has_prev());
        let (g, _, _, off) = s.begin(SearchScope::Channel(Snowflake(1)), 25).unwrap();
        assert_eq!(off, 25);
        s.finish(g, Ok(page(60, 25)));
        assert_eq!(s.page(), 2);
        assert!(s.has_next() && s.has_prev());
    }

    #[test]
    fn errors_and_close() {
        let mut s = with_input("x");
        let (g, ..) = s.begin(SearchScope::Channel(Snowflake(1)), 0).unwrap();
        s.finish(g, Err("boom".into()));
        assert_eq!(s.error.as_deref(), Some("boom"));
        let gen_before = s.generation;
        s.close();
        assert!(!s.panel_visible() && s.input.is_empty());
        assert!(s.generation > gen_before);
    }
}
