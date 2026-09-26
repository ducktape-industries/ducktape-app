use super::*;

#[gpui_kit::test]
fn primitive_canvas_paints_in_the_first_frame_and_after_a_move(cx: &mut gpui_kit::TestAppContext) {
    cx.update(gpui_kit::init);
    let command = wire::CanvasCommand::Draw {
        shape: wire::CanvasShape::Rectangle {
            position: [10., 10.],
            size: [40., 30.],
            radius: [4.; 4],
        },
        fill: Some(gpui_kit::rgb(0xff0000).into()),
        stroke: None,
        even_odd: false,
    };
    let mut canvas_style = div().w(px(100.)).h(px(80.));
    let node = wire::Node::Canvas {
        commands: vec![command.clone()],
        style: canvas_style.style().clone(),
    };
    let window = cx.open_window(size(px(100.), px(80.)), |_, _| ViewTree::new(node));
    let tree = window.root(cx).unwrap();
    cx.update_window(window.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        assert!(
            !window.painted_quads().is_empty(),
            "no asynchronous image load needed"
        );
        tree.update(cx, |tree, cx| {
            if let wire::Node::Canvas { commands, .. } = &mut tree.root
                && let wire::CanvasCommand::Draw {
                    shape: wire::CanvasShape::Rectangle { position, .. },
                    ..
                } = &mut commands[0]
            {
                *position = [40., 25.];
            }
            cx.notify();
        });
        window.draw(cx).clear(cx);
        assert!(
            !window.painted_quads().is_empty(),
            "moving keeps visible native geometry"
        );
    })
    .unwrap();
    let mut complex = vec![command];
    complex.push(wire::CanvasCommand::Pop);
    assert!(
        !native_canvas_commands(&complex),
        "transform stacks keep the complete SVG renderer"
    );
}

#[gpui_kit::test]
fn svg_uses_native_element_and_retains_data_without_resent_bytes(
    cx: &mut gpui_kit::TestAppContext,
) {
    cx.update(gpui_kit::init);
    let bytes = br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24"><path fill="#ff0000" d="M0 0h12v24H0z"/><path fill="#0000ff" d="M12 0h12v24H12z"/></svg>"##.to_vec();
    let node = wire::Node::Svg {
        id: Some(wire::ElementIdWire::Name("artwork".into())),
        source: wire::SvgSource::Data {
            hash: 42,
            bytes: Some(bytes),
        },
        transformation: wire::SvgTransformation {
            scale: [1., 1.],
            translate: [0., 0.],
            rotate: 0.,
        },
        label: None,
        style: Default::default(),
        interactivity: Default::default(),
    };
    let window = cx.open_window(size(px(80.), px(80.)), |_, _| ViewTree::new(node));
    let tree = window.root(cx).unwrap();
    let mut native = gpui_kit::VisualTestContext::from_window(window.into(), cx);
    native.update(|window, cx| window.render_frame(cx));
    native.run_until_parked();
    native.update(|window, cx| {
        assert!(tree.read(cx).vectors.contains_key(&42));
        tree.update(cx, |tree, cx| {
            let mut next = tree.root.clone();
            if let wire::Node::Svg {
                source: wire::SvgSource::Data { bytes, .. },
                ..
            } = &mut next
            {
                *bytes = None;
            }
            tree.replace(next, cx);
        });
        window.render_frame(cx);
        assert!(tree.read(cx).vectors.contains_key(&42));
    });
}

