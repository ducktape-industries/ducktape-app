//! The native chrome, and nothing a network decides: a window, a screen to
//! reach a node and unlock a key, a menu bar of whatever programs that node
//! runs, and one seat that draws the open program's view.

use crate::a11y::Control as _;
use futures::{
    StreamExt as _,
    channel::{mpsc, oneshot},
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    AppContext as _, AsyncApp, Context, Entity, IntoElement, ParentElement as _, Render,
    Styled as _, Window,
};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, OnceLock};
use view_wire::Task;

use crate::ui::layout::{self, PaneMessage};
use crate::{AppMessage as Message, Ducktape, Stage};

#[cfg(debug_assertions)]
mod fixtures;
#[cfg(debug_assertions)]
pub(crate) use fixtures::render_tree_fixture;
mod approve;
mod desk;
mod figure;
mod ink;
mod keys;
mod launch;
mod launcher;
mod menubar;
mod menus;
mod notifications;
mod panes;
#[cfg(test)]
mod panes_tests;
mod screens;
#[cfg(test)]
mod screens_tests;
mod spotlight;
mod windows;

pub(crate) use launch::run;
mod settings;
mod sign_in;
mod spin;
mod theme;

#[cfg(not(target_os = "macos"))]
use crate::fonts::EMOJI_FACE;
use crate::fonts::{BUNDLED_FACES, fallback_chain};
use theme::configure_native_theme;

pub(crate) use crate::runtime::WindowKey;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WindowKind {
    Console,
    View { module: &'static str },
}

/// How the platform writes a command chord: "⌘K" on a Mac, "Ctrl K"
/// elsewhere.
pub(crate) fn chord_label(key: &str) -> String {
    match cfg!(target_os = "macos") {
        true => format!("⌘{key}"),
        false => format!("Ctrl {key}"),
    }
}

pub(crate) enum Command {
    Open {
        key: WindowKey,
        kind: WindowKind,
        /// Where it opens; `None` is the display's centre.
        at: Option<gpui_kit::Bounds<gpui_kit::Pixels>>,
        reply: oneshot::Sender<WindowKey>,
    },
    Raise(WindowKey),
    /// A window off the screen (a pop-out whose pane left it).
    Close(WindowKey),
    /// The appearance the model holds, onto the app's theme.
    SyncAppearance,
    /// The console window to the launcher's size or the desk's, as the
    /// stage now is.
    SwapConsole,
    /// A link pressed off the window thread: the reducer reads it.
    OpenLink(String),
    /// A web page, for the system browser.
    OpenUrl(String),
    Quit,
}

pub(crate) struct PendingCommand {
    pub command: Command,
    pub completed: oneshot::Sender<()>,
}

fn sender() -> &'static Mutex<Option<mpsc::UnboundedSender<PendingCommand>>> {
    static SENDER: OnceLock<Mutex<Option<mpsc::UnboundedSender<PendingCommand>>>> = OnceLock::new();
    SENDER.get_or_init(Mutex::default)
}

pub(crate) fn commands() -> mpsc::UnboundedReceiver<PendingCommand> {
    let (send, receive) = mpsc::unbounded();
    let mut current = sender().lock().expect("native shell commands");
    assert!(current.is_none(), "one native shell per process");
    // the layers below hand these up without naming the shell
    crate::runtime::notify::on_open_link(open_link);
    crate::backend::passkey::on_open_url(open_url_now);
    *current = Some(send);
    receive
}

async fn send(command: Command) {
    let (completed, received) = oneshot::channel();
    let pending = PendingCommand { command, completed };
    let sent = sender()
        .lock()
        .expect("native shell commands")
        .as_ref()
        .is_some_and(|sender| sender.unbounded_send(pending).is_ok());
    if !sent {
        tracing::error!(target: "ducktape::app", reason = "native_shell_closed", "native window command could not be delivered");
        return;
    }
    let _ = received.await;
}

pub(crate) fn open(kind: WindowKind) -> (WindowKey, Task<WindowKey>) {
    open_at(kind, None)
}

