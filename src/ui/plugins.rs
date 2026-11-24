//! Plugins: the settings page (list, enable prompt, host-rendered plugin
//! settings) and the host-rendered message decorations. Plugins never draw:
//! everything here is built from their manifest and their decoration data.
//!
//! `page` is self-contained (title, back button, its own scrollable) so it
//! can sit in the settings dialog or in a settings sidebar.

use iced::widget::{
    Space, button, column, container, row, scrollable, text, text_input, toggler, tooltip,
};
use iced::{Alignment, Length, Padding};

use super::El;
use super::theme::{self, INTER_MEDIUM, INTER_SEMIBOLD};
use crate::app::plugins::PluginMsg;
use crate::app::{App, Message};
use crate::net::model::MessageId;
use crate::plugins::manifest::{Manifest, SettingKind};
use crate::plugins::package::Found;
use crate::plugins::{Decoration, DecorationKind, abi::SettingValue};

fn pm(m: PluginMsg) -> Message {
    Message::Plugin(m)
}

pub fn scroll_id() -> iced::widget::Id {
    iced::widget::Id::new("plugins-page")
}

/// The Plugins page.
pub fn page(app: &App) -> El<'_> {
    let t = &app.theme;
    let p = t.palette;
    let title = row![
        button(
            text("\u{2190} Back")
                .size(t.metrics.font_s)
                .color(p.text_secondary)
        )
        .padding(Padding::from([4, 8]))
        .on_press(pm(PluginMsg::ClosePage))
        .style(theme::secondary_button(p)),
        text("Plugins")
            .size(t.metrics.font_l)
            .font(INTER_SEMIBOLD)
            .color(p.header_text),
    ]
    .spacing(12)
    .align_y(Alignment::Center);

    if let Some(id) = &app.plugins.page.prompt
        && let Some(f) = app.plugins.scan.as_ref().and_then(|s| s.get(id))
    {
        return column![title, prompt(app, f)].spacing(16).into();
    }

    let note = text(
        "Plugins are third-party code that runs inside Ferrocord, sandboxed and limited to the \
         permissions you grant. Only install plugins you trust. Plugins that automate your \
         account (selfbots) break Discord's Terms of Service. There is no plugin store: \
         put plugin folders or .ferroplug files in the plugins folder.",
    )
    .size(t.metrics.font_s)
    .color(p.text_muted);

    let mut tools = row![
        button(text("Open plugins folder").size(t.metrics.font_s))
            .padding(Padding::from([6, 12]))
            .on_press_maybe(
                (app.plugins.dir.is_some() && !app.cfg.mock).then(|| pm(PluginMsg::OpenFolder))
            )
            .style(theme::secondary_button(p)),
        button(text("Reload plugins").size(t.metrics.font_s))
            .padding(Padding::from([6, 12]))
            .on_press_maybe(app.plugins.dir.is_some().then(|| pm(PluginMsg::Reload)))
            .style(theme::secondary_button(p)),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    if app.plugins.page.scanning {
        tools = tools.push(text("Scanning…").size(t.metrics.font_s).color(p.text_muted));
    }

    let mut list = column![].spacing(10);
    match (&app.plugins.dir, &app.plugins.scan) {
        (None, _) => {
            list = list.push(
                text("Plugins are unavailable (no configuration folder).")
                    .size(t.metrics.font_m)
                    .color(p.text_muted),
            );
        }
        (Some(dir), scan) => {
            let found = scan.as_ref().map_or(&[][..], |s| s.found.as_slice());
            if found.is_empty() && !app.plugins.page.scanning {
                list = list.push(
                    text(format!(
                        "No plugins installed. Plugins go in {}",
                        dir.display()
                    ))
                    .size(t.metrics.font_m)
                    .color(p.text_muted),
                );
            }
            for f in found {
                list = list.push(card(app, f));
            }
            for b in scan.iter().flat_map(|s| s.broken.iter()) {
                let name = b
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                list = list.push(
                    container(
                        column![
                            text(name).size(t.metrics.font_m).color(p.text_normal),
                            text(b.error.clone()).size(t.metrics.font_s).color(p.danger),
                        ]
                        .spacing(2),
                    )
                    .padding(12)
                    .width(Length::Fill)
                    .style(theme::plugin_card(p)),
                );
            }
        }
    }
    let dropped = app.plugins.dropped_events();
    if dropped > 0 {
        list = list.push(
            text(format!(
                "{dropped} events were skipped because plugins were too slow."
            ))
            .size(t.metrics.font_s)
            .color(p.text_muted),
        );
    }
    column![
        title,
        note,
        tools,
        container(
            scrollable(list.padding(Padding::ZERO.right(10)))
                .id(scroll_id())
                .style(theme::thin_scrollbar(p, false))
        )
        .max_height(440),
    ]
    .spacing(14)
    .into()
}