#[gpui_kit::test]
fn container_focus_is_native_and_handoff_never_reuses_retired_handles(
    cx: &mut gpui_kit::TestAppContext,
) {
    struct Host {
        tree: Entity<ViewTree>,
        keys: std::rc::Rc<std::cell::Cell<usize>>,
    }
    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let keys = self.keys.clone();
            div()
                .capture_key_down(move |_, _, cx| {
                    keys.set(keys.get() + 1);
                    cx.stop_propagation();
                })
                .child(self.tree.clone())
        }
    }
    cx.update(gpui_kit::init);
    let menu = || container("menu", [text("label", "A real menu, without an input")]);
    let keys = std::rc::Rc::new(std::cell::Cell::new(0));
    let window = cx.open_window(size(px(500.), px(300.)), |_, cx| Host {
        tree: cx.new(|_| ViewTree::new(menu())),
        keys: keys.clone(),
    });
    let host = window.root(cx).unwrap();
    let mut native = gpui_kit::VisualTestContext::from_window(window.into(), cx);
    native.update(|window, cx| window.render_frame(cx));
    let tree = host.read_with(&native, |host, _| host.tree.clone());
    native.update(|window, cx| {
        tree.update(cx, |tree, cx| {
            assert!(tree.fields.is_empty());
            let target = vec![named_id("menu")];
            tree.execute_widget_command(wire::WidgetCommand::Focus { target }, window, cx)
                .unwrap();
        })
    });
    native.update(|window, cx| window.render_frame(cx));
    let retired = native.update(|window, cx| {
        tree.read_with(cx, |tree, cx| {
            let target = vec![named_id("menu")];
            assert!(tree.target_focused(&target, window, cx));
            tree.focus_targets[&target].1.clone()
        })
    });
    native.update(|window, cx| {
        window.dispatch_keystroke(gpui_kit::Keystroke::parse("escape").unwrap(), cx)
    });
    assert_eq!(
        keys.get(),
        1,
        "focused menu participates in native key capture"
    );
    native.update(|window, cx| {
        let saved = tree.read_with(cx, |tree, cx| tree.presentation(window, cx));
        host.update(cx, |host, cx| {
            host.tree = cx.new(|_| ViewTree::new(menu()).with_presentation(saved));
            cx.notify();
        });
    });
    native.update(|window, cx| window.render_frame(cx));
    let replacement = host.read_with(&native, |host, _| host.tree.clone());
    native.update(|window, cx| {
        replacement.read_with(cx, |tree, cx| {
            assert!(tree.target_focused(&[named_id("menu")], window, cx));
            assert!(
                !retired.is_focused(window),
                "handoff uses a fresh native handle"
            );
        })
    });
    native.update(|window, cx| {
        window.dispatch_keystroke(gpui_kit::Keystroke::parse("escape").unwrap(), cx)
    });
    assert_eq!(keys.get(), 2);
    native.update(|window, cx| {
        replacement.update(cx, |tree, cx| {
            tree.replace(text("closed", "menu closed"), cx);
            assert!(tree.focus_targets.is_empty());
            assert!(!tree.target_focused(&[named_id("menu")], window, cx));
        })
    });
    native.update(|window, cx| window.render_frame(cx));
    native.update(|window, cx| {
        retired.focus(window, cx);
        window.dispatch_keystroke(gpui_kit::Keystroke::parse("escape").unwrap(), cx);
    });
    assert_eq!(
        keys.get(),
        2,
        "retired menu has no current native dispatch path"
    );
}

