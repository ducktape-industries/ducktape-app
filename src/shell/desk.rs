//! The desk: a menu bar across the top, the open programs' windows under
//! it. The bar holds, left to right: the network (its menu switches), the
//! network's programs as tabs, then Search (⌘K), the node's breath (its
//! status on a click), who is signed in (their menu), and Settings.

use super::*;
use crate::{Overlay, Popover};

/// The menu bar's height.
pub(super) const BAR: f32 = 36.;

impl DesktopWindow {
    /// Inside a node. Reached with an unlocked key, or by choosing to read
    /// without one.
    pub(super) fn console(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        use gpui_kit::*;
        let state = self.model.read(cx).state.clone_facts();
        let rail = crate::runtime::rail();
        if rail.iter().any(|row| row.note == Some("Loading")) {
            window.request_animation_frame();
        }
        // the bar folds its words once, drawn whole, its tabs ran past their
        // strip at this width: all of them fold together, none is cut.
        // `bar_needs` only grows while the bar is made of the same words; new
        // words (a network switched, a program listed or gone, a sign-in)
        // measure again, or the bar would stay folded for words it no longer
        // shows. Badges are left out: they tick while folded, and a
        // re-measure draws the bar whole for a frame.
        let made_of = {
            use std::hash::{Hash as _, Hasher as _};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            state.network.hash(&mut hasher);
            for row in &rail {
                (row.module, &row.label, row.note, row.empty).hash(&mut hasher);
            }
            (state.signer_key.is_empty(), &state.account).hash(&mut hasher);
            hasher.finish()
        };
        if self.bar_made != made_of {
            self.bar_made = made_of;
            self.bar_needs = 0.;
        }
        let width = f32::from(window.viewport_size().width);
        let over = f32::from(self.rail.max_offset().x);
        if let Some(drawn) = self.bar_drawn
            && over > 0.
            && drawn + over > self.bar_needs
        {
            self.bar_needs = drawn + over;
            window.refresh();
        }
        let narrow = width < self.bar_needs;
        self.bar_drawn = (!narrow).then_some(width);
        // the desk's size, and on an untouched console the program it opens
        let desk = self.desk(window);
        let layout = self.layout(cx);
        let seed = (self.kind == crate::shell::WindowKind::Console && !layout.initialized)
            .then(|| {
                state
                    .active
                    .or_else(|| rail.iter().find(|row| !row.empty).map(|row| row.module))
            })
            .flatten();
        if layout.desk != Some(desk) || seed.is_some() {
            let window = self.key;
            self.model.update(cx, |model, cx| {
                model.dispatch(Message::DeskShown { window, desk, seed }, cx)
            });
        }
        // the policy's "in front": this window, if it is, and the view
        // focused in it
        let layout = self.layout(cx);
        let focused = layout
            .panes
            .get(layout.focused)
            .map_or(layout::EMPTY, |pane| pane.module);
        crate::runtime::notify::center().set_front(self.key, window.is_window_active(), focused);
        let console = self.kind == crate::shell::WindowKind::Console;
        let bar = console.then(|| self.menubar(&state, &rail, narrow, window, cx));
        let seat = self.pane_stage(window, cx);
        let overlay = match state.overlay.filter(|_| console) {
            None => None,
            Some(Overlay::Spotlight) => Some(self.spotlight(&state, window, cx)),
            Some(Overlay::Approve) => Some(self.approve(&state, window, cx)),
            Some(Overlay::Settings) => Some(self.settings(&state)),
            Some(Overlay::Network) => Some(self.network_menu(&state, narrow, window)),
            Some(Overlay::Menu(Popover::Node)) => Some(self.node_menu(&state, window, cx)),
            Some(Overlay::Menu(Popover::Account)) => Some(self.account_menu(&state, window, cx)),
            Some(Overlay::Menu(Popover::Notifications)) => {
                Some(self.notifications(&state, window, cx))
            }
        };
        if state.overlay != Some(Overlay::Spotlight) {
            self.spotlight_focused = false;
        }
        div()
            .id("console")
            .size_full()
            .flex()
            .flex_col()
            .children(bar)
            .child(div().id("seat").flex_1().min_h_0().w_full().child(seat))
            .children(overlay)
            .children(self.footer(cx))
            .into_any_element()
    }

