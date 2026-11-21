//! State of the "Add a Server" dialogs (create / join with an invite) and
//! the Discover page. Pure reducers, unit-tested.

use std::path::PathBuf;

use crate::net::extras::{
    DISCOVER_PAGE, DiscoverCategory, DiscoverGuild, DiscoverPage, InvitePreview, parse_invite_code,
};
use crate::net::model::GuildId;

/// Discord's limits for server names.
pub const NAME_MIN: usize = 2;
pub const NAME_MAX: usize = 100;

#[derive(Debug, Clone, PartialEq)]
pub enum ServerDialog {
    /// "Create My Own" / "Join a Server".
    Choose,
    Create(CreateForm),
    Join(JoinForm),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CreateForm {
    pub name: String,
    pub icon_path: Option<PathBuf>,
    /// The chosen icon as a `data:` URI (checked when picked).
    pub icon_uri: Option<String>,
    pub busy: bool,
    pub error: Option<String>,
}

impl CreateForm {
    /// The official default: "<name>'s server".
    pub fn for_user(display_name: &str) -> Self {
        let name = if display_name.trim().is_empty() {
            String::new()
        } else {
            format!("{}'s server", display_name.trim())
        };
        Self {
            name: name.chars().take(NAME_MAX).collect(),
            ..Default::default()
        }
    }

    pub fn name_len(&self) -> usize {
        self.name.trim().chars().count()
    }

