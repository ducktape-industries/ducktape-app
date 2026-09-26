//! The plain multi-line editor: ONE native text field projecting one guest
//! document.
//!
//! An editor that asks for no block furniture is a text box, and a text box is
//! one field. Selecting across two lines, joining them with Backspace, walking
//! by word, dragging a selection through a paragraph and remembering the
//! caret's column are the editing engine's own work. A stack of one-line
//! fields can only imitate them one key at a time, and every key it has not
//! learned is an edit the writer cannot make.

use super::{EditorStore, Projection, key_state, offset, position};
use gpui_base::ScrollbarMode;
use gpui_base::StyledExt as _;
use gpui_kit::base::input::{Textarea, TextareaState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Bounds, Context, Edges, Element, ElementId, Entity,
    EntityInputHandler as _, EventEmitter, Focusable as _, GlobalElementId, InspectorElementId,
    InteractiveElement as _, IntoElement, Keystroke, LayoutId, MouseButton, ParentElement as _,
    Pixels, Render, Styled as _, Subscription, Window, div, px,
};
use std::ops::Range;
use std::sync::Arc;
use unicode_segmentation::UnicodeSegmentation;
use view_wire as wire;

/// The key context a guest editor sits in, which the app's own key bindings
/// read off the context stack (`shell::keys`).
pub const GUEST_EDITOR_CONTEXT: &str = "GuestEditor";

/// The tallest a field grows before it scrolls inside itself. The document
/// byte budget is the real limit; this is only the point past which growing
/// the element stops being how anyone reads it.
const MAX_ROWS: usize = 4096;

pub struct TextEditor {
    key: crate::render::AuthoredPath,
    store: EditorStore,
    input: Entity<TextareaState>,
    preview: Arc<str>,
    cursor: wire::EditorCursor,
    reset: Option<u64>,
    projection: Option<Projection>,
    painted: Option<wire::EditorOptions>,
    fills: bool,
    /// the most rows a field that does not fill grows to before it scrolls:
    /// what the node's own `max_h` holds
    cap: Option<usize>,
    /// the node's mapping; the field's text is added as its value
    accessible: crate::render::Accessible,
    ime: Option<crate::runtime::input::ImeState>,
    _observation: Subscription,
    _keystrokes: Subscription,
}

impl EventEmitter<()> for TextEditor {}

