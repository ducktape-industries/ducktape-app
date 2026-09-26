use super::*;

#[cfg(test)]
fn editor_path() -> crate::render::AuthoredPath {
    vec![wire::ElementIdWire::Name("document".into())]
}

#[cfg(test)]
fn store_with(
    name: &str,
    text: &str,
    claims: Vec<wire::EditorKeyClaim>,
    placeholder: &str,
) -> EditorStore {
    let store = EditorStore::new(91);
    let reference = wire::editor_document::EditorDocumentRef {
        document: name.into(),
        reset: 1,
        revision: 0,
        text_revision: 0,
        byte_len: text.len() as u32,
        cursor: wire::EditorCursor {
            position: position(text, text.len()),
            selection: None,
        },
    };
    let mut locked = store.lock();
    locked.fields.insert(
        editor_path(),
        super::super::Field {
            reference: reference.clone(),
            handler: 1,
            editable: true,
            placeholder: placeholder.to_owned(),
            options: wire::EditorOptions {
                binding: Some(Box::new(wire::EditorBinding {
                    authored: true,
                    on_request: 2,
                    on_event: 3,
                    claims,
                })),
                ..Default::default()
            },
        },
    );
    locked.documents.insert(
        reference.document.clone(),
        super::super::Document {
            reference,
            text: Some(Arc::from(text)),
            queue: Default::default(),
            queued_bytes: 0,
            phase: super::super::Phase::Ready,
        },
    );
    drop(locked);
    store
}

/// Settle every queued edit against the document, the way a guest that accepts
/// what the field did would.
#[cfg(test)]
fn settle(store: &EditorStore, name: &str) {
    let mut locked = store.lock();
    while !locked.documents[name].queue.is_empty() {
        let accepted = locked.documents[name].reference.clone();
        locked.fields.get_mut(&editor_path()).unwrap().reference = accepted;
        locked.acknowledge();
        locked.pump();
        assert!(locked.fault.is_none(), "{:?}", locked.fault);
    }
}

/// The field carries the guest's placeholder, follows it when the guest
/// changes it, and takes what is typed into it.
#[cfg(test)]
#[gpui_kit::test]
fn an_empty_field_wears_the_guests_placeholder_and_takes_what_is_typed(
    cx: &mut gpui_kit::TestAppContext,
) {
    use gpui_kit::test::TestWindowExt as _;
    cx.update(gpui_kit::init);
    let store = store_with("empty", "", Vec::new(), "Start writing");
    let window = cx.open_window(gpui_kit::size(px(400.), px(200.)), |window, cx| {
        TextEditor::new(editor_path(), store.clone(), window, cx)
    });
    let editor = window.root(cx).unwrap();
    let mut native = gpui_kit::VisualTestContext::from_window(window.into(), cx);
    native.update(|window, cx| {
        window.render_frame(cx);
        let input = editor.read(cx).input.clone();
        assert!(input.read(cx).value().is_empty());
        assert_eq!(
            input.read(cx).presentation().placeholder().as_ref(),
            "Start writing"
        );
        input.read(cx).focus_handle(cx).focus(window, cx);
        window.render_frame(cx);
    });
    store
        .lock()
        .fields
        .get_mut(&editor_path())
        .unwrap()
        .placeholder = "새 문서".into();
    native.update(|window, cx| {
        editor.update(cx, |editor, cx| editor.sync(window, cx));
        window.render_frame(cx);
        assert_eq!(
            editor
                .read(cx)
                .input
                .read(cx)
                .presentation()
                .placeholder()
                .as_ref(),
            "새 문서"
        );
        window.input("Written text", cx);
    });
    native.run_until_parked();
    native.update(|window, cx| {
        window.render_frame(cx);
        let editor = editor.read(cx);
        assert_eq!(editor.input.read(cx).value().as_ref(), "Written text");
        assert_eq!(editor.preview.as_ref(), "Written text");
        assert!(store.lock().fault.is_none());
        window.blur(cx);
    });
}

/// A capped field over a long body paints its bar in `Always`, and the theme's
/// own mode is back when the frame is done: the rest of the app keeps its bars.
#[cfg(test)]
#[gpui_kit::test]
fn a_capped_field_shows_its_bar_and_leaves_the_theme_as_it_was(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::test::TestWindowExt as _;
    cx.update(gpui_kit::init);
    let body = vec!["a line of a long message"; 40].join("\n");
    let store = store_with("long", &body, Vec::new(), "");
    let window = cx.open_window(gpui_kit::size(px(400.), px(600.)), |window, cx| {
        TextEditor::new(editor_path(), store.clone(), window, cx)
    });
    let editor = window.root(cx).unwrap();
    let mut native = gpui_kit::VisualTestContext::from_window(window.into(), cx);
    native.update(|window, cx| {
        editor.update(cx, |editor, cx| editor.set_fills(false, Some(8), cx));
        let before = gpui_base::Theme::global(cx).scrollbar.mode();
        assert_ne!(before, ScrollbarMode::Always);
        window.render_frame(cx);
        window.render_frame(cx);
        assert_eq!(gpui_base::Theme::global(cx).scrollbar.mode(), before);
        assert_eq!(editor.read(cx).input.read(cx).value().as_ref(), body);
    });
}