fn header<'a>(app: &'a App, m: &'a Manifest) -> El<'a> {
    let t = &app.theme;
    let p = t.palette;
    let mut by = format!("v{}", m.version);
    if !m.author.is_empty() {
        by.push_str(" · ");
        by.push_str(&m.author);
    }
    column![
        row![
            text(m.name.as_str())
                .size(t.metrics.font_m)
                .font(INTER_SEMIBOLD)
                .color(p.header_text),
            text(by).size(t.metrics.font_s).color(p.text_muted),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
        text(m.description.as_str())
            .size(t.metrics.font_s)
            .color(p.text_secondary),
    ]
    .spacing(2)
    .width(Length::Fill)
    .into()
}

fn card<'a>(app: &'a App, f: &'a Found) -> El<'a> {
    let t = &app.theme;
    let p = t.palette;
    let m = &f.manifest;
    let enabled = app.plugins.state.is_enabled(&m.id);
    let record = app.plugins.state.plugins.get(&m.id);
    let mut col = column![
        row![
            header(app, m),
            toggler(enabled)
                .on_toggle(|_| pm(PluginMsg::Toggle(m.id.clone())))
                .size(22.0)
                .style(theme::switch(p)),
        ]
        .spacing(12)
        .align_y(Alignment::Center),
    ]
    .spacing(8);
    if let Some(reason) = record.and_then(|r| r.disabled_reason.as_deref()) {
        col = col.push(text(reason).size(t.metrics.font_s).color(p.danger));
    }
    if !m.permissions.is_empty() {
        let names: Vec<&str> = m
            .permissions
            .iter()
            .map(|x| match x {
                crate::plugins::manifest::Permission::ReadMessages => "read messages",
                crate::plugins::manifest::Permission::ReadPresence => "presence",
                crate::plugins::manifest::Permission::TransformOutgoing => "edit outgoing text",
                crate::plugins::manifest::Permission::SendMessages => "send messages",
                crate::plugins::manifest::Permission::Reactions => "reactions",
                crate::plugins::manifest::Permission::Clipboard => "clipboard",
                crate::plugins::manifest::Permission::Composer => "message box",
                crate::plugins::manifest::Permission::Storage => "storage",
                crate::plugins::manifest::Permission::Network => "network",
                crate::plugins::manifest::Permission::Decorations => "message badges",
            })
            .collect();
        col = col.push(
            text(format!("Permissions: {}", names.join(", ")))
                .size(t.metrics.font_xs)
                .color(p.text_muted),
        );
    }
    if !m.network_hosts.is_empty() {
        col = col.push(
            text(format!("Connects to: {}", m.network_hosts.join(", ")))
                .size(t.metrics.font_xs)
                .color(p.text_muted),
        );
    }
    if !m.commands.is_empty() {
        let cmds: Vec<String> = m.commands.iter().map(|c| format!("/{}", c.name)).collect();
        col = col.push(
            text(format!("Commands: {}", cmds.join(" ")))
                .size(t.metrics.font_xs)
                .color(p.text_muted),
        );
    }
    if !m.settings.is_empty() {
        let values = m.effective_settings(&record.map(|r| r.settings.clone()).unwrap_or_default());
        for s in &m.settings {
            let v = values.get(&s.key).cloned().unwrap_or(s.default.clone());
            col = col.push(setting_row(app, m, s, v));
        }
    }
    container(col)
        .padding(12)
        .width(Length::Fill)
        .style(theme::plugin_card(p))
        .into()
}

fn setting_row<'a>(
    app: &'a App,
    m: &'a Manifest,
    s: &'a crate::plugins::manifest::SettingSpec,
    v: SettingValue,
) -> El<'a> {
    let t = &app.theme;
    let p = t.palette;
    let label = column![
        text(s.label.as_str())
            .size(t.metrics.font_s)
            .color(p.text_normal),
        text(s.description.as_str())
            .size(t.metrics.font_xs)
            .color(p.text_muted),
    ]
    .spacing(1)
    .width(Length::Fill);
    let (id, key) = (m.id.clone(), s.key.clone());
    let control: El<'a> = match (&s.kind, v) {
        (SettingKind::Bool, v) => toggler(v.as_bool().unwrap_or(false))
            .on_toggle(move |b| pm(PluginMsg::SetBool(id.clone(), key.clone(), b)))
            .size(18.0)
            .style(theme::switch(p))
            .into(),
        (SettingKind::Choice { choices }, v) => {
            let cur = v.as_str().unwrap_or_default().to_owned();
            let mut r = row![].spacing(4);
            for c in choices {
                let selected = *c == cur;
                r = r.push(
                    button(text(c.as_str()).size(t.metrics.font_xs))
                        .padding(Padding::from([3, 8]))
                        .on_press(pm(PluginMsg::SetChoice(id.clone(), key.clone(), c.clone())))
                        .style(move |th, st| {
                            if selected {
                                theme::primary_button(p)(th, st)
                            } else {
                                theme::secondary_button(p)(th, st)
                            }
                        }),
                );
            }
            r.into()
        }
        (SettingKind::Int { .. } | SettingKind::Str { .. }, v) => {
            let shown = app
                .plugins
                .page
                .drafts
                .get(&(id.clone(), key.clone()))
                .cloned()
                .unwrap_or_else(|| match v {
                    SettingValue::Int(i) => i.to_string(),
                    SettingValue::Str(s) => s,
                    SettingValue::Bool(b) => b.to_string(),
                });
            text_input("", &shown)
                .on_input(move |s| pm(PluginMsg::EditText(id.clone(), key.clone(), s)))
                .size(t.metrics.font_s)
                .padding(6)
                .width(200)
                .style(theme::input(p, p.bg_app))
                .into()
        }
    };
    row![label, control]
        .spacing(12)
        .align_y(Alignment::Center)
        .into()
}

