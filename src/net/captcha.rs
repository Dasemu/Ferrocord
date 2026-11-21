//! Discord's captcha demand and the headers of a retry.
//!
//! When Discord wants a captcha it answers HTTP 400 with:
//!
//! ```text
//! { "captcha_key": ["captcha-required"], "captcha_sitekey": "…",
//!   "captcha_service": "hcaptcha", "captcha_rqdata": "…",
//!   "captcha_rqtoken": "…", "captcha_session_id": "…" }
//! ```
//!
//! The official client shows the challenge and repeats the *same* request with
//! `X-Captcha-Key: <solution>` and `X-Captcha-Rqtoken: <captcha_rqtoken>`
//! (plus `X-Captcha-Session-Id` when given). Ferrocord never solves a captcha
//! itself and uses no solver service: a solution only ever comes from the user.
//!
//! Everything here comes from the network, so parsing is lenient (missing
//! fields are `None`), bounded (oversized values are dropped) and never puts a
//! value that could break an HTTP header into one.

use serde_json::Value;

/// `captcha_rqdata` is a few hundred bytes in practice; refuse anything huge.
const MAX_RQDATA: usize = 16 * 1024;
/// Site keys, tokens, ids and solutions are short; solutions can be a few KB.
const MAX_SHORT: usize = 512;
const MAX_SOLUTION: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptchaService {
    Hcaptcha,
    /// Another (or a missing) service name: not something a user can solve
    /// in Ferrocord's flows.
    Other(String),
}

/// A parsed captcha demand.
#[derive(Clone, PartialEq, Eq)]
pub struct CaptchaChallenge {
    /// The `captcha_key` codes (`captcha-required`, `invalid-response`, …).
    pub codes: Vec<String>,
    pub service: CaptchaService,
    pub sitekey: Option<String>,
    pub rqdata: Option<String>,
    pub rqtoken: Option<String>,
    pub session_id: Option<String>,
}

// rqdata / rqtoken are opaque but tie a challenge to a request: keep them out
// of logs.
impl std::fmt::Debug for CaptchaChallenge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptchaChallenge")
            .field("codes", &self.codes)
            .field(
                "service",
                &match &self.service {
                    CaptchaService::Hcaptcha => "hcaptcha",
                    CaptchaService::Other(_) => "other",
                },
            )
            .field("sitekey", &self.sitekey.is_some())
            .field("rqdata", &self.rqdata.is_some())
            .field("rqtoken", &self.rqtoken.is_some())
            .finish()
    }
}

impl CaptchaChallenge {
    /// Parses an error body. `None` unless it carries `captcha_key`.
    pub fn parse(body: &str) -> Option<Self> {
        let v: Value = serde_json::from_str(body).ok()?;
        Self::from_value(&v)
    }

    pub fn from_value(v: &Value) -> Option<Self> {
        let key = v.get("captcha_key")?;
        let codes = match key {
            Value::Array(a) => a
                .iter()
                .filter_map(Value::as_str)
                .filter(|s| s.len() <= 64)
                .take(8)
                .map(String::from)
                .collect(),
            Value::String(s) if s.len() <= 64 => vec![s.clone()],
            _ => Vec::new(),
        };
        let service = match field(v, "captcha_service", MAX_SHORT) {
            Some(s) if s.eq_ignore_ascii_case("hcaptcha") => CaptchaService::Hcaptcha,
            Some(s) => CaptchaService::Other(s),
            // Discord's captchas have been hCaptcha for years; older
            // answers omitted the field when a site key was present.
            None if v.get("captcha_sitekey").is_some() => CaptchaService::Hcaptcha,
            None => CaptchaService::Other(String::new()),
        };
        Some(Self {
            codes,
            service,
            sitekey: field(v, "captcha_sitekey", MAX_SHORT).filter(|s| header_safe(s)),
            rqdata: field(v, "captcha_rqdata", MAX_RQDATA),
            rqtoken: field(v, "captcha_rqtoken", MAX_SHORT).filter(|s| header_safe(s)),
            session_id: field(v, "captcha_session_id", MAX_SHORT).filter(|s| header_safe(s)),
        })
    }

    /// The previous solution was rejected (`invalid-response`).
    pub fn rejected_solution(&self) -> bool {
        self.codes.iter().any(|c| c == "invalid-response")
    }

    /// Whether a user could solve this in a browser engine (hCaptcha with
    /// a site key).
    pub fn is_solvable(&self) -> bool {
        self.service == CaptchaService::Hcaptcha && self.sitekey.is_some()
    }
}

fn field(v: &Value, name: &str, max: usize) -> Option<String> {
    v.get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.len() <= max)
        .map(String::from)
}

/// Visible ASCII only: safe as an HTTP header value.
pub fn header_safe(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| (0x21..=0x7e).contains(&b))
}

/// What the user's solved challenge gives back, ready for the retry.
#[derive(Clone, PartialEq, Eq)]
pub struct CaptchaSolution {
    key: String,
    rqtoken: Option<String>,
    session_id: Option<String>,
}

impl std::fmt::Debug for CaptchaSolution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CaptchaSolution(..)")
    }
}

impl CaptchaSolution {
    /// The solution `key` for `challenge`. `None` if the key is empty,
    /// oversized or not header-safe.
    pub fn new(key: &str, challenge: &CaptchaChallenge) -> Option<Self> {
        let key = key.trim();
        (header_safe(key) && key.len() <= MAX_SOLUTION).then(|| Self {
            key: key.to_owned(),
            rqtoken: challenge.rqtoken.clone(),
            session_id: challenge.session_id.clone(),
        })
    }

