//! Server rail interaction: clicks, drag and drop onto a server to make a
//! folder, the right-click menu and the folder settings dialog. The pure
//! folder logic lives in `folders.rs`; this module wires it to messages,
//! persistence and the cache.

use std::collections::HashSet;

use iced::{Point, Task};

use super::folders::{Folders, new_folder_id};
use super::{App, Done, Message, Ui, View, input};
use crate::net::model::*;

/// Something in the rail that can be pressed, dropped on or right-clicked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RailItem {
    Guild(GuildId),
    Folder(u64),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RailMenu {
    pub item: RailItem,
    pub at: Point,
}

/// The folder settings dialog ("Folder Name" and colour).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderDialog {
    pub folder: u64,
    pub name: String,
    pub color: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct RailUi {
    /// Folders shown open (local, like the official client).
    pub expanded: HashSet<u64>,
    /// Item the left button went down on.
    pub press: Option<RailItem>,
    pub hover: Option<RailItem>,
    pub menu: Option<RailMenu>,
    pub folder_dialog: Option<FolderDialog>,
}

impl RailUi {
    /// The item a dragged server would land on, for the drop highlight.
    pub fn drop_target(&self) -> Option<RailItem> {
        match (self.press, self.hover) {
            (Some(RailItem::Guild(_)), Some(h)) if self.press != Some(h) => Some(h),
            _ => None,
        }
    }
}

/// What releasing the button over `target` means after pressing `from`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gesture {
    Click(RailItem),
    DropOnGuild { dragged: GuildId, target: GuildId },
    DropOnFolder { dragged: GuildId, folder: u64 },
    Nothing,
}

pub fn gesture(from: Option<RailItem>, target: RailItem) -> Gesture {
    match (from, target) {
        (Some(f), t) if f == t => Gesture::Click(t),
        (Some(RailItem::Guild(d)), RailItem::Guild(t)) => Gesture::DropOnGuild {
            dragged: d,
            target: t,
        },
        (Some(RailItem::Guild(d)), RailItem::Folder(f)) => Gesture::DropOnFolder {
            dragged: d,
            folder: f,
        },
        // Dragging a folder (reordering) is not supported.
        _ => Gesture::Nothing,
    }
}

impl App {
    pub(super) fn update_rail(&mut self, u: Ui) -> Option<Task<Message>> {
        Some(match u {
            Ui::RailPress(i) => {
                self.rail.press = Some(i);
                Task::none()
            }
            Ui::RailEnter(i) => {
                self.rail.hover = Some(i);
                Task::none()
            }
            Ui::RailExit => {
                self.rail.hover = None;
                Task::none()
            }
            Ui::DragEnd => {
                self.rail.press = None;
                Task::none()
            }
            Ui::RailRelease(target) => {
                let from = self.rail.press.take();
                match gesture(from, target) {
                    Gesture::Click(RailItem::Guild(g)) => self.update_ui(Ui::SelectGuild(g)),
                    Gesture::Click(RailItem::Folder(f)) => self.update_ui(Ui::ToggleFolder(f)),
                    Gesture::DropOnGuild { dragged, target } => {
                        let prev = self.chat.folders.clone();
                        match self.chat.folders.drop_on(dragged, target, new_folder_id()) {
                            Some(id) => {
                                self.rail.expanded.insert(id);
                                self.folders_changed(prev)
                            }
                            None => Task::none(),
                        }
                    }
                    Gesture::DropOnFolder { dragged, folder } => {
                        let prev = self.chat.folders.clone();
                        if self.chat.folders.drop_on_folder(dragged, folder) {
                            self.folders_changed(prev)
                        } else {
                            Task::none()
                        }
                    }
                    Gesture::Nothing => Task::none(),
                }
            }
            Ui::RailMenu(item) => {
                self.rail.menu = Some(RailMenu {
                    item,
                    at: input::cursor(),
                });
                self.menu = None;
                self.emoji_picker = None;
                self.panels.open = None;
                Task::none()
            }
            Ui::CloseRailMenu => {
                self.rail.menu = None;
                Task::none()
            }
            Ui::ToggleFolder(f) => {
                if !self.rail.expanded.remove(&f) {
                    self.rail.expanded.insert(f);
                }
                Task::none()
            }
            Ui::CreateFolder(g) => {
                self.rail.menu = None;
                let prev = self.chat.folders.clone();
                match self.chat.folders.create_with(g, new_folder_id()) {
                    Some(id) => {
                        self.rail.expanded.insert(id);
                        self.folders_changed(prev)
                    }
                    None => Task::none(),
                }
            }
            Ui::MoveToFolder(g, f) => {
                self.rail.menu = None;
                let prev = self.chat.folders.clone();
                if self.chat.folders.drop_on_folder(g, f) {
                    self.folders_changed(prev)
                } else {
                    Task::none()
                }
            }
            Ui::RemoveFromFolder(g) => {
                self.rail.menu = None;
                let prev = self.chat.folders.clone();
                if self.chat.folders.remove_from_folder(g) {
                    self.folders_changed(prev)
                } else {
                    Task::none()
                }
            }
            Ui::UngroupFolder(f) => {
                self.rail.menu = None;
                self.rail.expanded.remove(&f);
                let prev = self.chat.folders.clone();
                if self.chat.folders.ungroup(f) {
                    self.folders_changed(prev)
                } else {
                    Task::none()
                }
            }
            Ui::OpenFolderSettings(f) => {
                self.rail.menu = None;
                if let Some(folder) = self.chat.folders.folder(f) {
                    self.rail.folder_dialog = Some(FolderDialog {
                        folder: f,
                        name: folder.name.clone().unwrap_or_default(),
                        color: folder.color,
                    });
                    return Some(iced::widget::operation::focus(
                        crate::ui::servers::folder_name_input_id(),
                    ));
                }
                Task::none()
            }
            Ui::FolderNameChanged(s) => {
                if let Some(d) = self.rail.folder_dialog.as_mut() {
                    d.name = s.chars().take(32).collect();
                }
                Task::none()
            }
            Ui::FolderColor(c) => {
                if let Some(d) = self.rail.folder_dialog.as_mut() {
                    d.color = c;
                }
                Task::none()
            }
            Ui::CloseFolderSettings => {
                self.rail.folder_dialog = None;
                Task::none()
            }
            Ui::SaveFolder => {
                let Some(d) = self.rail.folder_dialog.take() else {
                    return Some(Task::none());
                };
                let prev = self.chat.folders.clone();
                self.chat.folders.rename(d.folder, &d.name);
                self.chat.folders.set_color(d.folder, d.color);
                if self.chat.folders == prev {
                    Task::none()
                } else {
                    self.folders_changed(prev)
                }
            }
            _ => return None,
        })
    }

