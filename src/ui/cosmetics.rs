//! Drawing helpers for profile cosmetics: avatar decorations (a frame that
//! overflows the avatar slot without changing layout), styled names,
//! server tags and nameplates.
//!
//! Everything here takes already-created image handles from the `App`
//! (`App::decoration_frame`, ...): handles are never built in `view`.

use iced::advanced::widget::{self, Tree};
use iced::advanced::{Layout, Widget, layout, renderer};
use iced::widget::{Space, container, row, text};
use iced::{Alignment, Color, Element, Font, Length, Padding, Point, Rectangle, Size, mouse};

use super::theme::{self, INTER_BOLD, INTER_SEMIBOLD};
use super::{El, avatar_circle};
use crate::app::cosmetics::{DEC_LIST, DEC_POP, Phase};
use crate::app::{App, Message, Ui};
use crate::net::cosmetics::{DECORATION_SCALE, NamePaint, gradient_at};
use crate::net::model::User;

/// A fixed-size slot whose child is laid out at its own size and offset,
/// possibly larger than the slot (decoration frames, effect layers). The
/// slot's size is what its parent sees, so the overflow never moves
/// neighbouring widgets.
pub struct Placed<'a, M> {
    content: Element<'a, M>,
    width: Length,
    height: Length,
    child: Size,
    offset: Point,
    clip: bool,
}

impl<'a, M> Placed<'a, M> {
    pub fn new(content: impl Into<Element<'a, M>>, child: Size, offset: Point) -> Self {
        Self {
            content: content.into(),
            width: Length::Fixed(child.width),
            height: Length::Fixed(child.height),
            child,
            offset,
            clip: false,
        }
    }

    pub fn slot(mut self, width: impl Into<Length>, height: impl Into<Length>) -> Self {
        self.width = width.into();
        self.height = height.into();
        self
    }

    /// Clip the child to the slot.
    pub fn clip(mut self, clip: bool) -> Self {
        self.clip = clip;
        self
    }
}

impl<M> Widget<M, iced::Theme, iced::Renderer> for Placed<'_, M> {
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> Size<Length> {
        Size::new(self.width, self.height)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let limits = limits.width(self.width).height(self.height);
        let node = self
            .content
            .as_widget_mut()
            .layout(
                &mut tree.children[0],
                renderer,
                &layout::Limits::new(Size::ZERO, self.child),
            )
            .move_to(self.offset);
        let size = limits.resolve(self.width, self.height, Size::ZERO);
        layout::Node::with_children(size, vec![node])
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &iced::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        use iced::advanced::Renderer as _;
        let Some(child) = layout.children().next() else {
            return;
        };
        if self.clip {
            let bounds = layout.bounds();
            renderer.with_layer(bounds, |renderer| {
                self.content.as_widget().draw(
                    &tree.children[0],
                    renderer,
                    theme,
                    style,
                    child,
                    cursor,
                    viewport,
                );
            });
        } else {
            self.content.as_widget().draw(
                &tree.children[0],
                renderer,
                theme,
                style,
                child,
                cursor,
                viewport,
            );
        }
    }

    fn mouse_interaction(
        &self,
        _tree: &Tree,
        _layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        mouse::Interaction::None
    }

    fn operate(
        &mut self,
        _tree: &mut Tree,
        _layout: Layout<'_>,
        _renderer: &iced::Renderer,
        _operation: &mut dyn widget::Operation,
    ) {
    }
}

impl<'a, M: 'a> From<Placed<'a, M>> for Element<'a, M> {
    fn from(p: Placed<'a, M>) -> Self {
        Element::new(p)
    }
}

/// The decoration frame for a `size` px avatar, drawn over it at
/// [`DECORATION_SCALE`]. The slot is `size` x `size`.
pub fn decoration<'a>(app: &'a App, user: &User, size: f32, phase: Phase) -> Option<El<'a>> {
    let deco = user.avatar_decoration.as_ref()?;
    let cdn_size = if size > 64.0 { DEC_POP } else { DEC_LIST };
    let big = size * DECORATION_SCALE;
    let off = -(big - size) / 2.0;
    // Until the image arrives the slot stays (an empty one) so the widget
    // tree keeps its shape.
    let img: El<'a> = match app.decoration_frame(deco, cdn_size, phase) {
        Some(h) => iced::widget::image(h.clone()).width(big).height(big).into(),
        None => Space::new().width(big).height(big).into(),
    };
    Some(
        Placed::new(img, Size::new(big, big), Point::new(off, off))
            .slot(size, size)
            .into(),
    )
}

