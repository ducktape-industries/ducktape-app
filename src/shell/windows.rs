use super::*;
use gpui_kit::{Bounds, Pixels, Size, point, px, size};

/// A window's size before the person resizes it.
pub(super) const WINDOW_SIZE: (f32, f32) = (1280., 800.);

/// How far a pop-out steps down and right of the window it left.
const CASCADE: f32 = 32.;

/// Where a pop-out opens: stepped off its source window so it does not
/// land exactly on top of it, and pulled back inside `display` when the
/// step would push it off an edge.
pub(super) fn cascade(source: Bounds<Pixels>, display: Option<Bounds<Pixels>>) -> Bounds<Pixels> {
    let extent = size(px(WINDOW_SIZE.0), px(WINDOW_SIZE.1));
    let mut origin = point(source.origin.x + px(CASCADE), source.origin.y + px(CASCADE));
    if let Some(display) = display {
        let right = display.origin.x + display.size.width - extent.width;
        let bottom = display.origin.y + display.size.height - extent.height;
        origin.x = origin.x.min(right).max(display.origin.x);
        origin.y = origin.y.min(bottom).max(display.origin.y);
    }
    Bounds::new(origin, extent)
}

/// Where the launcher sits when the desk shrinks to it: centred in
/// `display`, and never past its top-left corner when it doesn't fit.
pub(super) fn centered(extent: Size<Pixels>, display: Bounds<Pixels>) -> Bounds<Pixels> {
    let origin = point(
        display.origin.x + ((display.size.width - extent.width) / 2.).max(px(0.)),
        display.origin.y + ((display.size.height - extent.height) / 2.).max(px(0.)),
    );
    Bounds::new(origin, extent)
}

/// Where a window leaving the desk opens: on the screen right where it sat,
/// under the console's bar rather than over it, and inside `display`.
/// With no place on the desk to keep, it cascades.
pub(super) fn unseated(
    source: Bounds<Pixels>,
    frame: Option<layout::Frame>,
    display: Option<Bounds<Pixels>>,
) -> Bounds<Pixels> {
    let Some(frame) = frame else {
        return cascade(source, display);
    };
    let extent = size(px(frame.w.max(720.)), px(frame.h.max(480.)));
    let mut origin = point(
        source.origin.x + px(frame.x),
        source.origin.y + px(desk::BAR + frame.y),
    );
    if let Some(display) = display {
        let right = display.origin.x + display.size.width - extent.width;
        let bottom = display.origin.y + display.size.height - extent.height;
        origin.x = origin.x.min(right).max(display.origin.x);
        origin.y = origin.y.min(bottom).max(display.origin.y);
    }
    Bounds::new(origin, extent)
}

impl DesktopWindow {
    /// A window of `kind`, focused on its own root (so the first Tab
    /// reaches the first control), telling the model when it gains or
    /// loses focus.
    pub(super) fn new(
        model: Entity<Desktop>,
        key: WindowKey,
        kind: WindowKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.on_release(Self::released).detach();
        let observer = cx.observe(&model, |_, _, cx| cx.notify());
        let activation =
            cx.observe_window_activation(window, move |this: &mut Self, window, cx| {
                let message = match window.is_window_active() {
                    true => Message::WindowFocused(key),
                    false => Message::WindowUnfocused(key),
                };
                let model = this.model.clone();
                cx.defer(move |cx| model.update(cx, |model, cx| model.dispatch(message, cx)));
            });
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        Self {
            model,
            key,
            kind,
            drag: None,
            inputs: HashMap::new(),
            spotlight_focused: false,
            bar_buttons: Default::default(),
            rail: Default::default(),
            bar_needs: 0.,
            bar_made: 0,
            bar_drawn: None,
            focus,
            _activation: activation,
            _observer: observer,
            _focus_lost: cx.on_focus_lost(window, |this, window, cx| this.focus_lost(window, cx)),
        }
    }
}

impl Desktop {
    pub(super) fn new(state: Ducktape, tray: crate::tray::Tray) -> Self {
        Self {
            state,
            tray,
            windows: BTreeMap::new(),
            views: BTreeMap::new(),
            streams: HashMap::new(),
            mounted: BTreeMap::new(),
            desk_bounds: None,
        }
    }

