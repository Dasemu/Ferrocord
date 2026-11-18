//! The `Message` enum: UI events, network events and task completions.

use iced::widget::scrollable::Viewport;

use crate::net;
use crate::net::model::*;
use crate::net::remote_auth::RaEvent;

#[derive(Debug, Clone)]
pub enum Message {
    Ui(Ui),
    Net(net::Event),
    Done(Done),
}

/// User input.
#[derive(Debug, Clone)]
pub enum Ui {
    TokenInput(String),
    SubmitToken,
    RetryQr,
    SelectHome,
    SelectGuild(GuildId),
    SelectChannel(ChannelId),
    ToggleCategory(ChannelId),
    ComposerChanged(String),
    Send,
    Scrolled(Viewport),
    RevealSpoiler(MessageId),
    OpenUrl(String),
    Logout,
    WindowFocus(bool),
    Tick,
}

/// Completion of a `Task`.
#[derive(Debug, Clone)]
pub enum Done {
    FontsLoaded,
    /// Keyring read: `Ok(true)` if a token was found (it is moved into the
    /// app through `TokenReady`, never through a cloned message).
    TokenRestored(Result<Option<crate::auth::secrets::Secret>, String>),
    /// `GET /users/@me` result for a token being validated.
    TokenValidated {
        result: Result<User, String>,
        revoked: bool,
    },
    TokenSaved(Result<(), String>),
    Qr(RaEvent),
    CachedHistory {
        channel: ChannelId,
        messages: Vec<crate::net::model::Message>,
    },
    History {
        channel: ChannelId,
        older: bool,
        result: Result<Vec<crate::net::model::Message>, String>,
    },
    Sent {
        channel: ChannelId,
        nonce: String,
        result: Result<Box<crate::net::model::Message>, String>,
    },
    Image {
        key: String,
        bytes: Option<Vec<u8>>,
    },
    Screenshot(iced::window::Screenshot),
    ScreenshotReady,
    Ignored,
}

impl From<Ui> for Message {
    fn from(u: Ui) -> Self {
        Message::Ui(u)
    }
}