/// Shift and an arrow reach past the line they started on. This is the whole
/// reason the document is one field: a selection that stops at the newline is
/// a selection that cannot take a paragraph.
#[cfg(test)]
#[gpui_kit::test]
fn shift_and_an_arrow_select_across_the_lines_of_one_document(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::test::TestWindowExt as _;
    cx.update(gpui_kit::init);
    let store = store_with("lines", "one\ntwo\nthree", Vec::new(), "");
    let window = cx.open_window(gpui_kit::size(px(400.), px(200.)), |window, cx| {
        TextEditor::new(editor_path(), store.clone(), window, cx)
    });
    let editor = window.root(cx).unwrap();
    let mut native = gpui_kit::VisualTestContext::from_window(window.into(), cx);
    native.update(|window, cx| {
        window.render_frame(cx);
        editor.update(cx, |editor, cx| {
            editor.input.read(cx).focus_handle(cx).focus(window, cx)
        });
        window.render_frame(cx);
        window.dispatch_keystroke(Keystroke::parse("shift-up").unwrap(), cx);
        window.dispatch_keystroke(Keystroke::parse("shift-up").unwrap(), cx);
    });
    native.run_until_parked();
    native.update(|window, cx| window.render_frame(cx));
    editor.read_with(&native, |editor, cx| {
        let selected = editor.input.read(cx).selected_range();
        assert!(
            editor.preview[selected.start..selected.end].contains('\n'),
            "a selection that took two shift-ups spans the newlines it crossed"
        );
        let anchor = editor
            .cursor
            .selection
            .expect("the selection reaches the guest");
        assert_ne!(
            anchor.line, editor.cursor.position.line,
            "the guest is told the selection crosses lines"
        );
    });
}

/// Backspace at the head of a line takes the newline before it and joins the
/// two lines — the ordinary way any text box works.
#[cfg(test)]
#[gpui_kit::test]
fn backspace_at_the_head_of_a_line_joins_it_to_the_one_above(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::test::TestWindowExt as _;
    cx.update(gpui_kit::init);
    let store = store_with("join", "one\ntwo", Vec::new(), "");
    let window = cx.open_window(gpui_kit::size(px(400.), px(200.)), |window, cx| {
        TextEditor::new(editor_path(), store.clone(), window, cx)
    });
    let editor = window.root(cx).unwrap();
    let mut native = gpui_kit::VisualTestContext::from_window(window.into(), cx);
    native.update(|window, cx| {
        window.render_frame(cx);
        editor.update(cx, |editor, cx| {
            editor.input.read(cx).focus_handle(cx).focus(window, cx)
        });
        window.render_frame(cx);
        // The caret starts at the end of "two"; Home puts it at the head.
        window.dispatch_keystroke(Keystroke::parse("home").unwrap(), cx);
        window.dispatch_keystroke(Keystroke::parse("backspace").unwrap(), cx);
    });
    native.run_until_parked();
    settle(&store, "join");
    native.update(|window, cx| window.render_frame(cx));
    editor.read_with(&native, |editor, _| {
        assert_eq!(editor.preview.as_ref(), "onetwo");
    });
    assert_eq!(
        store.lock().documents["join"].text.as_deref(),
        Some("onetwo"),
        "the joined document reaches the guest"
    );
}

/// A claimed chord is the guest's and never the field's; everything else is
/// the field's and never the guest's.
#[cfg(test)]
#[gpui_kit::test]
fn the_guest_hears_the_chords_it_claimed_and_no_others(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::test::TestWindowExt as _;
    cx.update(gpui_kit::init);
    let store = store_with(
        "claims",
        "text",
        vec![wire::EditorKeyClaim {
            key: wire::keyboard::Key::Character("z".into()),
            modifiers: Default::default(),
            command: true,
        }],
        "",
    );
    let window = cx.open_window(gpui_kit::size(px(400.), px(200.)), |window, cx| {
        TextEditor::new(editor_path(), store.clone(), window, cx)
    });
    let editor = window.root(cx).unwrap();
    let mut native = gpui_kit::VisualTestContext::from_window(window.into(), cx);
    native.update(|window, cx| {
        window.render_frame(cx);
        editor.update(cx, |editor, cx| {
            editor.input.read(cx).focus_handle(cx).focus(window, cx)
        });
        window.render_frame(cx);
    });
    store.drain();
    let undo = if cfg!(target_os = "macos") {
        "cmd-z"
    } else {
        "ctrl-z"
    };
    native.update(|window, cx| window.dispatch_keystroke(Keystroke::parse(undo).unwrap(), cx));
    let claimed = store.drain();
    assert!(
        claimed.iter().any(
            |event| matches!(event, wire::Event::EditorRequest { request, .. }
            if matches!(&request.input, wire::EditorRequestInput::Key { key, .. }
                if key.key == wire::keyboard::Key::Character("z".into())))
        ),
        "undo must reach guest history, not the field's own undo stack: {claimed:?}"
    );
    native.update(|window, cx| window.dispatch_keystroke(Keystroke::parse("left").unwrap(), cx));
    let unclaimed = store.drain();
    assert!(
        !unclaimed
            .iter()
            .any(|event| matches!(event, wire::Event::EditorRequest { .. })),
        "an arrow is the field's to answer: {unclaimed:?}"
    );
}

