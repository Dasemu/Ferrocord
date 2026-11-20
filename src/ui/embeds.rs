//! Embed cards. `app::embeds` decides the layout (sizes, grid, field rows,
//! video routing); this file only draws it. Image handles come from the
//! app's cache (created in `update`, never here).

use iced::widget::{Space, button, column, container, rich_text, row, span, stack, text};
use iced::{Alignment, Background, Border, Color, ContentFit, Font, Length, Padding};

use super::icons::{Icon, icon};
use super::theme::{self, INTER_MEDIUM, INTER_SEMIBOLD};
use super::{El, format_timestamp, markdown, ui};
use crate::app::embeds::{
    self, Group, MAX_AUTHOR, MAX_DESCRIPTION, MAX_FIELD_NAME, MAX_FIELD_VALUE, MAX_FOOTER,
    MAX_TITLE, MediaBox, Plan, VideoAction, clip, web_url,
};
use crate::app::{App, Message, Ui, now_ms};
use crate::net::model::{self as model, Embed};

/// All embed cards of a message, grouped (Twitter-style photo sets become one
/// card), with the "X" that suppresses them when the user may.
pub fn message_embeds<'a>(app: &'a App, m: &'a model::Message) -> Vec<El<'a>> {
    let mine = Some(m.author.id) == app.chat.me_id();
    let can = embeds::can_suppress(m, mine, app.chat.can_manage_messages(m.channel_id));
    embeds::group_embeds(embeds::visible_embeds(m))
        .into_iter()
        .map(|g| {
            let card = group_view(app, m, &g);
            if can {
                with_suppress(app, m, card)
            } else {
                card
            }
        })
        .collect()
}

/// Media that must be fetched for these embeds (used by the image loader).
pub fn media_keys(m: &model::Message) -> Vec<String> {
    let mut out = Vec::new();
    for g in embeds::group_embeds(embeds::visible_embeds(m)) {
        let e = g.embed;
        let p = embeds::plan(&g);
        out.extend(
            p.grid
                .iter()
                .chain(p.large.iter())
                .chain(p.thumb.iter())
                .filter_map(|b| b.key.clone()),
        );
        out.extend(e.author_icon.clone());
        out.extend(e.footer_icon.clone());
    }
    out
}

fn with_suppress<'a>(app: &'a App, m: &model::Message, card: El<'a>) -> El<'a> {
    let p = app.theme.palette;
    let x = button(
        container(icon(Icon::X, 14.0, p.text_secondary))
            .width(24)
            .height(24)
            .center_x(24)
            .center_y(24)
            .style(theme::rounded_bg(p.bg_app, 12.0)),
    )
    .padding(0)
    .on_press(ui(Ui::SuppressEmbeds(m.id)))
    .style(theme::link_button(p));
    // The button is always laid out (hover never changes layout).
    stack![
        card,
        container(x)
            .width(Length::Fill)
            .align_right(Length::Fill)
            .padding(Padding::new(6.0).right(10.0)),
    ]
    .width(Length::Shrink)
    .into()
}

fn placeholder<'a>(app: &App, w: u32, h: u32, glyph: Icon) -> El<'a> {
    let p = app.theme.palette;
    container(icon(glyph, 28.0, p.text_muted))
        .width(w as f32)
        .height(h as f32)
        .center_x(w as f32)
        .center_y(h as f32)
        .style(theme::rounded_bg(p.bg_app, 4.0))
        .into()
}

/// One image slot at its final size: the image, or a placeholder until it
/// has loaded; hidden behind a click when spoilered.
fn media<'a>(
    app: &'a App,
    m: &model::Message,
    b: &MediaBox,
    fit: ContentFit,
    radius: f32,
    spoiler: bool,
) -> El<'a> {
    let (w, h) = (b.w as f32, b.h as f32);
    if spoiler {
        let p = app.theme.palette;
        return button(
            container(
                text("SPOILER")
                    .size(13)
                    .font(theme::INTER_BOLD)
                    .color(p.accent_text),
            )
            .width(w)
            .height(h)
            .center_x(w)
            .center_y(h)
            .style(theme::rounded_bg(p.spoiler, radius)),
        )
        .padding(0)
        .on_press(ui(Ui::RevealSpoiler(m.id)))
        .style(theme::link_button(p))
        .into();
    }
    match b.key.as_deref().and_then(|k| app.image(k)) {
        // No `Cover`/crop/clip: the software renderer ignores them and the
        // image would overflow its box. `Contain` letterboxes instead.
        Some(handle) => container(
            iced::widget::image(handle.clone())
                .width(w)
                .height(h)
                .content_fit(fit)
                .border_radius(radius),
        )
        .width(w)
        .height(h)
        .center_x(w)
        .center_y(h)
        .style(theme::rounded_bg(app.theme.palette.bg_app, radius))
        .into(),
        None => placeholder(app, b.w, b.h, Icon::Image),
    }
}