    pub fn can_submit(&self) -> bool {
        !self.busy && (NAME_MIN..=NAME_MAX).contains(&self.name_len())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum JoinStep {
    Idle,
    Resolving,
    Preview(InvitePreview),
    Joining(InvitePreview),
}

#[derive(Debug, Clone, PartialEq)]
pub struct JoinForm {
    pub input: String,
    pub step: JoinStep,
    pub error: Option<String>,
    generation: u64,
}

impl Default for JoinForm {
    fn default() -> Self {
        Self {
            input: String::new(),
            step: JoinStep::Idle,
            error: None,
            generation: 0,
        }
    }
}

impl JoinForm {
    /// Typing invalidates a preview.
    pub fn set_input(&mut self, s: String) {
        self.input = s;
        self.generation += 1;
        self.step = JoinStep::Idle;
        self.error = None;
    }

    pub fn code(&self) -> Option<String> {
        parse_invite_code(&self.input)
    }

    /// "Continue": returns `(generation, code)` to resolve.
    pub fn begin_resolve(&mut self) -> Option<(u64, String)> {
        if matches!(self.step, JoinStep::Resolving | JoinStep::Joining(_)) {
            return None;
        }
        let Some(code) = self.code() else {
            self.error = Some("That does not look like an invite link or code.".into());
            return None;
        };
        self.generation += 1;
        self.step = JoinStep::Resolving;
        self.error = None;
        Some((self.generation, code))
    }

    pub fn resolved(&mut self, generation: u64, result: Result<InvitePreview, String>) {
        if generation != self.generation || self.step != JoinStep::Resolving {
            return;
        }
        match result {
            Ok(p) => self.step = JoinStep::Preview(p),
            Err(e) => {
                self.step = JoinStep::Idle;
                self.error = Some(e);
            }
        }
    }

    /// "Join Server": returns `(generation, code)` to join.
    pub fn begin_join(&mut self) -> Option<(u64, String)> {
        let JoinStep::Preview(p) = &self.step else {
            return None;
        };
        let code = p.code.clone();
        self.generation += 1;
        self.step = JoinStep::Joining(p.clone());
        self.error = None;
        Some((self.generation, code))
    }

    /// A failed join keeps the preview so the user can retry or go back.
    pub fn join_failed(&mut self, generation: u64, message: String) {
        if generation != self.generation {
            return;
        }
        if let JoinStep::Joining(p) = &self.step {
            self.step = JoinStep::Preview(p.clone());
            self.error = Some(message);
        }
    }

    pub fn is_busy(&self) -> bool {
        matches!(self.step, JoinStep::Resolving | JoinStep::Joining(_))
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }
}

/// Wording for a failed invite request.
pub fn invite_error_text(e: &crate::error::Error, joining: bool) -> String {
    use crate::error::Error;
    match e {
        Error::CaptchaRequired(_) => e.user_message(),
        Error::Http { status: 404, .. } => "This invite is invalid or has expired.".into(),
        Error::Http { status: 403, .. } if joining => {
            "You cannot join this server (you may be banned from it).".into()
        }
        Error::Http {
            status: 400,
            message,
        } if joining && message.to_lowercase().contains("maximum") => {
            "You are in the maximum number of servers.".into()
        }
        Error::Http { status: 429, .. } => {
            "Discord is rate limiting invites. Wait a minute and try again.".into()
        }
        other => other.user_message(),
    }
}

// ---------------------------------------------------------------------------
// Discover
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct DiscoverState {
    /// Text in the search box.
    pub input: String,
    /// Query of the listing shown.
    pub query: String,
    pub category: Option<u32>,
    pub categories: Vec<DiscoverCategory>,
    pub guilds: Vec<DiscoverGuild>,
    pub total: Option<u32>,
    pub loading: bool,
    pub error: Option<String>,
    /// Server being joined.
    pub joining: Option<GuildId>,
    /// Server whose details card is open.
    pub preview: Option<GuildId>,
    generation: u64,
    /// Offset of the request in flight.
    pending_offset: u32,
}

impl DiscoverState {
    /// Starts a listing request; `offset == 0` replaces the grid.
    /// Returns `(generation, query, category, offset)`.
    pub fn begin(&mut self, offset: u32) -> (u64, String, Option<u32>, u32) {
        self.generation += 1;
        if offset == 0 {
            self.query = self.input.trim().to_owned();
            self.guilds.clear();
            self.total = None;
        }
        self.pending_offset = offset;
        self.loading = true;
        self.error = None;
        (self.generation, self.query.clone(), self.category, offset)
    }

    pub fn finish(&mut self, generation: u64, result: Result<DiscoverPage, String>) {
        if generation != self.generation {
            return;
        }
        self.loading = false;
        match result {
            Ok(page) => {
                self.total = page.total.or(self.total);
                for g in page.guilds {
                    if !self.guilds.iter().any(|x| x.id == g.id) {
                        self.guilds.push(g);
                    }
                }
            }
            Err(e) => self.error = Some(e),
        }
    }

    pub fn select_category(&mut self, c: Option<u32>) -> bool {
        let changed = self.category != c;
        self.category = c;
        changed
    }

    /// Whether another page can be requested.
    pub fn has_more(&self) -> bool {
        if self.loading || self.error.is_some() {
            return false;
        }
        match self.total {
            Some(t) => (self.guilds.len() as u32) < t,
            None => {
                self.guilds.len() as u32 >= DISCOVER_PAGE
                    && (self.guilds.len() as u32).is_multiple_of(DISCOVER_PAGE)
            }
        }
    }

    pub fn next_offset(&self) -> u32 {
        self.guilds.len() as u32
    }

    pub fn guild(&self, id: GuildId) -> Option<&DiscoverGuild> {
        self.guilds.iter().find(|g| g.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;
    use crate::net::model::Snowflake;

    fn preview(code: &str) -> InvitePreview {
        InvitePreview {
            code: code.into(),
            guild_id: Snowflake(5),
            name: "Rustaceans".into(),
            icon: None,
            description: None,
            members: Some(10),
            online: Some(3),
            inviter: None,
            channel: None,
            verified: false,
            partnered: false,
        }
    }

    fn dg(id: u64) -> DiscoverGuild {
        DiscoverGuild {
            id: Snowflake(id),
            name: format!("g{id}"),
            description: None,
            icon: None,
            splash: None,
            members: None,
            online: None,
            verified: false,
            partnered: false,
        }
    }

    #[test]
    fn create_form_validates_the_name() {
        let f = CreateForm::for_user("Ferris");
        assert_eq!(f.name, "Ferris's server");
        assert!(f.can_submit());
        let mut f = CreateForm::default();
        assert!(!f.can_submit());
        f.name = "a".into();
        assert!(!f.can_submit());
        f.name = "  ab  ".into();
        assert!(f.can_submit());
        f.name = "x".repeat(101);
        assert!(!f.can_submit());
        f.name = "ok name".into();
        f.busy = true;
        assert!(!f.can_submit(), "no double submit");
        assert_eq!(CreateForm::for_user("  ").name, "");
    }

    #[test]
    fn join_flow_resolves_previews_and_joins() {
        let mut j = JoinForm::default();
        j.set_input("https://discord.gg/abc".into());
        let (g, code) = j.begin_resolve().unwrap();
        assert_eq!(code, "abc");
        assert!(j.is_busy());
        assert!(j.begin_resolve().is_none(), "already resolving");
        j.resolved(g, Ok(preview("abc")));
        assert_eq!(j.step, JoinStep::Preview(preview("abc")));
        let (g2, code) = j.begin_join().unwrap();
        assert_eq!(code, "abc");
        assert!(matches!(j.step, JoinStep::Joining(_)));
        // A failed join keeps the card and shows the message.
        j.join_failed(g2, "captcha".into());
        assert!(matches!(j.step, JoinStep::Preview(_)));
        assert_eq!(j.error.as_deref(), Some("captcha"));
        assert!(j.begin_join().is_some(), "retry is possible");
    }

    #[test]
    fn join_ignores_stale_answers_and_bad_input() {
        let mut j = JoinForm::default();
        j.set_input("not a link at all!".into());
        assert!(j.begin_resolve().is_none());
        assert!(j.error.is_some());
        j.set_input("code".into());
        let (g, _) = j.begin_resolve().unwrap();
        j.set_input("other".into());
        j.resolved(g, Ok(preview("code")));
        assert_eq!(j.step, JoinStep::Idle, "typing cancelled the old lookup");
        let (g, _) = j.begin_resolve().unwrap();
        j.resolved(g, Err("This invite is invalid or has expired.".into()));
        assert_eq!(j.step, JoinStep::Idle);
        assert!(j.error.as_deref().unwrap().contains("invalid"));
        assert!(
            j.begin_join().is_none(),
            "nothing to join without a preview"
        );
    }

    #[test]
    fn invite_errors_are_worded_for_people() {
        let http = |status: u16, m: &str| Error::Http {
            status,
            message: m.into(),
        };
        assert!(
            invite_error_text(
                &Error::CaptchaRequired(Box::new(
                    crate::net::captcha::CaptchaChallenge::parse(r#"{"captcha_key":[]}"#).unwrap()
                )),
                true
            )
            .contains("captcha")
        );
        assert!(
            invite_error_text(&http(404, "Unknown Invite"), false)
                .contains("invalid or has expired")
        );
        assert!(invite_error_text(&http(403, "x"), true).contains("banned"));
        assert!(invite_error_text(&http(403, "x"), false).contains("permission"));
        assert!(
            invite_error_text(&http(400, "Maximum number of guilds reached (100)"), true)
                .contains("maximum number of servers")
        );
        assert!(invite_error_text(&http(429, ""), true).contains("rate limiting"));
        assert!(invite_error_text(&Error::Net("x".into()), true).contains("Could not reach"));
    }

    #[test]
    fn discover_replaces_then_appends_and_dedupes() {
        let mut d = DiscoverState {
            input: "  rust ".into(),
            ..Default::default()
        };
        let (g, q, cat, off) = d.begin(0);
        assert_eq!((q.as_str(), cat, off), ("rust", None, 0));
        d.finish(
            g,
            Ok(DiscoverPage {
                guilds: vec![dg(1), dg(2)],
                total: Some(4),
            }),
        );
        assert!(d.has_more());
        assert_eq!(d.next_offset(), 2);
        let (g, ..) = d.begin(2);
        // Typing meanwhile does not change the query of paging requests.
        d.input = "other".into();
        d.finish(
            g,
            Ok(DiscoverPage {
                guilds: vec![dg(2), dg(3), dg(4)],
                total: Some(4),
            }),
        );
        assert_eq!(d.guilds.len(), 4);
        assert!(!d.has_more());
        assert_eq!(d.query, "rust");
        assert!(d.guild(Snowflake(3)).is_some());
    }

    #[test]
    fn discover_drops_stale_pages_and_reports_errors() {
        let mut d = DiscoverState::default();
        let (old, ..) = d.begin(0);
        assert!(d.select_category(Some(3)));
        assert!(!d.select_category(Some(3)));
        let (new, _, cat, _) = d.begin(0);
        assert_eq!(cat, Some(3));
        d.finish(
            old,
            Ok(DiscoverPage {
                guilds: vec![dg(9)],
                total: None,
            }),
        );
        assert!(d.guilds.is_empty() && d.loading);
        d.finish(new, Err("offline".into()));
        assert!(!d.loading && !d.has_more());
        assert_eq!(d.error.as_deref(), Some("offline"));
    }

    #[test]
    fn discover_without_a_total_pages_on_full_pages() {
        let mut d = DiscoverState::default();
        let (g, ..) = d.begin(0);
        d.finish(
            g,
            Ok(DiscoverPage {
                guilds: (0..DISCOVER_PAGE as u64).map(dg).collect(),
                total: None,
            }),
        );
        assert!(d.has_more());
        let (g, ..) = d.begin(DISCOVER_PAGE);
        d.finish(
            g,
            Ok(DiscoverPage {
                guilds: vec![dg(999)],
                total: None,
            }),
        );
        assert!(!d.has_more());
    }
}
