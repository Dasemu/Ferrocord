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
pub struct Placed<'a, M, T = iced::Theme, R = iced::Renderer> {
    content: Element<'a, M, T, R>,
    width: Length,
    height: Length,
    child: Size,
    offset: Point,
    clip: bool,
}

impl<'a, M, T, R> Placed<'a, M, T, R> {
    pub fn new(content: impl Into<Element<'a, M, T, R>>, child: Size, offset: Point) -> Self {
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

impl<M, T, R: renderer::Renderer> Widget<M, T, R> for Placed<'_, M, T, R> {
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> Size<Length> {
        Size::new(self.width, self.height)
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &R, limits: &layout::Limits) -> layout::Node {
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
        renderer: &mut R,
        theme: &T,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
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

    // Events reach the child: a self-playing animation (decoration,
    // effect layer) needs every `RedrawRequested` to advance and to ask
    // for its next frame. Without this the child never moves.
    fn update(
        &mut self,
        tree: &mut Tree,
        event: &iced::Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &R,
        clipboard: &mut dyn iced::advanced::Clipboard,
        shell: &mut iced::advanced::Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        let Some(child) = layout.children().next() else {
            return;
        };
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            child,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        _tree: &Tree,
        _layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &R,
    ) -> mouse::Interaction {
        mouse::Interaction::None
    }

    fn operate(
        &mut self,
        _tree: &mut Tree,
        _layout: Layout<'_>,
        _renderer: &R,
        _operation: &mut dyn widget::Operation,
    ) {
    }
}

impl<'a, M: 'a, T: 'a, R: renderer::Renderer + 'a> From<Placed<'a, M, T, R>>
    for Element<'a, M, T, R>
{
    fn from(p: Placed<'a, M, T, R>) -> Self {
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
    let img: El<'a> = if let Some(a) = app.decoration_anim(deco, cdn_size, phase) {
        let (epoch, playing) = app.anim_clock(phase);
        super::anim_image::anim_image(a.clone(), epoch, playing, big, big).into()
    } else {
        match app.decoration_frame(deco, cdn_size) {
            Some(h) => iced::widget::image(h.clone()).width(big).height(big).into(),
            None => Space::new().width(big).height(big).into(),
        }
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
        super::emoji_font::emoji_text(name, size, color, font, false)
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

/// Wraps `el` so a click opens the user's profile popout and a right click
/// the user menu.
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
        .on_right_press(Message::Ui(Ui::ContextOpen(
            crate::app::context::CtxTarget::User {
                user: user.id,
                guild,
            },
        )))
        .interaction(mouse::Interaction::Pointer)
        .into()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use iced::advanced::widget::Tree;
    use iced::advanced::{Shell, Widget, layout};
    use iced::{Element, Event, Point, Rectangle, Size, mouse, window};

    use super::Placed;
    use crate::app::anim::from_rgba_frames;
    use crate::ui::anim_image::anim_image;

    /// Regression: `Placed` (decoration frames, effect layers) swallowed
    /// every event, so the animation inside never saw a redraw and stayed
    /// on its first frame. A redraw must reach it and schedule the next.
    #[test]
    fn placed_children_receive_redraws_and_keep_animating() {
        let anim = Arc::new(from_rgba_frames(vec![
            (1, 1, vec![0u8; 4], Duration::from_millis(40)),
            (1, 1, vec![9u8; 4], Duration::from_millis(40)),
        ]));
        let epoch = Instant::now();
        let child: Element<'_, (), (), ()> = anim_image(anim, epoch, true, 48.0, 48.0).into();
        let mut placed =
            Placed::new(child, Size::new(48.0, 48.0), Point::new(-4.0, -4.0)).slot(40.0, 40.0);
        let mut tree = Tree::new(&placed as &dyn Widget<(), (), ()>);
        let node = placed.layout(
            &mut tree,
            &(),
            &layout::Limits::new(Size::ZERO, Size::new(500.0, 500.0)),
        );
        let lay = iced::advanced::Layout::new(&node);
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        let mut clipboard = iced::advanced::clipboard::Null;
        let now = epoch + Duration::from_millis(10);
        placed.update(
            &mut tree,
            &Event::Window(window::Event::RedrawRequested(now)),
            lay,
            mouse::Cursor::Unavailable,
            &(),
            &mut clipboard,
            &mut shell,
            &Rectangle::new(Point::ORIGIN, Size::new(500.0, 500.0)),
        );
        match shell.redraw_request() {
            window::RedrawRequest::At(at) => {
                assert!(at >= epoch + Duration::from_millis(40));
                assert!(at <= epoch + Duration::from_millis(60));
            }
            other => panic!("the animation asked for no redraw: {other:?}"),
        }
    }
}