fn round_icon<'a>(app: &'a App, key: &str, size: f32) -> El<'a> {
    let p = app.theme.palette;
    match app.image(key) {
        Some(h) => iced::widget::image(h.clone())
            .width(size)
            .height(size)
            .content_fit(ContentFit::Contain)
            .border_radius(size / 2.0)
            .into(),
        None => container(Space::new())
            .width(size)
            .height(size)
            .style(theme::rounded_bg(p.bg_app, size / 2.0))
            .into(),
    }
}

fn link_span<'a>(label: String, url: Option<&str>, size: f32, font: Font, color: Color) -> El<'a> {
    let mut sp = span::<Ui, Font>(label).font(font).size(size).color(color);
    if let Some(u) = url.and_then(web_url) {
        // The text is arbitrary: confirm the real target.
        sp = sp.link(Ui::ConfirmLink(u.to_owned()));
    }
    rich_text(vec![sp]).on_link_click(Message::Ui).into()
}

fn group_view<'a>(app: &'a App, m: &'a model::Message, g: &Group<'a>) -> El<'a> {
    let e = g.embed;
    let plan = embeds::plan(g);
    let spoiler = e
        .url
        .as_deref()
        .is_some_and(|u| embeds::url_is_spoilered(&m.content, u))
        && !app.revealed_spoilers.contains(&m.id);

    // Bare media: GIF links and direct image links, without a card. `gifv`
    // shows its still frame; `embeds::gifv_source` is where the animation
    // engine plugs in.
    if plan.grid.is_empty()
        && let Some(bm) = e.bare_media()
    {
        let b = bare_box(bm);
        return media(app, m, &b, ContentFit::Contain, 8.0, spoiler);
    }
    if plan.grid.len() >= 2 && is_bare_group(e) {
        return grid_view(app, m, &plan, spoiler);
    }
    card(app, m, g, &plan, spoiler)
}

fn is_bare_group(e: &Embed) -> bool {
    e.title.is_none() && e.description.is_none() && e.fields.is_empty() && e.author_name.is_none()
}

fn bare_box(bm: &model::EmbedMedia) -> MediaBox {
    let (w, h) = super::fit_size(bm.width.unwrap_or(400), bm.height.unwrap_or(300), 400, 300);
    MediaBox {
        key: Some(bm.download_url().to_owned()),
        w,
        h,
    }
}

fn grid_view<'a>(app: &'a App, m: &model::Message, plan: &Plan, spoiler: bool) -> El<'a> {
    let total_w = plan.grid.iter().map(|b| b.w).max().unwrap_or(400).max(400);
    let cols = embeds::grid_columns(plan.grid.len(), total_w);
    let mut it = plan.grid.iter();
    let mut r = row![].spacing(embeds::GRID_GAP as f32);
    for col in cols {
        let mut c = column![].spacing(embeds::GRID_GAP as f32);
        for (w, h) in col {
            let key = it.next().and_then(|b| b.key.clone());
            let cell = MediaBox { key, w, h };
            c = c.push(media(app, m, &cell, ContentFit::Contain, 4.0, spoiler));
        }
        r = r.push(c);
    }
    r.into()
}

fn video_view<'a>(
    app: &'a App,
    m: &model::Message,
    e: &Embed,
    b: &MediaBox,
    spoiler: bool,
) -> El<'a> {
    let p = app.theme.palette;
    let poster = media(app, m, b, ContentFit::Contain, 4.0, spoiler);
    let play = container(icon(Icon::Play, 26.0, Color::WHITE))
        .width(56)
        .height(56)
        .center_x(56)
        .center_y(56)
        .style(theme::rounded_bg(
            Color {
                a: 0.65,
                ..Color::BLACK
            },
            28.0,
        ));
    let overlay = container(play)
        .width(b.w as f32)
        .height(b.h as f32)
        .center_x(b.w as f32)
        .center_y(b.h as f32);
    let stacked: El<'a> = stack![poster, overlay].into();
    let msg = match embeds::video_action(e, &|u| app.is_allowed_media(u)) {
        VideoAction::Play(u) => Some(Ui::PlayVideo(u)),
        VideoAction::Browser(u) => Some(Ui::ConfirmLink(u)),
        VideoAction::None => None,
    };
    let mut btn = button(stacked).padding(0).style(theme::link_button(p));
    if let Some(msg) = msg {
        btn = btn.on_press(ui(msg));
    }
    btn.into()
}