    /// Something open over the desk, below the bar (its items stay live,
    /// and one menu gives way to the next in one click): a backdrop that
    /// closes it on a click, dimmed when `scrim`, and on it the card the
    /// canvas dresses its menus and dialogs in, `border: 1.5px solid ink`
    /// and a soft shadow. `dress` places and fills the card. Escape is
    /// `global_key`'s.
    #[allow(clippy::too_many_arguments, reason = "one frame, five overlays")]
    pub(super) fn overlay(
        &self,
        id: &'static str,
        role: gpui_kit::Role,
        name: &'static str,
        closes: crate::Overlay,
        scrim: bool,
        ink: &super::ink::Ink,
        dress: impl FnOnce(gpui_kit::Stateful<gpui_kit::Div>) -> gpui_kit::AnyElement,
    ) -> gpui_kit::AnyElement {
        use gpui_kit::*;
        let model = self.model.clone();
        let backdrop = div()
            .id(SharedString::from(format!("{id}-backdrop")))
            .absolute()
            .top(px(BAR))
            .left_0()
            .right_0()
            .bottom_0()
            .occlude()
            .when(scrim, |backdrop| {
                backdrop.bg(ink.bg.opacity(0.6)).flex().justify_center()
            })
            .on_click(move |_, _, cx| {
                model.update(cx, |model, cx| {
                    model.dispatch(Message::CloseOverlay(closes), cx)
                });
            });
        let card = div()
            .id(id)
            .control(role, name)
            .occlude()
            .flex()
            .flex_col()
            .bg(ink.bg)
            .text_color(ink.ink)
            .border(px(1.5))
            .border_color(ink.ink)
            .shadow_lg()
            .on_click(|_, _, cx| cx.stop_propagation());
        backdrop.child(dress(card)).into_any_element()
    }

    /// A menu hanging below the bar (the canvas's menus: `top: 40px`), its
    /// right edge under its button's.
    pub(super) fn hanging(
        &self,
        which: crate::Popover,
        width: f32,
        body: impl IntoElement,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        use gpui_kit::*;
        let ink = super::ink::Ink::of(self.model.read(cx).state.dark());
        let (id, name) = match which {
            crate::Popover::Node => ("node-status", "Node status"),
            crate::Popover::Account => ("account-menu", "Account"),
            crate::Popover::Notifications => ("notifications", "Notifications"),
        };
        let overlay = Overlay::Menu(which);
        let at = self.under_button(overlay, Anchor::TopRight, window);
        self.overlay(id, Role::Dialog, name, overlay, false, &ink, |card| {
            at.child(card.w(px(width)).child(body)).into_any_element()
        })
    }

    /// Where `overlay`'s menu hangs: `anchor` (a top corner) at the same
    /// corner of its bar button, 4px below the bar, shifted back inside the
    /// window when it would run off it. Before the button was ever painted,
    /// the window's edge on that side.
    pub(super) fn under_button(
        &self,
        overlay: Overlay,
        anchor: gpui_kit::Anchor,
        window: &Window,
    ) -> gpui_kit::Anchored {
        use gpui_kit::*;
        let left = anchor == Anchor::TopLeft;
        let x = match self.bar_buttons.get(&overlay) {
            Some(button) if left => button.left(),
            Some(button) => button.right(),
            None if left => px(8.),
            None => window.viewport_size().width - px(8.),
        };
        anchored()
            .position(point(x, px(BAR + 4.)))
            .anchor(anchor)
            .snap_to_window_with_margin(px(8.))
    }

    /// A menu item: `height: 36px; padding: 0 16px; font: 400 14px`, a mono
    /// hint at its right end.
    pub(super) fn menu_row(
        &self,
        id: &'static str,
        label: &'static str,
        run: impl Fn(&mut gpui_kit::App) + 'static,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        self.menu_row_hint(id, label, "", run, cx)
    }

    pub(super) fn menu_row_hint(
        &self,
        id: &'static str,
        label: &'static str,
        hint: &'static str,
        run: impl Fn(&mut gpui_kit::App) + 'static,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        use super::ink::{Ink, mono, sans};
        use gpui_kit::*;
        let ink = Ink::of(self.model.read(cx).state.dark());
        let surface = ink.surface;
        crate::a11y::keyboard(
            sans(400, 14.)
                .id(id)
                .control(Role::MenuItem, label)
                .h(px(36.))
                .px(px(16.))
                .flex()
                .items_center()
                .justify_between()
                .cursor_pointer()
                .hover(move |style| style.bg(surface))
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    run(cx)
                })
                .child(label)
                .child(mono(400, 12.).text_color(ink.muted).child(hint)),
        )
    }

    pub(super) fn dispatching(
        &self,
        message: fn() -> Message,
    ) -> impl Fn(&mut gpui_kit::App) + 'static {
        let model = self.model.clone();
        move |cx| model.update(cx, |model, cx| model.dispatch(message(), cx))
    }
}