impl TextEditor {
    pub fn new(
        key: crate::render::AuthoredPath,
        store: EditorStore,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, MAX_ROWS)
                .soft_wrap(true)
                .searchable(false)
                .context_menu(false)
        });
        let observation = cx.observe_in(&input, window, |this, _, window, cx| {
            this.observed(window, cx);
        });
        // Native key bindings consume Enter/Tab/navigation before element key
        // listeners. Guest claims must run at GPUI's pre-action seam.
        let editor = cx.entity().downgrade();
        let keystrokes = cx.intercept_keystrokes(move |event, window, cx| {
            let _ = editor.update(cx, |editor, cx| {
                editor.key_down(&event.keystroke, window, cx);
            });
        });
        let mut this = Self {
            key,
            store,
            input,
            preview: Arc::from(""),
            cursor: Default::default(),
            reset: None,
            projection: None,
            painted: None,
            fills: true,
            cap: None,
            accessible: Default::default(),
            ime: None,
            _observation: observation,
            _keystrokes: keystrokes,
        };
        this.sync(window, cx);
        this
    }

    pub fn is_focused(&self, window: &Window, cx: &App) -> bool {
        self.input.read(cx).focus_handle(cx).is_focused(window)
    }

    /// Whether the field takes the box it was given or the room its own words
    /// need. Set from the node's height, because a field that always asked for
    /// all of its parent's height gave a shrinking box nothing to shrink to.
    pub fn set_fills(&mut self, fills: bool, cap: Option<usize>, cx: &mut Context<Self>) {
        if (self.fills, self.cap) == (fills, cap) {
            return;
        }
        self.fills = fills;
        self.cap = cap;
        let rows = cap.filter(|_| !fills).unwrap_or(MAX_ROWS);
        self.input
            .update(cx, |input, cx| input.set_auto_grow(1, rows, cx));
        cx.notify();
    }

    /// What assistive technology reads for the field, but its text.
    pub fn set_accessible(
        &mut self,
        accessible: crate::render::Accessible,
        cx: &mut Context<Self>,
    ) {
        if self.accessible != accessible {
            self.accessible = accessible;
            cx.notify();
        }
    }

    pub fn widget_command(
        &mut self,
        command: &wire::WidgetCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let at = |index: u32| {
            position(
                &self.preview,
                self.preview
                    .grapheme_indices(true)
                    .nth(index as usize)
                    .map_or(self.preview.len(), |(offset, _)| offset),
            )
        };
        let cursor = match command {
            wire::WidgetCommand::Focus { .. } => self.cursor,
            wire::WidgetCommand::CursorFront { .. } => wire::EditorCursor::default(),
            wire::WidgetCommand::CursorEnd { .. } => wire::EditorCursor {
                position: position(&self.preview, self.preview.len()),
                selection: None,
            },
            wire::WidgetCommand::Cursor {
                position: index, ..
            } => wire::EditorCursor {
                position: at(*index),
                selection: None,
            },
            wire::WidgetCommand::SelectAll { .. } => wire::EditorCursor {
                position: position(&self.preview, self.preview.len()),
                selection: Some(Default::default()),
            },
            wire::WidgetCommand::Select { start, end, .. } => wire::EditorCursor {
                position: at(*end),
                selection: (*start != *end).then(|| at(*start)),
            },
            _ => return false,
        };
        if cursor != self.cursor {
            self.move_cursor(cursor, cx);
            self.install(window, cx);
        }
        self.input.read(cx).focus_handle(cx).focus(window, cx);
        self.sync(window, cx);
        true
    }

    /// Carry the guest's accepted document into the field, and the field's
    /// options with it. While an edit is in flight the guest's copy is behind
    /// what was typed, so nothing is installed until the queue settles: a
    /// field that reverted to the last acknowledged text between keystrokes
    /// would eat every letter typed faster than a block.
    pub fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(projection) = self.store.projection(&self.key) else {
            return;
        };
        if self.projection.as_ref() == Some(&projection) {
            return;
        }
        let reset = self.reset != Some(projection.reference.reset);
        let settled = !projection.pending;
        let canonical = projection.text.clone().unwrap_or_else(|| Arc::from(""));
        let install = reset
            || (settled
                && (canonical != self.preview || projection.reference.cursor != self.cursor));
        if install {
            self.preview = canonical;
            self.cursor = projection.reference.cursor;
            self.reset = Some(projection.reference.reset);
            self.install(window, cx);
        }
        // A field the view just opened takes keys before its document is
        // here: the store holds them and replays them once it arrives.
        let editable = projection.editable && projection.fault.is_none();
        let input = self.input.clone();
        input.update(cx, |input, cx| {
            if input.is_editable() != editable {
                input.set_readonly(!editable, cx);
            }
            input.set_placeholder(projection.placeholder.clone(), window, cx);
            input.set_soft_wrap(true, window, cx);
            input.set_editor_paddings(Edges::all(px(0.)));
        });
        self.painted = Some(projection.options.clone());
        self.projection = Some(projection);
    }

    /// Put the document this mount holds into the field, text and caret both.
    /// ONLY the guest's own answer reaches here: a field rewritten on every
    /// frame would be rewritten between a keystroke and the observation of it,
    /// and the letter just typed would be the one it took back.
    fn install(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selection = selected_range(&self.preview, self.cursor);
        let text = self.preview.clone();
        self.input.clone().update(cx, |input, cx| {
            if input.value().as_ref() != text.as_ref() {
                input.set_value(text.to_string(), window, cx);
            }
            let standing = selection.start.min(selection.end)..selection.start.max(selection.end);
            let moved = input.selected_range() != standing || input.cursor() != selection.end;
            if moved {
                input.set_selected_range(selection, cx);
            }
        });
    }

    /// The field changed under the writer's hands: report the whole text and
    /// the caret that came with it. The editing engine has already decided
    /// what the keystroke meant, so there is one description of the edit here
    /// and it is the difference between two documents.
    fn observed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.is_focused(window, cx) {
            return;
        }
        let input = self.input.clone();
        let (text, marked, caret, selected) = input.update(cx, |input, cx| {
            (
                input.value().to_string(),
                input.marked_text_range(window, cx),
                input.cursor(),
                input.selected_range(),
            )
        });
        let events =
            crate::runtime::input::ime_events(&mut self.ime, &text, marked, caret, selected);
        if !events.is_empty() {
            self.store.observe_ime(events);
            cx.emit(());
        }
        // Preedit is observation only. The committed native edit follows the
        // ordinary guest transaction path exactly once after composition ends.
        if self.composing(window, cx) {
            return;
        }
        let state = input.read(cx);
        let text = state.value().to_string();
        let caret = state.cursor();
        let selected = state.selected_range();
        let next = cursor_at(&text, caret, selected);
        let same = text == self.preview.as_ref() && next == self.cursor;
        if same {
            return;
        }
        let kind = edit_kind(&self.preview, &text);
        self.store
            .native(&self.key, &self.preview, self.cursor, &text, next, kind);
        self.preview = Arc::from(text);
        self.cursor = next;
        cx.emit(());
        cx.notify();
    }

    fn composing(&self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.input.clone().update(cx, |input, cx| {
            input.marked_text_range(window, cx).is_some()
        })
    }

    /// The caret moved and nothing else: the guest still owns the cursor, so
    /// it hears about the move the same way it hears about a letter.
    fn move_cursor(&mut self, cursor: wire::EditorCursor, cx: &mut Context<Self>) {
        self.store.native(
            &self.key,
            &self.preview,
            self.cursor,
            &self.preview,
            cursor,
            wire::EditorEditKind::Cursor,
        );
        self.cursor = cursor;
        cx.emit(());
        cx.notify();
    }

    /// Only the chords the guest claimed are taken off the field. Everything
    /// else — every arrow, every Backspace, every selection — belongs to the
    /// editing engine, and taking one of those away is how an editor stops
    /// being one.
    fn key_down(&mut self, keystroke: &Keystroke, window: &mut Window, cx: &mut Context<Self>) {
        if !self.is_focused(window, cx) {
            return;
        }
        if self.composing(window, cx) {
            return;
        }
        let key = key_state(keystroke);
        let claimed = self
            .projection
            .as_ref()
            .and_then(|projection| projection.options.binding.as_ref())
            .is_some_and(|binding| {
                binding
                    .claims
                    .iter()
                    .any(|claim| claim.matches(&key, cfg!(target_os = "macos")))
            });
        if claimed {
            self.store.request(
                &self.key,
                wire::EditorRequestInput::Key { key, repeat: false },
            );
            cx.stop_propagation();
            cx.emit(());
            return;
        }
        self.tab(keystroke, window, cx);
    }

    /// Tab, which the writer means as an indent and the field would otherwise
    /// spend on leaving. The editing engine has an indent of its own and will
    /// not run it here — it is switched off for a field that grows with its
    /// text, which is every guest editor — so the keystroke walks on to the
    /// window's focus ring and the caret never sees it. Type the indent
    /// instead, exactly as if the two spaces had been pressed.
    fn tab(&mut self, keystroke: &Keystroke, window: &mut Window, cx: &mut Context<Self>) {
        let tab = keystroke.key == "tab";
        let plain = !keystroke.modifiers.control
            && !keystroke.modifiers.alt
            && !keystroke.modifiers.platform
            && !keystroke.modifiers.function;
        if !tab || !plain {
            return;
        }
        let outward = keystroke.modifiers.shift;
        let input = self.input.clone();
        let (text, selected, writable) = input.update(cx, |input, _| {
            (
                input.value().to_string(),
                input.selected_range(),
                input.is_editable(),
            )
        });
        if !writable {
            return;
        }
        // Shift+Tab against a line with no indent left to give has nothing to
        // do, and a key with nothing to do is the key that walks the focus
        // ring. Only an indent that actually moved is one this field keeps.
        let Some((next, moved)) = indent(&text, selected, outward) else {
            return;
        };
        input.update(cx, |input, cx| {
            input.set_value(next, window, cx);
            input.set_selected_range(moved, cx);
        });
        cx.stop_propagation();
    }

    /// A press in the box that the field itself did not take — the empty room
    /// under the last line — is still a press on the writing. It puts the
    /// caret at the end of the text, the way clicking under the words in any
    /// text box does, instead of landing nowhere.
    fn pressed(
        &mut self,
        event: &gpui_kit::MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let on_the_words = self.input.read(cx).input_bounds().contains(&event.position);
        if on_the_words {
            return;
        }
        let end = wire::EditorCursor {
            position: position(&self.preview, self.preview.len()),
            selection: None,
        };
        if end != self.cursor {
            self.move_cursor(end, cx);
            self.install(window, cx);
        }
        self.input.read(cx).focus_handle(cx).focus(window, cx);
    }
}