/// A window of `kind` opened at `at`, its key known before it is.
pub(crate) fn open_at(
    kind: WindowKind,
    at: Option<gpui_kit::Bounds<gpui_kit::Pixels>>,
) -> (WindowKey, Task<WindowKey>) {
    let key = WindowKey::unique();
    let task = Task::stream(
        futures::stream::once(async move {
            let (reply, receive) = oneshot::channel();
            send(Command::Open {
                key,
                kind,
                at,
                reply,
            })
            .await;
            receive.await.ok()
        })
        .filter_map(std::future::ready),
    );
    (key, task)
}

fn effect<M: 'static>(command: Command) -> Task<M> {
    Task::future(async move {
        send(command).await;
    })
    .discard()
}

pub(crate) fn raise<M: 'static>(key: WindowKey) -> Task<M> {
    effect(Command::Raise(key))
}

pub(crate) fn close<M: 'static>(key: WindowKey) -> Task<M> {
    effect(Command::Close(key))
}

pub(crate) fn swap_console<M: 'static>() -> Task<M> {
    effect(Command::SwapConsole)
}

pub(crate) fn sync_appearance<M: 'static>() -> Task<M> {
    effect(Command::SyncAppearance)
}

pub(crate) fn quit<M: 'static>() -> Task<M> {
    effect(Command::Quit)
}

/// A web page, in the system browser.
pub(crate) fn open_url<M: 'static>(url: String) -> Task<M> {
    effect(Command::OpenUrl(url))
}

/// A link pressed off the window thread (a banner's click), handed to the
/// reducer as `Message::OpenLink`.
fn open_link(url: String) {
    post(Command::OpenLink(url));
}

/// A web page for the system browser, asked for off the window thread
/// (the passkey ceremony's page).
fn open_url_now(url: String) {
    post(Command::OpenUrl(url));
}

/// A command sent without waiting for it to be done.
fn post(command: Command) {
    let (completed, _dropped) = oneshot::channel();
    let pending = PendingCommand { command, completed };
    let sent = sender()
        .lock()
        .expect("native shell commands")
        .as_ref()
        .is_some_and(|sender| sender.unbounded_send(pending).is_ok());
    if !sent {
        tracing::error!(target: "ducktape::app", reason = "native_shell_closed", "a link could not be delivered");
    }
}

/// A stream that yields every `period`.
pub(crate) fn every(period: std::time::Duration) -> impl futures::Stream<Item = ()> {
    futures::stream::unfold((), move |()| async move {
        tokio::time::sleep(period).await;
        Some(((), ()))
    })
}

/// How long a window leaving full size has to report its frame.
const LEAVE_FULL_SIZE: std::time::Duration = std::time::Duration::from_secs(3);
/// How long a frame has to stand before it is the one: a window manager
/// can send the state and the frame as separate events, in either order.
const FRAME_SETTLES: std::time::Duration = std::time::Duration::from_millis(100);

/// The frame a window leaving full size went back to, from the frames it
/// reports (`None` while still full size): the last one once no other
/// follows for a moment, or whatever it last said when time runs out.
async fn left_full_size(
    mut frames: futures::channel::mpsc::UnboundedReceiver<
        Option<gpui_kit::Bounds<gpui_kit::Pixels>>,
    >,
    cx: &gpui_kit::AsyncApp,
) -> Option<gpui_kit::Bounds<gpui_kit::Pixels>> {
    let clock = cx.background_executor();
    let give_up = clock.now() + LEAVE_FULL_SIZE;
    let mut last = None;
    loop {
        let left = give_up.saturating_duration_since(clock.now());
        let wait = if last.is_some() {
            FRAME_SETTLES.min(left)
        } else {
            left
        };
        let timer = std::pin::pin!(clock.timer(wait));
        match futures::future::select(frames.next(), timer).await {
            futures::future::Either::Left((Some(frame), _)) => last = frame.or(last),
            _ => return last,
        }
    }
}

// ---------- the desktop actor ----------

