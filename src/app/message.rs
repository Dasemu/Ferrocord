//! The `Message` enum: UI events, network events and task completions.

use iced::widget::scrollable::Viewport;

use std::path::PathBuf;

use super::emoji::{PickerTarget, Section};
use super::upload::UploadEvent;
use crate::net;
use crate::net::model::*;
use crate::net::remote_auth::RaEvent;
use crate::net::rest::UploadFile;

#[derive(Debug, Clone)]
pub enum Message {
    Ui(Ui),
    Net(net::Event),
    Done(Done),
    /// Events from the system tray service.
    Tray(crate::tray::Event),
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
    /// A link whose visible text differs from its target: ask first.
    ConfirmLink(String),
    CancelLink,
    Logout,
    WindowFocus(bool),
    WindowOpened,
    Tick,
    // -- replies, jumps
    StartReply(MessageId),
    CancelReply,
    ToggleReplyMention,
    JumpTo {
        channel: ChannelId,
        message: MessageId,
    },
    JumpToPresent,
    // -- edit and delete
    StartEdit(MessageId),
    EditChanged(String),
    SubmitEdit,
    CancelEdit,
    /// Up arrow in an empty composer.
    EditLast,
    DeleteClicked(MessageId),
    ConfirmDelete,
    CancelDelete,
    /// Escape: closes the topmost popup / cancels edit or reply.
    Escape,
    // -- message menus
    ContextMenu(MessageId),
    CloseMenu,
    CopyText(MessageId),
    CopyLink(MessageId),
    // -- reactions and emoji
    ToggleReaction(MessageId, Emoji),
    OpenEmojiPicker(PickerTarget),
    EmojiQuery(String),
    EmojiSection(Section),
    PickEmoji(Emoji),
    CloseEmojiPicker,
    /// Composer GIF button.
    OpenGifPicker,
    GifQuery(String),
    /// Sends the GIF with this Tenor URL to the open channel.
    PickGif(String),
    CloseGifPicker,
    // -- attachments
    PickFiles,
    FileDropped(PathBuf),
    Paste,
    RemoveAttachment(usize),
    CancelUpload(u64),
    // -- member list, status, search
    ToggleMembers,
    MembersScrolled(Viewport),
    ToggleStatusMenu,
    SetStatus(Presence),
    SearchChanged(String),
    SubmitSearch,
    SearchPage(bool),
    CloseSearch,
    /// Header pin button: opens (or closes) the pinned messages panel.
    TogglePins,
    /// Header inbox button: opens (or closes) the recent mentions panel.
    ToggleInbox,
    /// A notification was clicked.
    OpenChannel(ChannelId),
    // -- window and tray
    /// The window's close button (or Alt+F4).
    CloseRequested,
    ToggleWindow,
    ShowWindow,
    HideWindow,
    Quit,
    OpenSettings,
    CloseSettings,
    ToggleCloseToTray,
    ToggleStartMinimized,
    /// Tray "Mute notifications" / settings toggle.
    ToggleMuted,
    ToggleAutostart,
}

/// Completion of a `Task`.
#[derive(Debug, Clone)]
pub enum Done {
    /// Autostart entry written or removed: the resulting state.
    AutostartChanged(Result<bool, String>),
    /// Initial autostart state read from disk.
    AutostartLoaded(bool),
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
    Edited {
        channel: ChannelId,
        message: MessageId,
        result: Result<Box<crate::net::model::Message>, String>,
    },
    ActionFailed(String),
    ReactionFailed {
        channel: ChannelId,
        message: MessageId,
        emoji: Emoji,
        change: super::state::ReactionChange,
        error: String,
    },
    /// Files chosen in the dialog or dropped (already stat-ed).
    FilesReady(Vec<Result<(UploadFile, bool), String>>),
    /// Result of a Ctrl+V clipboard probe; `pasted` are file paths that may
    /// also have been pasted as text.
    Pasted {
        files: Vec<Result<(UploadFile, bool), String>>,
        pasted: Vec<PathBuf>,
    },
    ClearHighlight(MessageId),
    Upload {
        id: u64,
        event: UploadEvent,
    },
    /// A window around a message (jump), or `None` when loading failed.
    Around {
        channel: ChannelId,
        target: MessageId,
        result: Result<Vec<crate::net::model::Message>, String>,
    },
    /// Newer messages after a detached window.
    Newer {
        channel: ChannelId,
        result: Result<Vec<crate::net::model::Message>, String>,
    },
    Search {
        generation: u64,
        result: Result<SearchPage, String>,
    },
    /// Messages loaded for the pins / inbox panel.
    Panel {
        generation: u64,
        result: Result<Vec<crate::net::model::Message>, String>,
    },
    /// The GIF search debounce timer of this generation fired.
    GifSearchDue(u64),
    Gifs {
        generation: u64,
        result: Result<Vec<crate::net::model::Gif>, String>,
    },
    /// Delay after a jump so the new rows are laid out before scrolling.
    ScrollToMessage(MessageId),
    StatusSaved(Result<(), String>),
    Screenshot(iced::window::Screenshot),
    ScreenshotReady,
    Ignored,
}

impl From<Ui> for Message {
    fn from(u: Ui) -> Self {
        Message::Ui(u)
    }
}