impl Render for TextEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync(window, cx);
        let options = self
            .projection
            .as_ref()
            .map(|projection| projection.options.clone())
            .unwrap_or_default();
        let presentation_style = options
            .presentation
            .as_ref()
            .map(|value| value.style.clone())
            .unwrap_or_default();
        let accessible = crate::render::Accessible {
            value: Some(self.input.read(cx).value().to_string()),
            ..self.accessible.clone()
        };
        // The app's bindings read this context off a keystroke: Ctrl+K is a
        // link here, not the search palette.
        div()
            .key_context(GUEST_EDITOR_CONTEXT)
            .relative()
            .w_full()
            .refine_style(&presentation_style)
            // The box the guest gave, not the room the words take: a press in
            // the empty part of a card is a press on the card's writing. A
            // field asked to shrink has no empty part to press — its box IS
            // its words — and taking the parent's height there would be taking
            // the height the parent is waiting on this field to report.
            .when(self.fills, |element| element.h_full())
            .on_mouse_down(MouseButton::Left, cx.listener(Self::pressed))
            .child(crate::render::announce(
                crate::a11y::text_field(
                    "editor-field",
                    &self.input.read(cx).focus_handle(cx),
                    {
                        let state = self.input.clone();
                        move |value, window, cx| {
                            state.update(cx, |state, cx| state.replace_all(value, window, cx))
                        }
                    },
                    // the base Textarea draws no node of its own
                    ShownBar(Textarea::new(&self.input).into_any_element()),
                )
                .when(self.fills, |field| field.h_full()),
                accessible,
            ))
    }
}