struct Desktop {
    state: Ducktape,
    tray: crate::tray::Tray,
    windows: BTreeMap<WindowKey, gpui_kit::AnyWindowHandle>,
    views: BTreeMap<WindowKey, gpui_kit::WeakEntity<DesktopWindow>>,
    streams: HashMap<u64, gpui_kit::Task<()>>,
    /// Every pane's view, by its instance: a pane keeps its view whichever
    /// window the model puts it in.
    mounted: BTreeMap<u64, panes::MountedPane>,
    /// Where the desk window was when it last gave way to the launcher:
    /// it comes back there.
    desk_bounds: Option<gpui_kit::WindowBounds>,
}

impl Desktop {
    fn ax_windows(&self) -> Vec<(String, gpui_kit::AnyWindowHandle)> {
        let mut nth = 0;
        self.windows
            .values()
            .map(|handle| {
                nth += 1;
                let name = match nth {
                    1 => "console".to_owned(),
                    nth => format!("console{nth}"),
                };
                (name, *handle)
            })
            .collect()
    }

    fn dispatch(&mut self, message: Message, cx: &mut Context<Self>) {
        let runtime = crate::runtime::handle();
        let _runtime = runtime.enter();
        let task = self.state.handle(message);
        self.mount(cx);
        self.tray.sync(&self.state);
        self.start(task, cx).detach();
        self.subscriptions(cx);
        cx.notify();
    }

    /// The launcher and the desk are one window: crossing from one to
    /// the other resizes it in place rather than closing it and opening
    /// another. The launcher comes up centred on the window's display; the
    /// desk comes back where it was, at its size, maximized or fullscreen
    /// again if it had been.
    fn swap_console(&mut self, cx: &mut Context<Self>) {
        use gpui_kit::WindowBounds;
        let Some(handle) = self
            .state
            .console_win
            .and_then(|key| self.windows.get(&key).copied())
        else {
            return;
        };
        let Some(view) = self
            .state
            .console_win
            .and_then(|key| self.views.get(&key).cloned())
        else {
            return;
        };
        let launcher = self.state.in_launcher();
        let desk = self.desk_bounds;
        // deferred: the crossing is often dispatched from inside this very
        // window's update (a click on Lock), where it can't be updated again
        cx.spawn(async move |desktop, cx| {
            // the frames the window reports, `None` while still full size
            let (heard, frames) = futures::channel::mpsc::unbounded();
            // a full-size window ignores (or the window manager overrides) a
            // new frame, so leave that state first
            let left = handle
                .update(cx, |_, window, cx| {
                    let bounds = window.window_bounds().get_bounds();
                    let left = if window.is_fullscreen() {
                        window.toggle_fullscreen();
                        WindowBounds::Fullscreen(bounds)
                    } else if window.is_maximized() {
                        // macOS "maximized" is only a size a new frame replaces
                        if !cfg!(target_os = "macos") {
                            window.zoom_window();
                        }
                        WindowBounds::Maximized(bounds)
                    } else {
                        return (WindowBounds::Windowed(bounds), None);
                    };
                    let observing = view
                        .update(cx, |_, cx| {
                            cx.observe_window_bounds(window, move |_, window, _| {
                                let frame = (!window.is_fullscreen() && !window.is_maximized())
                                    .then(|| window.bounds());
                                let _ = heard.unbounded_send(frame);
                            })
                        })
                        .ok();
                    (left, observing)
                })
                .ok();
            // held until the wait is over
            let (mut left, _observing) = left.unzip();
            let full = matches!(left, Some(WindowBounds::Fullscreen(_)))
                || matches!(left, Some(WindowBounds::Maximized(_))) && !cfg!(target_os = "macos");
            if full {
                // the window manager (or macOS's animation) puts back the
                // frame the window had before it went full size: wait for it,
                // and keep it as the frame the desk goes full size from again
                let restored = match left_full_size(frames, cx).await {
                    Some(restored) => Some(restored),
                    // it never said: take the window as it is
                    None => handle
                        .update(cx, |_, window, _| {
                            (!window.is_fullscreen() && !window.is_maximized())
                                .then(|| window.bounds())
                        })
                        .ok()
                        .flatten(),
                };
                if let Some(restored) = restored {
                    left = left.map(|left| match left {
                        WindowBounds::Fullscreen(_) => WindowBounds::Fullscreen(restored),
                        _ => WindowBounds::Maximized(restored),
                    });
                }
            }
            let _ = handle.update(cx, |_, window, cx| {
                let display = window.display(cx).map(|display| display.visible_bounds());
                let centred = |extent: gpui_kit::Size<gpui_kit::Pixels>| match display {
                    Some(display) => windows::centered(extent, display),
                    None => gpui_kit::Bounds::new(window.bounds().origin, extent),
                };
                let to = match (launcher, desk) {
                    (true, _) => centred(gpui_kit::size(
                        gpui_kit::px(launcher::LAUNCHER_SIZE.0),
                        gpui_kit::px(launcher::LAUNCHER_SIZE.1),
                    )),
                    (false, Some(desk)) => desk.get_bounds(),
                    (false, None) => centred(gpui_kit::size(
                        gpui_kit::px(windows::WINDOW_SIZE.0),
                        gpui_kit::px(windows::WINDOW_SIZE.1),
                    )),
                };
                window.set_bounds(to);
                match (launcher, desk) {
                    (false, Some(WindowBounds::Fullscreen(_))) => window.toggle_fullscreen(),
                    (false, Some(WindowBounds::Maximized(_))) if !cfg!(target_os = "macos") => {
                        window.zoom_window()
                    }
                    _ => {}
                }
            });
            if launcher {
                let _ = desktop.update(cx, |this, _| this.desk_bounds = left);
            }
        })
        .detach();
    }