    pub(super) fn open_window(
        &mut self,
        key: WindowKey,
        kind: WindowKind,
        reply: oneshot::Sender<WindowKey>,
        at: Option<Bounds<Pixels>>,
        cx: &mut Context<Self>,
    ) {
        use gpui_kit::*;
        let title = match kind {
            crate::shell::WindowKind::Console => "Ducktape".to_owned(),
            crate::shell::WindowKind::View { module } => panes::label(module),
        };
        // the launcher is a small window of a fixed size; the desk grows
        let launcher = kind == crate::shell::WindowKind::Console && self.state.in_launcher();
        let extent = match kind {
            crate::shell::WindowKind::Console if launcher => {
                size(px(launcher::LAUNCHER_SIZE.0), px(launcher::LAUNCHER_SIZE.1))
            }
            crate::shell::WindowKind::Console => size(px(WINDOW_SIZE.0), px(WINDOW_SIZE.1)),
            crate::shell::WindowKind::View { .. } => size(px(WINDOW_SIZE.0), px(WINDOW_SIZE.1)),
        };
        let model = cx.entity();
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(
                at.unwrap_or_else(|| Bounds::centered(None, extent, cx)),
            )),
            titlebar: Some(TitlebarOptions {
                title: Some(title.into()),
                appears_transparent: cfg!(target_os = "macos"),
                // the buttons are 14px tall: centred in the 36px bar
                traffic_light_position: Some(point(px(11.), px(11.))),
            }),
            // one window serves the launcher and the desk, so it resizes
            window_min_size: Some(gpui_kit::size(px(720.), px(480.))),
            app_id: Some("dev.ducktape.app".into()),
            kind: gpui_kit::WindowKind::Normal,
            icon: image::RgbaImage::from_raw(
                128,
                128,
                include_bytes!("../../assets/icon.rgba").to_vec(),
            )
            .map(std::sync::Arc::new),
            ..Default::default()
        };
        cx.defer(move |cx| {
            let mut opened_view = None;
            let window_model = model.clone();
            let opened = cx.open_window(options, |window, cx| {
                let view = cx.new(|cx| DesktopWindow::new(window_model, key, kind, window, cx));
                opened_view = Some(view.downgrade());
                let closing = view.downgrade();
                window.on_window_should_close(cx, move |window, cx| {
                    let _ = closing.update(cx, |this, cx| {
                        this.observe_window(view_wire::events::Window::CloseRequested, cx)
                    });
                    release_window_input(window, cx);
                    true
                });
                cx.new(|cx| gpui_kit::component::Root::new(view, window, cx))
            });
            match opened {
                Ok(handle) => {
                    model.update(cx, |model, _| {
                        model.windows.insert(key, handle.into());
                        if let Some(view) = opened_view {
                            model.views.insert(key, view);
                        }
                    });
                    let _ = reply.send(key);
                }
                Err(error) => {
                    tracing::error!(target: "ducktape::app", reason = "native_window_open_failed", %error, "window could not be opened");
                    model.update(cx, |model, cx| {
                        // a pane on its way to this window goes back to the desk
                        model.dispatch(Message::Pane(key, PaneMessage::PopIn), cx);
                        model.dispatch(Message::WindowWasClosed(key), cx);
                        model.state.error = format!("The window could not be opened: {error}");
                        cx.notify();
                    });
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(x: f32, y: f32, width: f32, height: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(width), px(height)))
    }

    #[test]
    fn a_window_leaving_the_desk_opens_where_it_sat_under_the_bar() {
        let seat = layout::Frame {
            x: 100.,
            y: 50.,
            w: 900.,
            h: 600.,
        };
        let display = frame(0., 0., 2560., 1440.);
        let at = unseated(frame(200., 100., 1280., 800.), Some(seat), Some(display));
        assert_eq!(at, frame(300., 100. + desk::BAR + 50., 900., 600.));
        assert!(
            at.origin.y >= px(100. + desk::BAR),
            "the console's bar stays uncovered"
        );
    }

    #[test]
    fn a_popout_steps_off_its_source_window() {
        let display = frame(0., 0., 1600., 1000.);
        let at = cascade(frame(160., 100., 1280., 800.), Some(display));
        assert_eq!(at, frame(192., 132., 1280., 800.));
    }

    #[test]
    fn a_popout_stays_on_the_display() {
        let display = frame(0., 0., 1600., 1000.);
        let at = cascade(frame(320., 200., 1280., 800.), Some(display));
        assert_eq!(at, frame(320., 200., 1280., 800.));
        let small = frame(0., 0., 1024., 700.);
        assert_eq!(
            cascade(frame(0., 0., 1024., 700.), Some(small)).origin,
            point(px(0.), px(0.))
        );
    }

    #[test]
    fn the_launcher_comes_up_centred_on_its_display() {
        let launcher = size(px(960.), px(640.));
        // a second display to the right, under a 25px menu bar
        let display = frame(1920., 25., 2560., 1415.);
        assert_eq!(
            centered(launcher, display),
            frame(1920. + 800., 25. + 387.5, 960., 640.)
        );
        // too small to hold it: pinned to the corner, not pushed off it
        let small = frame(0., 0., 800., 600.);
        assert_eq!(centered(launcher, small).origin, point(px(0.), px(0.)));
    }
}