/// Round avatar plus its decoration, in a `size` slot.
pub fn avatar_decorated<'a>(app: &'a App, user: &User, size: f32, phase: Phase) -> El<'a> {
    let base = avatar_circle(app, user, size, phase);
    match decoration(app, user, size, phase) {
        Some(d) => iced::widget::stack![base, d]
            .width(size)
            .height(size)
            .into(),
        None => base,
    }
}

/// A name in the user's display-name style (colour / gradient / weight),
/// or `base` coloured when they have none.
pub fn name<'a>(app: &App, user: &User, name: &str, size: f32, font: Font, base: Color) -> El<'a> {
    let _ = app;
    let plain = |color: Color, font: Font| -> El<'a> {
        text(name.to_owned())
            .size(size)
            .font(font)
            .color(color)
            .wrapping(text::Wrapping::None)
            .into()
    };
    let Some(style) = user.name_style.as_ref() else {
        return plain(base, font);
    };
    let font = if style.bold() { INTER_BOLD } else { font };
    match style.paint() {
        None => plain(base, font),
        Some(NamePaint::Solid(c)) => plain(theme::from_int(c), font),
        Some(NamePaint::Gradient(stops)) => {
            let n = name.chars().count();
            let spans: Vec<iced::widget::text::Span<'a, Ui, Font>> = name
                .chars()
                .enumerate()
                .map(|(i, ch)| {
                    iced::widget::span(ch.to_string())
                        .color(theme::from_int(gradient_at(&stops, i, n)))
                        .font(font)
                })
                .collect();
            iced::widget::rich_text(spans)
                .size(size)
                .wrapping(text::Wrapping::None)
                .into()
        }
    }
}

/// Server tag chip (`[badge] TAG`), if the user shows one.
pub fn tag_chip<'a>(app: &'a App, user: &User) -> Option<El<'a>> {
    let g = user.primary_guild.as_ref()?;
    let tag = g.visible_tag()?;
    let p = app.theme.palette;
    let mut r = row![].spacing(3).align_y(Alignment::Center);
    if let Some(h) = app.tag_badge_handle(g) {
        r = r.push(iced::widget::image(h.clone()).width(12).height(12));
    }
    r = r.push(
        text(tag.to_owned())
            .size(11)
            .font(INTER_SEMIBOLD)
            .color(p.text_secondary)
            .wrapping(text::Wrapping::None),
    );
    Some(
        container(r)
            .padding(Padding::from([1, 5]))
            .style(theme::rounded_bg(p.bg_hover, 4.0))
            .into(),
    )
}

/// Draws the user's nameplate behind `content` (a list row). The row keeps
/// its size: the plate fills whatever space `content` takes.
pub fn with_nameplate<'a>(app: &'a App, user: &User, content: El<'a>) -> El<'a> {
    let Some(h) = user
        .collectibles
        .as_ref()
        .and_then(|c| c.nameplate.as_ref())
        .and_then(|n| app.nameplate_handle(n))
    else {
        return content;
    };
    let plate = iced::widget::image(h.clone())
        .width(Length::Fill)
        .height(Length::Fill)
        .content_fit(iced::ContentFit::Cover)
        .opacity(0.9_f32);
    iced::widget::Stack::new()
        .width(Length::Fill)
        .push(content)
        .push_under(plate)
        .into()
}

/// Wraps `el` so a click opens the user's profile popout.
pub fn clickable<'a>(
    el: impl Into<El<'a>>,
    user: &User,
    guild: Option<crate::net::model::GuildId>,
) -> El<'a> {
    iced::widget::mouse_area(el)
        .on_press(Message::Ui(Ui::OpenProfile {
            user: user.id,
            guild,
        }))
        .interaction(mouse::Interaction::Pointer)
        .into()
}