#[gpui_kit::test]
fn text_respects_parent_width_and_keeps_nowrap_inside_its_box(cx: &mut gpui_kit::TestAppContext) {
    cx.update(gpui_kit::init);
    let text = |key: &str, content: String, width, nowrap| {
        let mut element = div().min_w_0().max_w_full().text_size(px(14.));
        element.style().size.width = width;
        element = if nowrap {
            element.truncate().flex_shrink_0()
        } else {
            element.whitespace_normal()
        };
        wire::Node::Text(view_wire::TextNode {
            id: Some(named_id(key)),
            style: element.style().clone(),
            content,
            heading: None,
            live: None,
        })
    };
    let paragraph = text(
        "paragraph",
        "A long description with words that must wrap within the available parent width. "
            .repeat(8),
        None,
        false,
    );
    let row = axis_container(
        "row",
        wire::Axis::Row,
        [
            text("height", "17968".into(), None, true),
            text("hash", "0123456789abcdef".repeat(16), Some(fill()), false),
            text("count", "12 ops".into(), None, true),
        ],
    );
    let mut header = row.clone();
    if let wire::Node::Container(view_wire::ContainerNode { children, .. }) = &mut header {
        *children = vec![
            text("label", "Header".into(), None, true),
            wire::Node::Space {
                style: sized_style(Some(fill()), None),
            },
            text("actions", "New page".into(), Some(fixed(100.)), true),
        ];
    }
    let mut reference = row.clone();
    if let wire::Node::Container(view_wire::ContainerNode { children, .. }) = &mut reference {
        *children = vec![text("reference", "Header".into(), None, true)];
    }
    let mut root = axis_container(
        "column",
        wire::Axis::Column,
        [paragraph, row, reference, header],
    );
    if let wire::Node::Container(view_wire::ContainerNode { style, .. }) = &mut root {
        let mut root_style = div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .min_h_0()
            .gap(px(8.))
            .max_w(px(620.));
        *style = root_style.style().clone();
    }
    let root = wire::Node::Button {
        id: named_id("wrapping-parent"),
        role: None,
        selected: None,
        content: wire::ButtonContent::Child(Box::new(root)),
        label: None,
        checked: None,
        expanded: None,
        description: None,
        on_press: Some(1),
        style: div().w_full().style().clone(),
    };
    let window = cx.open_window(size(px(800.), px(500.)), |_, _| ViewTree::new(root));
    let tree = window.root(cx).unwrap();
    let mut native = gpui_kit::VisualTestContext::from_window(window.into(), cx);
    native.update(|window, cx| window.render_frame(cx));
    let button_bounds = native.update(|window, _| window.find("wrapping-parent").bounds());
    tree.read_with(&native, |tree, _| {
        let parent = named_id("wrapping-parent");
        let column = named_id("column");
        let row = named_id("row");
        let paragraph = tree
            .measured_bounds(&[parent.clone(), column.clone(), named_id("paragraph")])
            .unwrap();
        let hash = tree
            .measured_bounds(&[
                parent.clone(),
                column.clone(),
                row.clone(),
                named_id("hash"),
            ])
            .unwrap();
        assert!(
            button_bounds.bottom() >= hash.bottom(),
            "auto-height button must show every wrapped line"
        );
        let count = tree
            .measured_bounds(&[
                parent.clone(),
                column.clone(),
                row.clone(),
                named_id("count"),
            ])
            .unwrap();
        assert!(
            tree.measured_bounds(&[
                parent.clone(),
                column.clone(),
                row.clone(),
                named_id("height"),
            ])
            .unwrap()
            .right()
                <= hash.left()
        );
        assert_eq!(
            tree.measured_bounds(&[
                parent.clone(),
                column.clone(),
                row.clone(),
                named_id("label"),
            ])
            .unwrap()
            .size
            .width,
            tree.measured_bounds(&[
                parent.clone(),
                column.clone(),
                row.clone(),
                named_id("reference"),
            ])
            .unwrap()
            .size
            .width,
            "intrinsic labels cannot lose letters to a Fill spacer"
        );
        assert!(paragraph.size.width <= px(620.));
        assert!(
            paragraph.size.height > hash.size.height,
            "default wrapping creates multiple lines"
        );
        assert!(hash.right() <= count.left());
        assert!(
            hash.size.height
                > tree
                    .measured_bounds(&[parent, column, row, named_id("reference")])
                    .unwrap()
                    .size
                    .height,
            "WordOrGlyph must override a native Button's inherited nowrap"
        );
        assert!(count.right() <= paragraph.left() + px(620.));
    });
    let mut nowrap = div().truncate();
    assert_eq!(nowrap.style().overflow.x, Some(gpui_kit::Overflow::Hidden));
    assert!(
        nowrap.text_style().text_overflow.is_some(),
        "the native text shaper must truncate glyphs, not only the containing box"
    );
}