    fn sync_appearance(&mut self, cx: &mut Context<Self>) {
        use gpui_kit::component::{Theme, ThemeMode};
        match self.state.appearance {
            crate::Appearance::Light => Theme::change(ThemeMode::Light, None, cx),
            crate::Appearance::Dark => Theme::change(ThemeMode::Dark, None, cx),
            crate::Appearance::System => Theme::sync_system_appearance(None, cx),
        }
        self.state.system_dark = Theme::global(cx).is_dark();
        configure_native_theme(cx);
    }

    fn start(&self, task: Task<Message>, cx: &mut Context<Self>) -> gpui_kit::Task<()> {
        let mut stream = task.into_stream();
        let runtime = crate::runtime::handle();
        cx.spawn(async move |desktop, cx| {
            loop {
                let message = futures::future::poll_fn(|context| {
                    let _runtime = runtime.enter();
                    stream.poll_next_unpin(context)
                })
                .await;
                let Some(message) = message else {
                    break;
                };
                if desktop
                    .update(cx, |this, cx| this.dispatch(message, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
    }

    fn subscriptions(&mut self, cx: &mut Context<Self>) {
        // the ticks run on the wall clock and wake from the kernel's
        // thread: under gpui's test scheduler a wake from another thread
        // is nondeterminism, and fails whichever test outlasts a tick
        if cfg!(test) {
            return;
        }
        let runtime = crate::runtime::handle();
        let _runtime = runtime.enter();
        let recipes = self.state.subscriptions().into_recipes();
        self.streams
            .retain(|key, _| recipes.iter().any(|recipe| recipe.key == *key));
        for recipe in recipes {
            if self.streams.contains_key(&recipe.key) {
                continue;
            }
            let stream = (recipe.start)();
            let task = self.start(Task::stream(stream), cx);
            self.streams.insert(recipe.key, task);
        }
    }

    fn execute(&mut self, command: Command, cx: &mut Context<Self>) {
        match command {
            Command::Open {
                key,
                kind,
                at,
                reply,
            } => self.open_window(key, kind, reply, at, cx),
            Command::Raise(key) => self.raise_window(key, cx),
            Command::Close(key) => {
                if let Some(handle) = self.windows.get(&key) {
                    remove(*handle, cx);
                }
            }
            Command::SwapConsole => self.swap_console(cx),
            Command::SyncAppearance => {
                self.sync_appearance(cx);
                cx.notify();
            }
            Command::OpenLink(link) => self.dispatch(Message::OpenLink(link), cx),
            Command::OpenUrl(url) => cx.open_url(&url),
            Command::Quit => self.quit(cx),
        }
    }

    fn raise_window(&mut self, key: WindowKey, cx: &mut Context<Self>) {
        if let Some(handle) = self.windows.get(&key) {
            let _ = handle.update(cx, |_, window, _| window.activate_window());
        }
    }

    fn quit(&mut self, cx: &mut Context<Self>) {
        let windows = self.windows.values().copied().collect::<Vec<_>>();
        cx.defer(move |cx| {
            for handle in windows {
                let _ = handle.update(cx, |_, window, cx| release_window_input(window, cx));
            }
            cx.quit();
        });
    }
}

fn release_window_input(window: &mut gpui_kit::Window, cx: &mut gpui_kit::App) {
    window.blur(cx);
    window.draw(cx).clear(cx);
}

/// Takes a window off the screen, its input let go first. Deferred:
/// releasing input draws the window, and the caller is most often in the
/// middle of updating it. `on_window_closed` (launch.rs) then forgets it.
fn remove(window: gpui_kit::AnyWindowHandle, cx: &mut gpui_kit::App) {
    cx.defer(move |cx| {
        let _ = window.update(cx, |_, window, cx| {
            release_window_input(window, cx);
            window.remove_window();
        });
    });
}

// ---------- the window ----------

pub(crate) struct DesktopWindow {
    model: Entity<Desktop>,
    key: WindowKey,
    kind: WindowKind,
    drag: Option<panes::Drag>,
    inputs: HashMap<&'static str, NativeInput>,
    /// ⌘K's field took focus when it opened; it is not taken again while
    /// Spotlight stays open.
    spotlight_focused: bool,
    /// Where the bar's menu buttons were last painted: each menu hangs
    /// under its own.
    bar_buttons: HashMap<crate::Overlay, gpui_kit::Bounds<gpui_kit::Pixels>>,
    /// The bar's program tabs: how far past their strip they ran.
    rail: gpui_kit::ScrollHandle,
    /// The window width the bar's full words need; narrower, it folds.
    bar_needs: f32,
    /// What `bar_needs` was measured over: the network, the tabs, who is
    /// signed in. Changed, the bar is measured again.
    bar_made: u64,
    /// The width the bar was last drawn at unfolded; `None` while folded.
    bar_drawn: Option<f32>,
    focus: gpui_kit::FocusHandle,
    _activation: gpui_kit::Subscription,
    _observer: gpui_kit::Subscription,
    _focus_lost: gpui_kit::Subscription,
}

struct NativeInput {
    state: Entity<gpui_kit::component::input::InputState>,
    /// A digest of the model text the field last agreed with — what it
    /// sent on its last change, or what the model last pushed into it.
    mirrored: std::rc::Rc<std::cell::Cell<u64>>,
    _subscription: gpui_kit::Subscription,
}

impl DesktopWindow {
    /// On the desk: connected, and past the key and account steps.
    fn on_desk(&self, cx: &gpui_kit::App) -> bool {
        !self.model.read(cx).state.in_launcher()
    }

    /// The desk's own keys reach its windows: the console, on the desk,
    /// with no overlay (Spotlight, a menu, Settings) keeping its keys.
    fn desk_keys(&self, cx: &gpui_kit::App) -> bool {
        self.kind == WindowKind::Console
            && self.on_desk(cx)
            && self.model.read(cx).state.overlay.is_none()
    }

    /// What ⌘W closes: the focused desk window, when the desk's keys reach
    /// it and it has one; `None` is the app's window (`close_by_key`).
    fn command_w_pane(&self, cx: &gpui_kit::App) -> Option<usize> {
        let layout = self.layout(cx);
        (self.desk_keys(cx) && !layout.panes.is_empty()).then_some(layout.focused)
    }

    /// ⌘W closes a window, never the app. A pop-out closes as its pane's ×
    /// does. The console closes too where the status item reopens it
    /// (macOS); elsewhere there is no tray to bring it back from, and the
    /// last window closing would quit — so it minimizes instead.
    fn close_by_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match (self.kind, cfg!(target_os = "macos")) {
            (WindowKind::Console, false) => window.minimize_window(),
            _ => remove(window.window_handle(), cx),
        }
    }

    /// The window's focused element vanished — most often because the
    /// screen it was on gave way to another (Connect → sign-in, sign-in →
    /// recovery phrase, phrase → console, a dialog opening or closing
    /// mid-form). Refocus the window's own root (the same handle a fresh
    /// window starts on) so a keyboard-only reader's next Tab still lands
    /// on the new screen's first control, instead of the window going
    /// silently blurred with no dispatch path for Tab, Enter or Escape to
    /// reach at all.
    fn focus_lost(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
    }

    fn released(&mut self, cx: &mut gpui_kit::App) {
        self.observe_window(view_wire::events::Window::Closed, cx);
    }

    /// This window's panes, as the model has them.
    fn layout(&self, cx: &gpui_kit::App) -> layout::Layout {
        self.model
            .read(cx)
            .state
            .layouts
            .get(&self.key)
            .cloned()
            .unwrap_or_default()
    }

    fn observe_window(&mut self, event: view_wire::events::Window, cx: &mut gpui_kit::App) {
        let panes = self.layout(cx).panes;
        let views: Vec<_> = {
            let mounted = &self.model.read(cx).mounted;
            panes
                .iter()
                .filter_map(|pane| mounted.get(&pane.instance))
                .map(|mounted| (mounted.module, mounted.view.clone()))
                .collect()
        };
        for (module, view) in views {
            let intents = view.update(cx, |view, cx| {
                view.observe_final_window_event(event.clone(), cx)
            });
            for intent in intents {
                self.model.update(cx, |model, cx| {
                    model.dispatch(Message::ViewEvent(module, intent), cx)
                });
            }
        }
    }
}

impl Render for DesktopWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui_kit::InteractiveElement as _;
        let content = match self.kind {
            WindowKind::View { .. } => self.console(window, cx),
            WindowKind::Console => {
                let state = self.model.read(cx).state.clone_facts();
                match self.model.read(cx).state.stage {
                    Stage::Connect => self.connect(window, cx),
                    Stage::Phrase(_) => self.phrase(&state, window, cx),
                    Stage::Unlock(_) => self.unlock(&state, window, cx),
                    Stage::Recover(_) => self.recover(&state, window, cx),
                    Stage::Account(_) => self.account_step(&state, window, cx),
                    Stage::Desk => self.console(window, cx),
                }
            }
        };
        let ink = ink::Ink::of(self.model.read(cx).state.dark());
        let mut root = gpui_kit::div();
        root.text_style().font_fallbacks = Some(fallback_chain());
        root.text_style().font_family = Some(theme::FAMILY_UI.into());
        let root = root.id("desktop-root");
        self.on_keys(root, cx)
            .size_full()
            .bg(ink.bg)
            .text_color(ink.ink)
            .track_focus(&self.focus)
            .on_modifiers_changed(cx.listener(
                |this, event: &gpui_kit::ModifiersChangedEvent, _, cx| {
                    this.model.update(cx, |model, cx| {
                        model.dispatch(Message::ModifierStateChanged(event.modifiers), cx)
                    });
                },
            ))
            .child(content)
    }
}

#[cfg(test)]
mod swap_tests {
    use super::*;
    use gpui_kit::{Bounds, point, px, size};

    #[gpui_kit::test]
    async fn a_window_leaving_full_size_is_taken_at_the_frame_it_settles_on(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        let frame = |x: f32| Bounds::new(point(px(x), px(0.)), size(px(800.), px(600.)));
        let (heard, frames) = futures::channel::mpsc::unbounded();
        let wait = cx.spawn(async move |cx| left_full_size(frames, &cx).await);
        // the state first, still at full size, then two frames
        for reported in [None, Some(frame(1.)), Some(frame(2.))] {
            heard.unbounded_send(reported).unwrap();
        }
        cx.run_until_parked();
        cx.executor().advance_clock(FRAME_SETTLES);
        assert_eq!(wait.await, Some(frame(2.)));

        // a window that never says: given up on, not waited for forever
        let (_heard, frames) = futures::channel::mpsc::unbounded();
        let wait = cx.spawn(async move |cx| left_full_size(frames, &cx).await);
        cx.run_until_parked();
        cx.executor().advance_clock(LEAVE_FULL_SIZE);
        assert_eq!(wait.await, None);
    }
}