/// "Enable <plugin>?" with everything it may do.
fn prompt<'a>(app: &'a App, f: &'a Found) -> El<'a> {
    let t = &app.theme;
    let p = t.palette;
    let m = &f.manifest;
    let mut perms = column![].spacing(6);
    if m.permissions.is_empty() {
        perms = perms.push(
            text("It asks for no permissions.")
                .size(t.metrics.font_s)
                .color(p.text_secondary),
        );
    }
    for x in &m.permissions {
        perms = perms.push(
            text(format!("•  {}", x.describe()))
                .size(t.metrics.font_s)
                .color(if x.sensitive() {
                    p.header_text
                } else {
                    p.text_secondary
                }),
        );
    }
    for h in &m.network_hosts {
        perms = perms.push(
            text(format!("      {h}"))
                .size(t.metrics.font_s)
                .font(theme::MONO)
                .color(p.header_text),
        );
    }
    column![
        header(app, m),
        text(format!("Enable \u{201c}{}\u{201d}? It will be able to:", m.name))
            .size(t.metrics.font_m)
            .font(INTER_MEDIUM)
            .color(p.text_normal),
        perms,
        text("Plugins are third-party code. Ferrocord limits them to these permissions, but only enable plugins you trust.")
            .size(t.metrics.font_xs)
            .color(p.text_muted),
        row![
            Space::new().width(Length::Fill),
            button(text("Cancel").size(14))
                .padding(Padding::from([8, 16]))
                .on_press(pm(PluginMsg::CancelPrompt))
                .style(theme::secondary_button(p)),
            button(text("Enable").size(14).color(p.accent_text))
                .padding(Padding::from([8, 16]))
                .on_press(pm(PluginMsg::ConfirmEnable))
                .style(theme::primary_button(p)),
        ]
        .spacing(8),
    ]
    .spacing(12)
    .into()
}

/// Plugin decorations under a message (`None`: there are none).
pub fn decorations<'a>(app: &'a App, m: MessageId) -> Option<El<'a>> {
    let items = app.plugins.decorations.get(m.0);
    if items.is_empty() {
        return None;
    }
    let t = &app.theme;
    let p = t.palette;
    let mut badges = row![].spacing(4);
    let mut lines = column![].spacing(2);
    let (mut any_badge, mut any_line) = (false, false);
    for (_, d) in items {
        match d.kind {
            DecorationKind::Line if !d.text.is_empty() => {
                any_line = true;
                lines = lines.push(
                    text(d.text.as_str())
                        .size(t.metrics.font_s)
                        .color(p.text_muted),
                );
            }
            DecorationKind::Badge | DecorationKind::Highlight if !d.text.is_empty() => {
                any_badge = true;
                badges = badges.push(badge(app, d));
            }
            _ => {}
        }
    }
    if !any_badge && !any_line {
        return None;
    }
    let mut col = column![].spacing(4);
    if any_line {
        col = col.push(lines);
    }
    if any_badge {
        col = col.push(badges);
    }
    Some(col.into())
}

fn badge<'a>(app: &'a App, d: &'a Decoration) -> El<'a> {
    let t = &app.theme;
    let p = t.palette;
    let pill = container(text(d.text.as_str()).size(t.metrics.font_xs))
        .padding(Padding::from([1, 6]))
        .style(theme::plugin_badge(p, d.kind == DecorationKind::Highlight));
    match &d.tooltip {
        Some(tip) => tooltip(
            pill,
            container(
                text(tip.as_str())
                    .size(t.metrics.font_s)
                    .color(p.text_normal),
            )
            .padding(6)
            .style(theme::popup(p)),
            tooltip::Position::Top,
        )
        .into(),
        None => pill.into(),
    }
}

/// Context-menu entries the running plugins add to a message.
pub fn menu_entries(app: &App, m: MessageId) -> Vec<(String, Message)> {
    app.plugins
        .actions()
        .map(|(plugin, action, label)| {
            (
                label.to_owned(),
                pm(PluginMsg::Action {
                    plugin: plugin.to_owned(),
                    action: action.to_owned(),
                    message: m,
                }),
            )
        })
        .collect()
}