#[gpui_kit::test]
fn horizontal_overflow_scrollbar_reveals_offscreen_columns(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::InputEvent as _;
    cx.update(gpui_kit::init);
    let columns = sized(
        "columns-box",
        axis_container(
            "columns",
            wire::Axis::Row,
            (0..4).map(|index| {
                sized(
                    &format!("column-{index}"),
                    container(
                        &format!("column-content-{index}"),
                        [text(&format!("name-{index}"), "Folder")],
                    ),
                    Some(fixed(230.)),
                    Some(fill()),
                )
            }),
        ),
        Some(fixed(920.)),
        Some(fill()),
    );
    let mut columns = columns;
    if let wire::Node::Container(view_wire::ContainerNode { children, .. }) = &mut columns
        && let wire::Node::Container(view_wire::ContainerNode { style, .. }) = &mut children[0]
    {
        *style = div()
            .flex()
            .flex_row()
            .w_full()
            .min_w_0()
            .gap(px(0.))
            .style()
            .clone();
    }
    let root = wire::Node::Scroll {
        id: wire::ElementIdWire::Name("folders".into()),
        content: Box::new(columns),
        direction: wire::ScrollDirection::Horizontal,
        style: sized_style(Some(fill()), Some(fill())),
        on_scroll: None,
        virtual_rows: false,
        bar_hidden: false,
        bar_width: None,
        bar_margin: None,
        scroller_width: None,
        bar_spacing: None,
        anchor_x: Default::default(),
        anchor_y: Default::default(),
        auto_scroll: false,
    };
    let mut main = axis_container(
        "main-column",
        wire::Axis::Column,
        [
            sized(
                "header",
                container("header-content", [text("header-text", "Header")]),
                None,
                Some(fixed(24.)),
            ),
            root,
            sized(
                "footer",
                container("footer-content", [text("footer-text", "Footer")]),
                None,
                Some(fixed(24.)),
            ),
        ],
    );
    if let wire::Node::Container(view_wire::ContainerNode { style, .. }) = &mut main {
        *style = div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .min_h_0()
            .h_full()
            .gap(px(8.))
            .style()
            .clone();
    }
    let root = sized("main", main, Some(fill()), Some(fill()));
    let mut root = root;
    if let wire::Node::Container(view_wire::ContainerNode { children, .. }) = &mut root
        && let wire::Node::Container(view_wire::ContainerNode { style, .. }) = &mut children[0]
    {
        *style = div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .min_h_0()
            .h_full()
            .gap(px(0.))
            .style()
            .clone();
    }
    let window = cx.open_window(size(px(400.), px(200.)), |_, _| ViewTree::new(root));
    let tree = window.root(cx).unwrap();
    let mut native = gpui_kit::VisualTestContext::from_window(window.into(), cx);
    native.update(|window, cx| window.render_frame(cx));
    tree.read_with(&native, |tree, _| {
        let folders = vec![
            named_id("main"),
            named_id("main-column"),
            named_id("folders"),
        ];
        assert_eq!(tree.scrolls[&folders].max_offset().x, px(520.));
        assert_eq!(tree.scrolls[&folders].bounds().size.height, px(152.));
        assert_eq!(
            tree.measured_bounds(&[
                named_id("main"),
                named_id("main-column"),
                named_id("folders"),
                named_id("columns-box"),
                named_id("columns"),
                named_id("column-0"),
            ])
            .unwrap()
            .size
            .width,
            px(230.)
        );
    });
    native.update(|window, cx| window.render_frame(cx));
    let position = point(px(350.), px(168.));
    native.update(|window, cx| {
        window.dispatch_event(
            MouseDownEvent {
                position,
                button: MouseButton::Left,
                modifiers: Default::default(),
                click_count: 1,
                first_mouse: false,
            }
            .to_platform_input(),
            cx,
        );
        window.dispatch_event(
            MouseUpEvent {
                position,
                button: MouseButton::Left,
                modifiers: Default::default(),
                click_count: 1,
            }
            .to_platform_input(),
            cx,
        );
        window.render_frame(cx);
    });
    tree.read_with(&native, |tree, _| {
        let folders = vec![
            named_id("main"),
            named_id("main-column"),
            named_id("folders"),
        ];
        assert!(
            tree.scrolls[&folders].offset().x < px(-200.),
            "scrollbar track click reveals offscreen columns: bounds={:?}, offset={:?}",
            tree.scrolls[&folders].bounds(),
            tree.scrolls[&folders].offset()
        );
        assert_eq!(tree.scrolls[&folders].offset().y, px(0.));
    });
}

