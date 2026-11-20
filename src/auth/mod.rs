//! Login flows (token, QR) and the authentication state machine.

pub mod secrets;

use crate::net::model::User;

/// Authentication state machine.
#[derive(Debug, Clone, PartialEq)]
pub enum AuthState {
    /// Reading the keyring at startup.
    Restoring,
    /// Login screen.
    LoggedOut {
        error: Option<String>,
    },
    /// A token was submitted (typed, restored or from QR) and is being
    /// validated against `GET /users/@me`.
    Validating,
    LoggedIn {
        user: User,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum AuthEvent {
    /// Keyring read finished; `true` if a token was found.
    Restored {
        found: bool,
    },
    /// The keyring itself is unavailable (no Secret Service).
    KeyringUnavailable(String),
    Submitted,
    Validated(User),
    /// Token rejected or validation failed.
    Rejected(String),
    /// Session revoked by the server (401 / gateway 4004).
    Revoked,
    LoggedOut,
}

impl AuthState {
    pub fn apply(&self, ev: AuthEvent) -> AuthState {
        use AuthEvent as E;
        use AuthState as S;
        match (self, ev) {
            (S::Restoring, E::Restored { found: true }) => S::Validating,
            (S::Restoring, E::Restored { found: false }) => S::LoggedOut { error: None },
            (_, E::KeyringUnavailable(reason)) => S::LoggedOut {
                error: Some(format!(
                    "Secure storage is unavailable ({reason}). Ferrocord never stores tokens in plain text, so login is disabled until a Secret Service (KWallet, GNOME Keyring) is running."
                )),
            },
            (S::LoggedOut { .. } | S::Restoring, E::Submitted) => S::Validating,
            (S::Validating, E::Validated(user)) => S::LoggedIn { user },
            // Restored sessions validate in the background: READY can also
            // confirm the user while already logged in.
            (S::LoggedIn { .. }, E::Validated(user)) => S::LoggedIn { user },
            (S::Validating, E::Rejected(reason)) => S::LoggedOut {
                error: Some(reason),
            },
            (_, E::Revoked) => S::LoggedOut {
                error: Some("Your session has expired or was revoked. Please log in again.".into()),
            },
            (_, E::LoggedOut) => S::LoggedOut { error: None },
            (s, _) => s.clone(),
        }
    }

    pub fn is_logged_in(&self) -> bool {
        matches!(self, AuthState::LoggedIn { .. })
    }
}

/// Cleans up a pasted token. Returns a user-facing error for obviously
/// invalid input (without echoing the input).
pub fn normalize_token(input: &str) -> Result<String, &'static str> {
    let t = input.trim().trim_matches('"').trim_matches('\'').trim();
    if t.is_empty() {
        return Err("Please enter a token.");
    }
    if t.len() > 512 {
        return Err("That does not look like a Discord token.");
    }
    if t.starts_with("Bot ") || t.starts_with("Bearer ") {
        return Err("Bot and OAuth tokens are not supported; use your account token.");
    }
    if t.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("Tokens cannot contain spaces.");
    }
    if !t.contains('.') {
        return Err("That does not look like a Discord token.");
    }
    Ok(t.to_owned())
}

/// QR login sub-state shown on the login screen.
#[derive(Debug, Clone, PartialEq)]
pub enum QrState {
    Connecting,
    /// Render this URL as QR (matrix size, modules).
    Ready {
        url: String,
        size: usize,
        modules: Vec<bool>,
    },
    Scanned {
        username: String,
    },
    Failed(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::model::Snowflake;

    fn user() -> User {
        User {
            id: Snowflake(1),
            username: "u".into(),
            global_name: None,
            avatar: None,
            bot: false,
            ..Default::default()
        }
    }

    #[test]
    fn restore_with_token_validates_then_logs_in() {
        let s = AuthState::Restoring.apply(AuthEvent::Restored { found: true });
        assert_eq!(s, AuthState::Validating);
        let s = s.apply(AuthEvent::Validated(user()));
        assert!(s.is_logged_in());
    }

    #[test]
    fn restore_without_token_shows_login() {
        assert_eq!(
            AuthState::Restoring.apply(AuthEvent::Restored { found: false }),
            AuthState::LoggedOut { error: None }
        );
    }

    #[test]
    fn rejection_shows_error() {
        let s = AuthState::LoggedOut { error: None }
            .apply(AuthEvent::Submitted)
            .apply(AuthEvent::Rejected("bad".into()));
        assert_eq!(
            s,
            AuthState::LoggedOut {
                error: Some("bad".into())
            }
        );
    }

    #[test]
    fn revoked_from_anywhere_logs_out_with_message() {
        let s = AuthState::LoggedIn { user: user() }.apply(AuthEvent::Revoked);
        assert!(matches!(s, AuthState::LoggedOut { error: Some(_) }));
    }

    #[test]
    fn keyring_unavailable_never_falls_back() {
        let s = AuthState::Restoring.apply(AuthEvent::KeyringUnavailable("no dbus".into()));
        let AuthState::LoggedOut { error: Some(e) } = s else {
            panic!()
        };
        assert!(e.contains("never stores tokens in plain text"));
    }

    #[test]
    fn ignores_out_of_order_events() {
        let s = AuthState::LoggedOut { error: None }.apply(AuthEvent::Validated(user()));
        assert_eq!(s, AuthState::LoggedOut { error: None });
    }

    #[test]
    fn token_normalisation() {
        assert_eq!(
            normalize_token("  \"abc.def.ghi\" ").unwrap(),
            "abc.def.ghi"
        );
        assert!(normalize_token("").is_err());
        assert!(normalize_token("Bot abc.def").is_err());
        assert!(normalize_token("has space.x").is_err());
        assert!(normalize_token("nodots").is_err());
    }
}
