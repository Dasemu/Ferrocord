//! The `Message` enum: UI events, network events and task completions.

use iced::widget::scrollable::Viewport;

use std::path::PathBuf;

use super::emoji::{PickerTarget, Section};
use super::folders::Folders;
use super::panels::Popover;
use super::rail::RailItem;
use super::upload::UploadEvent;
use crate::net;
use crate::net::extras::{
    ChannelNotify, DiscoverCategory, DiscoverPage, InvitePreview, MuteDuration, NotifyLevel,
};
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
    // -- channel header popovers
    TogglePopover(Popover),
    ClosePopover,
    SetChannelLevel(ChannelId, NotifyLevel),
    MuteChannel(ChannelId, MuteDuration),
    UnmuteChannel(ChannelId),
    Unpin(ChannelId, MessageId),
    InboxMore,
    InboxGuildOnly(bool),
    OpenHelp,
    // -- add a server
    OpenAddServer,
    CloseAddServer,
    AddServerBack,
    AddServerCreate,
    AddServerJoin,
    CreateNameChanged(String),
    PickServerIcon,
    ClearServerIcon,
    SubmitCreate,
    JoinInputChanged(String),
    SubmitJoin,
    ConfirmJoin,
    // -- discover
    OpenDiscover,
    CloseDiscover,
    DiscoverInput(String),
    DiscoverSubmit,
    DiscoverCategory(Option<u32>),
    DiscoverMore,
    DiscoverJoin(GuildId),
    DiscoverDetails(Option<GuildId>),
    // -- server rail: folders and drag and drop
    RailPress(RailItem),
    RailRelease(RailItem),
    RailEnter(RailItem),
    RailExit,
    /// Any left-button release: a drag that did not end on a rail item.
    DragEnd,
    RailMenu(RailItem),
    CloseRailMenu,
    ToggleFolder(u64),
    CreateFolder(GuildId),
    MoveToFolder(GuildId, u64),
    RemoveFromFolder(GuildId),
    UngroupFolder(u64),
    OpenFolderSettings(u64),
    FolderNameChanged(String),
    FolderColor(Option<u32>),
    SaveFolder,
    CloseFolderSettings,
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
    /// Delay after a jump so the new rows are laid out before scrolling.
    ScrollToMessage(MessageId),
    StatusSaved(Result<(), String>),
    Pins {
        generation: u64,
        result: Result<Vec<crate::net::model::Message>, String>,
    },
    Mentions {
        generation: u64,
        result: Result<Vec<crate::net::model::Message>, String>,
    },
    /// Result of saving a channel's notification override; on failure the
    /// previous value is restored.
    NotifySaved {
        channel: ChannelId,
        previous: ChannelNotify,
        result: Result<(), String>,
    },
    Unpinned {
        channel: ChannelId,
        message: MessageId,
        result: Result<(), String>,
    },
    Invite {
        generation: u64,
        result: Result<InvitePreview, String>,
    },
    Joined {
        generation: u64,
        result: Result<(GuildId, String), String>,
    },
    Created(Result<(GuildId, String), String>),
    /// Server icon chosen: `(path, data URI)` or the reason it was refused;
    /// `None` when the dialog was cancelled.
    IconPicked(Option<Result<(PathBuf, String), String>>),
    DiscoverCategories(Result<Vec<DiscoverCategory>, String>),
    DiscoverPage {
        generation: u64,
        result: Result<DiscoverPage, String>,
    },
    DiscoverJoined {
        guild: GuildId,
        result: Result<(), String>,
    },
    FoldersSaved {
        previous: Folders,
        result: Result<(), String>,
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