#[gpui_kit::test]
fn sensor_preserves_linear_fill_bounds(cx: &mut gpui_kit::TestAppContext) {
    cx.update(gpui_kit::init);
    let root = wire::Node::Sensor {
        id: named_id("viewport"),
        reset: None,
        on_show: Some(1),
        on_resize: None,
        on_hide: None,
        anticipate: None,
        delay: None,
        child: Box::new({
            let mut content = axis_container(
                "content",
                wire::Axis::Column,
                [wire::Node::Space {
                    style: sized_style(Some(fixed(20.)), Some(fixed(5.))),
                }],
            );
            if let wire::Node::Container(view_wire::ContainerNode { style, .. }) = &mut content {
                *style = div().flex().flex_col().w_full().h_full().style().clone();
            }
            content
        }),
        style: sized_style(Some(fill()), Some(fill())),
    };
    let window = cx.open_window(size(px(400.), px(300.)), |_, _| ViewTree::new(root));
    let tree = window.root(cx).unwrap();
    let mut native = gpui_kit::VisualTestContext::from_window(window.into(), cx);
    native.update(|window, cx| window.render_frame(cx));
    tree.read_with(&native, |tree, _| {
        assert_eq!(
            tree.sensors[&vec![named_id("viewport")]].size,
            Some(size(px(400.), px(300.)))
        );
    });
}

/// A scrolling container keeps its handle at its own id: the offset holds
/// across frames that insert a sibling before it, and goes with it. One
/// without an id keeps none — its path is its parent's, shared with any
/// sibling scroller there.
#[gpui_kit::test]
fn a_scrolling_container_keeps_its_handle_at_its_own_id(cx: &mut gpui_kit::TestAppContext) {
    cx.update(gpui_kit::init);
    let scroll_style = || {
        let mut style = div().flex().flex_col().style().clone();
        style.overflow.y = Some(gpui_kit::Overflow::Scroll);
        style.size.width = Some(fixed(300.));
        style.size.height = Some(fixed(200.));
        style.min_size.height = Some(fixed(200.));
        style
    };
    let rows = |tag: &str, n: usize| -> Vec<wire::Node> {
        (0..n)
            .map(|i| {
                sized(
                    &format!("{tag}-row-{i}"),
                    text(&format!("{tag}-t-{i}"), "x"),
                    Some(fixed(300.)),
                    Some(fixed(100.)),
                )
            })
            .collect()
    };
    let banner = || {
        sized(
            "banner",
            text("bt", "b"),
            Some(fixed(300.)),
            Some(fixed(50.)),
        )
    };
    let root = |with_banner: bool, n: usize| {
        container(
            "main",
            with_banner
                .then(banner)
                .into_iter()
                .chain([container_with_style("list", scroll_style(), rows("a", n))]),
        )
    };
    let window = cx.open_window(size(px(400.), px(600.)), |_, _| {
        ViewTree::new(root(false, 10))
    });
    let tree = window.root(cx).unwrap();
    let mut native = gpui_kit::VisualTestContext::from_window(window.into(), cx);
    native.update(|window, cx| window.render_frame(cx));
    let path = vec![named_id("main"), named_id("list")];
    tree.read_with(&native, |tree, _| {
        assert_eq!(tree.scrolls[&path].max_offset().y, px(800.));
        assert_eq!(
            tree.measured_bounds(&path).unwrap().size,
            size(px(300.), px(200.)),
            "the bar leaves the scroller's layout alone"
        );
        tree.scrolls[&path].set_offset(point(px(0.), px(-300.)));
    });
    native.update(|window, cx| window.render_frame(cx));
    tree.update(&mut native, |tree, cx| tree.replace(root(true, 12), cx));
    native.update(|window, cx| window.render_frame(cx));
    tree.read_with(&native, |tree, _| {
        assert_eq!(tree.scrolls[&path].offset().y, px(-300.));
        assert_eq!(tree.scrolls[&path].max_offset().y, px(1000.));
    });
    tree.update(&mut native, |tree, cx| {
        tree.replace(container("main", [banner()]), cx)
    });
    native.update(|window, cx| window.render_frame(cx));
    tree.read_with(&native, |tree, _| {
        assert!(
            tree.scrolls.is_empty(),
            "a removed scroller's handle is dropped"
        );
    });
    let anonymous = |tag: &str, n: usize| {
        wire::Node::Container(view_wire::ContainerNode {
            id: None,
            style: scroll_style(),
            interactivity: Default::default(),
            children: rows(tag, n),
        })
    };
    tree.update(&mut native, |tree, cx| {
        tree.replace(
            container("main", [anonymous("b", 10), anonymous("c", 20)]),
            cx,
        )
    });
    native.update(|window, cx| window.render_frame(cx));
    tree.read_with(&native, |tree, _| {
        assert!(tree.scrolls.is_empty(), "id-less scrollers share no handle");
    });
}