    /// The retry headers, in the official client's names.
    pub fn headers(&self) -> Vec<(&'static str, String)> {
        let mut h = vec![("X-Captcha-Key", self.key.clone())];
        if let Some(t) = &self.rqtoken {
            h.push(("X-Captcha-Rqtoken", t.clone()));
        }
        if let Some(s) = &self.session_id {
            h.push(("X-Captcha-Session-Id", s.clone()));
        }
        h
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"{"captcha_key":["captcha-required"],"captcha_sitekey":"a9b5fb07-92ff-493f-86fe-352a2803b3df","captcha_service":"hcaptcha","captcha_session_id":"sess-1","captcha_rqdata":"RQDATA/+==","captcha_rqtoken":"IlJRVE9LRU4i.abc"}"#;

    #[test]
    fn parses_all_fields() {
        let c = CaptchaChallenge::parse(FULL).unwrap();
        assert_eq!(c.codes, ["captcha-required"]);
        assert_eq!(c.service, CaptchaService::Hcaptcha);
        assert_eq!(
            c.sitekey.as_deref(),
            Some("a9b5fb07-92ff-493f-86fe-352a2803b3df")
        );
        assert_eq!(c.rqdata.as_deref(), Some("RQDATA/+=="));
        assert_eq!(c.rqtoken.as_deref(), Some("IlJRVE9LRU4i.abc"));
        assert_eq!(c.session_id.as_deref(), Some("sess-1"));
        assert!(c.is_solvable());
        assert!(!c.rejected_solution());
    }

    #[test]
    fn partial_bodies_still_parse() {
        // The old fixture shape: no rqdata / rqtoken.
        let c = CaptchaChallenge::parse(
            r#"{"captcha_key":["captcha-required"],"captcha_sitekey":"k","captcha_service":"hcaptcha"}"#,
        )
        .unwrap();
        assert!(c.rqdata.is_none() && c.rqtoken.is_none() && c.session_id.is_none());
        // No service but a site key: hCaptcha.
        let c =
            CaptchaChallenge::parse(r#"{"captcha_key":"captcha-required","captcha_sitekey":"k"}"#)
                .unwrap();
        assert_eq!(c.service, CaptchaService::Hcaptcha);
        assert_eq!(c.codes, ["captcha-required"]);
        // Only the key.
        let c = CaptchaChallenge::parse(r#"{"captcha_key":[]}"#).unwrap();
        assert!(!c.is_solvable());
        assert!(c.codes.is_empty());
        let c = CaptchaChallenge::parse(
            r#"{"captcha_key":["invalid-response"],"captcha_sitekey":"k"}"#,
        )
        .unwrap();
        assert!(c.rejected_solution());
    }

    #[test]
    fn non_captcha_and_garbage_are_none() {
        assert!(CaptchaChallenge::parse(r#"{"message":"Unknown","code":0}"#).is_none());
        assert!(CaptchaChallenge::parse("not json").is_none());
        assert!(CaptchaChallenge::parse("").is_none());
        assert!(CaptchaChallenge::parse("[1,2]").is_none());
        assert!(CaptchaChallenge::parse("null").is_none());
    }

    #[test]
    fn hostile_values_are_dropped() {
        let huge = "A".repeat(MAX_RQDATA + 1);
        let body = serde_json::json!({
            "captcha_key": [42, "x".repeat(65), "captcha-required"],
            "captcha_sitekey": "bad\r\nX-Injected: 1",
            "captcha_service": {"nested": true},
            "captcha_rqdata": huge,
            "captcha_rqtoken": "tok with spaces",
            "captcha_session_id": 7,
        })
        .to_string();
        let c = CaptchaChallenge::parse(&body).unwrap();
        assert_eq!(c.codes, ["captcha-required"]);
        assert!(c.sitekey.is_none(), "CRLF must never reach a header");
        assert!(c.rqdata.is_none());
        assert!(c.rqtoken.is_none());
        assert!(c.session_id.is_none());
        assert!(!c.is_solvable());
        // Other services are recognised as not solvable.
        let c = CaptchaChallenge::parse(
            r#"{"captcha_key":["captcha-required"],"captcha_service":"recaptcha","captcha_sitekey":"k"}"#,
        )
        .unwrap();
        assert_eq!(c.service, CaptchaService::Other("recaptcha".into()));
        assert!(!c.is_solvable());
    }

    #[test]
    fn debug_hides_request_data() {
        let c = CaptchaChallenge::parse(FULL).unwrap();
        let d = format!("{c:?}");
        assert!(!d.contains("RQDATA") && !d.contains("IlJRVE9LRU4i") && !d.contains("a9b5"));
        let s = CaptchaSolution::new("P1_secret", &c).unwrap();
        assert!(!format!("{s:?}").contains("P1_secret"));
    }

    #[test]
    fn solution_headers_follow_the_official_names() {
        let c = CaptchaChallenge::parse(FULL).unwrap();
        let s = CaptchaSolution::new("  P1_eyJ0eXAi.solution  ", &c).unwrap();
        assert_eq!(
            s.headers(),
            vec![
                ("X-Captcha-Key", "P1_eyJ0eXAi.solution".to_owned()),
                ("X-Captcha-Rqtoken", "IlJRVE9LRU4i.abc".to_owned()),
                ("X-Captcha-Session-Id", "sess-1".to_owned()),
            ]
        );
        let bare = CaptchaChallenge::parse(r#"{"captcha_key":["captcha-required"]}"#).unwrap();
        let s = CaptchaSolution::new("k", &bare).unwrap();
        assert_eq!(s.headers(), vec![("X-Captcha-Key", "k".to_owned())]);
        assert!(CaptchaSolution::new("", &c).is_none());
        assert!(CaptchaSolution::new("a\nb", &c).is_none());
        assert!(CaptchaSolution::new(&"k".repeat(MAX_SOLUTION + 1), &c).is_none());
    }
}