/// The field's own vertical bar, shown whenever its words run past it, as a
/// view's scroller shows its bar (`render::layout`): a capped composer with
/// no bar reads as a message cut short. The field draws the bar under the
/// theme's mode and takes none of its own, so the mode is `Always` while this
/// field, and only this field, lays out and paints.
struct ShownBar(AnyElement);

impl ShownBar {
    fn always<R>(cx: &mut App, f: impl FnOnce(&mut App) -> R) -> R {
        let theme = gpui_base::Theme::global_mut(cx);
        let before = theme.scrollbar.clone();
        theme.scrollbar = before.clone().with_mode(ScrollbarMode::Always);
        let out = f(cx);
        gpui_base::Theme::global_mut(cx).scrollbar = before;
        out
    }
}

impl IntoElement for ShownBar {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for ShownBar {
    type RequestLayoutState = ();
    type PrepaintState = ();
    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.0.request_layout(window, cx), ())
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        Self::always(cx, |cx| self.0.prepaint(window, cx));
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        Self::always(cx, |cx| self.0.paint(window, cx));
    }
}

/// One indent. Two spaces, the editing engine's own tab size: what an indent
/// has to do in prose is line the next line up under this one, and a hard tab
/// lines it up against a stop no painter here draws.
const INDENT: &str = "  ";