fn card<'a>(
    app: &'a App,
    m: &'a model::Message,
    g: &Group<'a>,
    plan: &Plan,
    spoiler: bool,
) -> El<'a> {
    let t = &app.theme;
    let p = t.palette;
    let e = g.embed;
    let mut text_col = column![].spacing(8).width(Length::Fill);
    if let Some(pn) = &e.provider_name {
        text_col = text_col.push(
            text(clip(pn, MAX_AUTHOR).into_owned())
                .size(12)
                .color(p.text_secondary),
        );
    }
    if let Some(an) = &e.author_name {
        let name = link_span(
            clip(an, MAX_AUTHOR).into_owned(),
            e.author_url.as_deref(),
            14.0,
            INTER_SEMIBOLD,
            if e.author_url.as_deref().and_then(web_url).is_some() {
                p.link
            } else {
                p.header_text
            },
        );
        let mut r = row![].spacing(8).align_y(Alignment::Center);
        if let Some(k) = &e.author_icon {
            r = r.push(round_icon(app, k, 24.0));
        }
        text_col = text_col.push(r.push(name));
    }
    if let Some(title) = &e.title {
        let linked = e.url.as_deref().and_then(web_url).is_some();
        text_col = text_col.push(link_span(
            clip(title, MAX_TITLE).into_owned(),
            e.url.as_deref(),
            16.0,
            INTER_SEMIBOLD,
            if linked { p.link } else { p.header_text },
        ));
    }
    if let Some(d) = &e.description {
        text_col = text_col.push(markdown::render(
            app,
            m.id,
            &clip(d, MAX_DESCRIPTION),
            p.text_normal,
            14.0,
        ));
    }
    if !e.fields.is_empty() {
        let mut fields = column![].spacing(8);
        for fr in embeds::field_rows(&e.fields, plan.field_cols) {
            let mut r = row![].spacing(8);
            for &i in &fr.idx {
                let f = &e.fields[i];
                r = r.push(
                    column![
                        text(clip(&f.name, MAX_FIELD_NAME).into_owned())
                            .size(14)
                            .font(INTER_SEMIBOLD)
                            .color(p.header_text),
                        markdown::render(
                            app,
                            m.id,
                            &clip(&f.value, MAX_FIELD_VALUE),
                            p.text_normal,
                            14.0
                        ),
                    ]
                    .spacing(2)
                    .width(Length::FillPortion(1)),
                );
            }
            if fr.inline {
                // Inline cells keep their column width when the row is short.
                for _ in fr.idx.len()..plan.field_cols {
                    r = r.push(Space::new().width(Length::FillPortion(1)));
                }
            }
            fields = fields.push(r);
        }
        text_col = text_col.push(fields);
    }

    let top: El<'a> = match &plan.thumb {
        Some(th) => row![
            text_col,
            media(app, m, th, ContentFit::Contain, 4.0, spoiler)
        ]
        .spacing(16)
        .into(),
        None => text_col.into(),
    };
    let mut body = column![top].spacing(8);
    if let Some(l) = &plan.large {
        body = body.push(if plan.video {
            video_view(app, m, e, l, spoiler)
        } else {
            media(app, m, l, ContentFit::Contain, 4.0, spoiler)
        });
    }
    if plan.grid.len() >= 2 {
        body = body.push(grid_view(app, m, plan, spoiler));
    }
    if e.footer.is_some() || e.timestamp_ms.is_some() {
        let mut label = e
            .footer
            .as_deref()
            .map(|f| clip(f, MAX_FOOTER).into_owned())
            .unwrap_or_default();
        if let Some(ts) = e.timestamp_ms {
            if !label.is_empty() {
                label.push_str(" • ");
            }
            label.push_str(&format_timestamp(ts, now_ms()));
        }
        let mut r = row![].spacing(8).align_y(Alignment::Center);
        if let Some(k) = &e.footer_icon {
            r = r.push(round_icon(app, k, 20.0));
        }
        body = body.push(
            r.push(
                text(label)
                    .size(12)
                    .font(INTER_MEDIUM)
                    .color(p.text_secondary),
            ),
        );
    }

    let bar = e.color.map(theme::from_int).unwrap_or(p.bg_app);
    let content = container(
        container(body)
            .padding(Padding::from([8, 16]).top(8).bottom(16).left(12))
            .width(Length::Fill),
    )
    .max_width(t.metrics.embed_max_width)
    .style(move |_| container::Style {
        background: Some(Background::Color(p.embed_bg)),
        border: Border {
            radius: 4.0.into(),
            width: 0.0,
            color: Color::TRANSPARENT,
        },
        ..Default::default()
    });
    let bar_el = container(text(""))
        .width(4)
        .height(Length::Fill)
        .style(move |_| container::Style {
            background: Some(Background::Color(bar)),
            border: Border {
                radius: iced::border::Radius::default().left(4.0),
                ..Default::default()
            },
            ..Default::default()
        });
    container(row![bar_el, content].height(Length::Shrink))
        .max_width(t.metrics.embed_max_width + 4.0)
        .into()
}
