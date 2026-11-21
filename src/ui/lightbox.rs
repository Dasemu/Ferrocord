//! The full-window image viewer (`app::lightbox`): the picture as large as
//! fits, centred over a dark margin that closes it when clicked.

use iced::widget::{Space, button, column, container, mouse_area, opaque, row, stack};
use iced::{ContentFit, Length, mouse};

use super::anim_image::message_anim;
use super::icons::{Icon, icon};
use super::{El, theme, ui};
use crate::app::lightbox::{Lightbox, MARGIN, fill_size};
use crate::app::{App, Ui, input};

/// Makes a chat image clickable: a click opens it full-window.
pub fn zoomable<'a>(content: impl Into<El<'a>>, lb: Lightbox) -> El<'a> {
    mouse_area(content)
        .on_press(ui(Ui::OpenLightbox(lb)))
        .interaction(mouse::Interaction::Pointer)
        .into()
}

/// The viewer layer, `None` while closed.
pub fn view(app: &App) -> Option<El<'_>> {
    let lb = app.lightbox.as_ref()?;
    let p = app.theme.palette;
    let win = input::window_size();
    let (w, h) = fill_size(
        lb.width,
        lb.height,
        win.width - 2.0 * MARGIN,
        win.height - 2.0 * MARGIN,
    );
    let picture: El<'_> = if let Some(a) = app.media_anim(&lb.key) {
        message_anim(app, a, w, h)
            .content_fit(ContentFit::Contain)
            .into()
    } else if let Some(handle) = lb
        .full
        .as_deref()
        .and_then(|f| app.image(f))
        .or_else(|| app.image(&lb.key))
    {
        iced::widget::image(handle.clone())
            .width(w)
            .height(h)
            .content_fit(ContentFit::Contain)
            .into()
    } else {
        container(icon(Icon::Image, 48.0, theme::video_fg_dim()))
            .center_x(w)
            .center_y(h)
            .into()
    };
    // The picture swallows its clicks; only the margin closes.
    let centred = container(opaque(container(picture).width(w).height(h))).center(Length::Fill);
    let mut buttons = row![Space::new().width(Length::Fill)].spacing(4);
    if let Some(url) = &lb.url {
        buttons = buttons.push(
            button(icon(Icon::ExternalLink, 22.0, theme::video_fg()))
                .padding(8)
                .style(theme::video_control(p))
                .on_press(ui(Ui::OpenUrl(url.clone()))),
        );
    }
    buttons = buttons.push(
        button(icon(Icon::X, 22.0, theme::video_fg()))
            .padding(8)
            .style(theme::video_control(p))
            .on_press(ui(Ui::CloseLightbox)),
    );
    let body = stack![
        centred,
        column![buttons.padding(8), Space::new().height(Length::Fill)],
    ];
    Some(opaque(
        mouse_area(
            container(body)
                .width(Length::Fill)
                .height(Length::Fill)
                .style(theme::bg(theme::lightbox_scrim())),
        )
        .on_press(ui(Ui::CloseLightbox)),
    ))
}