/// The document after Tab, and where the selection lands in it. `None` when
/// the key had nothing to do.
///
/// A caret types an indent where it stands. A SELECTION moves whole lines
/// instead — that is what makes Tab worth having in a list, and replacing the
/// selected words with two spaces is a deletion nobody asked for.
fn indent(text: &str, selected: Range<usize>, outward: bool) -> Option<(String, Range<usize>)> {
    let lo = selected.start.min(selected.end);
    let hi = selected.start.max(selected.end);
    let typing = lo == hi && !outward;
    if typing {
        let mut next = text.to_owned();
        next.insert_str(lo, INDENT);
        let at = lo + INDENT.len();
        return Some((next, at..at));
    }
    // The first line is the one the selection starts ON, wherever in it that
    // is; the last is the last one it starts BEFORE, so a selection carried to
    // the head of a line leaves that line alone, as it does everywhere else.
    let head = text[..lo].rfind('\n').map_or(0, |at| at + 1);
    let mut next = text[..head].to_owned();
    let mut edits: Vec<(usize, usize, usize)> = Vec::new();
    let mut at = head;
    for line in text[head..].split_inclusive('\n') {
        let touched = at == head || at < hi;
        if !touched {
            next.push_str(line);
            at += line.len();
            continue;
        }
        match outward {
            true => {
                let shed = outdent(line);
                edits.push((at, 0, shed));
                next.push_str(&line[shed..]);
            }
            false => {
                edits.push((at, INDENT.len(), 0));
                next.push_str(INDENT);
                next.push_str(line);
            }
        }
        at += line.len();
    }
    let nothing_to_shed = edits
        .iter()
        .all(|(_, added, removed)| *added + *removed == 0);
    if nothing_to_shed {
        return None;
    }
    let shifted = |offset: usize| {
        let mut moved = offset;
        for &(start, added, removed) in &edits {
            if start > offset {
                break;
            }
            moved += added;
            moved -= removed.min(offset - start);
        }
        moved
    };
    Some((next, shifted(selected.start)..shifted(selected.end)))
}

/// How much of a line's leading whitespace one Shift+Tab takes back: a hard
/// tab whole, or up to an indent's worth of spaces.
fn outdent(line: &str) -> usize {
    if line.starts_with('\t') {
        return 1;
    }
    line.bytes()
        .take(INDENT.len())
        .take_while(|byte| *byte == b' ')
        .count()
}

/// The field's selection for a guest cursor, ANCHOR first: the range runs
/// backwards when the caret is the earlier end, which is how the engine is
/// told which end a shift-arrow extends from.
fn selected_range(text: &str, cursor: wire::EditorCursor) -> Range<usize> {
    let caret = offset(text, cursor.position);
    let anchor = cursor.selection.map_or(caret, |at| offset(text, at));
    anchor..caret
}

/// The guest cursor for a field's caret and selection, read back the same way.
fn cursor_at(text: &str, caret: usize, selected: Range<usize>) -> wire::EditorCursor {
    let anchor = if caret == selected.start {
        selected.end
    } else {
        selected.start
    };
    wire::EditorCursor {
        position: position(text, caret),
        selection: (selected.start != selected.end).then(|| position(text, anchor)),
    }
}

fn edit_kind(before: &str, after: &str) -> wire::EditorEditKind {
    let unchanged = before == after;
    if unchanged {
        return wire::EditorEditKind::Cursor;
    }
    let shorter = after.len() < before.len();
    match shorter {
        true => wire::EditorEditKind::Backspace,
        false => wire::EditorEditKind::Insert,
    }
}

/// A store holding one ready document with the given claims. The mount reads
/// its projection exactly the way it reads a live guest's.
#[cfg(test)]
#[path = "text/tests.rs"]
mod tests;