    /// After a local folder edit: re-sort the rail, cache the list and save
    /// it with the same settings update the official client uses.
    fn folders_changed(&mut self, previous: Folders) -> Task<Message> {
        self.chat.resort_guilds();
        self.cache_folders();
        if self.cfg.mock {
            return Task::none();
        }
        let rest = self.rest.clone();
        let body = self.chat.folders.to_wire();
        Task::perform(
            async move { rest.set_guild_folders(body).await },
            move |r| {
                Message::Done(Done::FoldersSaved {
                    previous,
                    result: r.map_err(|e| e.user_message()),
                })
            },
        )
    }

    pub(super) fn on_folders_saved(
        &mut self,
        previous: Folders,
        result: Result<(), String>,
    ) -> Task<Message> {
        if let Err(e) = result {
            self.chat.folders = previous;
            self.chat.resort_guilds();
            self.cache_folders();
            self.toast = Some(format!("Could not save your server folders: {e}"));
        }
        Task::none()
    }

    /// Writes the folder list into the cache so the next start shows the
    /// folders before READY arrives.
    pub(super) fn cache_folders(&self) {
        if self.cfg.mock {
            return;
        }
        let json = self.chat.folders.to_wire()["guild_folders"].to_string();
        if let Err(e) = self.store.set_meta("guild_folders", &json) {
            tracing::warn!(error = %e, "could not cache server folders");
        }
    }

    /// Whether a guild's server view is the open one (for the pill).
    pub fn guild_open(&self, g: GuildId) -> bool {
        matches!(self.view, View::Guild { guild, .. } if guild == g)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(n: u64) -> RailItem {
        RailItem::Guild(Snowflake(n))
    }

    #[test]
    fn release_on_the_pressed_item_is_a_click() {
        assert_eq!(gesture(Some(g(1)), g(1)), Gesture::Click(g(1)));
        assert_eq!(
            gesture(Some(RailItem::Folder(4)), RailItem::Folder(4)),
            Gesture::Click(RailItem::Folder(4))
        );
    }

    #[test]
    fn release_elsewhere_after_a_press_drops() {
        assert_eq!(
            gesture(Some(g(1)), g(2)),
            Gesture::DropOnGuild {
                dragged: Snowflake(1),
                target: Snowflake(2)
            }
        );
        assert_eq!(
            gesture(Some(g(1)), RailItem::Folder(9)),
            Gesture::DropOnFolder {
                dragged: Snowflake(1),
                folder: 9
            }
        );
    }

    #[test]
    fn no_press_or_a_dragged_folder_does_nothing() {
        // A press that began outside the rail never creates a folder.
        assert_eq!(gesture(None, g(2)), Gesture::Nothing);
        assert_eq!(gesture(Some(RailItem::Folder(1)), g(2)), Gesture::Nothing);
    }

    #[test]
    fn drop_target_only_while_dragging_a_server_over_another_item() {
        let mut r = RailUi::default();
        assert_eq!(r.drop_target(), None);
        r.press = Some(g(1));
        r.hover = Some(g(1));
        assert_eq!(r.drop_target(), None, "over itself");
        r.hover = Some(g(2));
        assert_eq!(r.drop_target(), Some(g(2)));
        r.press = None;
        assert_eq!(r.drop_target(), None);
    }
}