/// A field the guest will not let anyone write in reports nothing, and keeps
/// the text it was given.
#[cfg(test)]
#[gpui_kit::test]
fn a_readonly_field_reports_no_edit(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::test::TestWindowExt as _;
    cx.update(gpui_kit::init);
    let store = store_with("readonly", "Read only 한글", Vec::new(), "");
    store
        .lock()
        .fields
        .get_mut(&editor_path())
        .unwrap()
        .editable = false;
    let window = cx.open_window(gpui_kit::size(px(400.), px(200.)), |window, cx| {
        TextEditor::new(editor_path(), store.clone(), window, cx)
    });
    let editor = window.root(cx).unwrap();
    let mut native = gpui_kit::VisualTestContext::from_window(window.into(), cx);
    native.update(|window, cx| {
        window.render_frame(cx);
        editor.update(cx, |editor, cx| {
            editor.input.read(cx).focus_handle(cx).focus(window, cx)
        });
        window.render_frame(cx);
    });
    store.drain();
    native.update(|window, cx| {
        window.input("no", cx);
        window.dispatch_keystroke(Keystroke::parse("backspace").unwrap(), cx);
    });
    native.run_until_parked();
    editor.read_with(&native, |editor, cx| {
        assert_eq!(editor.preview.as_ref(), "Read only 한글");
        assert!(!editor.input.read(cx).is_editable());
    });
    assert_eq!(
        store.lock().documents["readonly"].text.as_deref(),
        Some("Read only 한글")
    );
}

/// A drag-selection is one caret move per pointer sample, against a queue that
/// drains one item per guest frame and faults the whole view when it fills.
/// Two caret moves in a row compose, so the queue keeps the one in flight and
/// one destination however far the pointer travels.
#[cfg(test)]
#[test]
fn a_drag_through_a_paragraph_does_not_fill_the_queue() {
    let text = "one two three four five six seven eight nine ten";
    let store = store_with("drag", text, Vec::new(), "");
    let reaching = |byte: usize| wire::EditorCursor {
        position: position(text, byte),
        selection: Some(position(text, 0)),
    };
    let mut held = wire::EditorCursor {
        position: position(text, 0),
        selection: None,
    };
    for byte in 1..text.len() {
        let next = reaching(byte);
        store.native(
            &editor_path(),
            text,
            held,
            text,
            next,
            wire::EditorEditKind::Cursor,
        );
        held = next;
    }
    let locked = store.lock();
    assert!(locked.fault.is_none(), "{:?}", locked.fault);
    let queue = &locked.documents["drag"].queue;
    assert!(
        queue.len() <= 2,
        "a drag of {} samples left {} in the queue",
        text.len() - 1,
        queue.len()
    );
}

/// Tab is an indent: typed where the caret stands, and carried across whole
/// lines when a selection covers them. Shift+Tab takes one back, and takes
/// nothing when there is nothing left to take — which is what leaves the key
/// to the focus ring.
#[cfg(test)]
#[test]
fn tab_indents_a_caret_a_block_and_gives_it_back() {
    let typed = indent("ab", 1..1, false).expect("an indent at the caret");
    assert_eq!(typed, ("a  b".to_owned(), 3..3));

    // Two lines selected from the middle of the first to the middle of the
    // second: both move, and both ends of the selection move with them.
    let block = indent("one\ntwo\nthree", 1..5, false).expect("a block indent");
    assert_eq!(block, ("  one\n  two\nthree".to_owned(), 3..9));

    // A selection carried to the head of the next line leaves that line where
    // it is — the writer stopped before it.
    let up_to = indent("one\ntwo", 0..4, false).expect("a block indent");
    assert_eq!(up_to.0, "  one\ntwo");

    let back = indent("  one\n  two", 3..9, true).expect("an outdent");
    assert_eq!(back, ("one\ntwo".to_owned(), 1..5));

    // A caret inside the indentation being taken back lands at the line's
    // head rather than running off it.
    let inside = indent("  one", 1..1, true).expect("an outdent");
    assert_eq!(inside, ("one".to_owned(), 0..0));

    assert_eq!(indent("one\ntwo", 0..7, true), None);
    assert_eq!(indent("\tone", 0..0, true), Some(("one".to_owned(), 0..0)));
}
