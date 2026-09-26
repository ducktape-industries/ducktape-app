use super::*;
use crate::render::native_id;

impl ViewTree {
    pub(super) fn container(
        &mut self,
        node: &wire::Node,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let wire::Node::Container(view_wire::ContainerNode {
            id,
            style,
            interactivity,
            children,
        }) = node
        else {
            unreachable!()
        };
        let mut element = div();
        *element.style() = style.clone();
        crate::fonts::refine_fallbacks(element.style());
        let native_id = id.as_ref().map(native_id).unwrap_or_else(|| {
            let index = self.render_index;
            self.render_index += 1;
            ElementId::NamedInteger("guest-container".into(), index)
        });
        let mut element = element.id(native_id);
        if let Some(group) = &interactivity.group {
            element = element.group(group.clone());
        }
        if let Some(style) = &interactivity.hover {
            let style = style.clone();
            element = element.hover(move |_| style);
        }
        if let Some(style) = &interactivity.active {
            let style = style.clone();
            element = element.active(move |_| style);
        }
        if let Some(group) = &interactivity.group_hover {
            let style = group.style.clone();
            element = element.group_hover(group.group.clone(), move |_| style);
        }
        if let Some(group) = &interactivity.group_active {
            let style = group.style.clone();
            element = element.group_active(group.group.clone(), move |_| style);
        }
        if let Some(role) = interactivity.role {
            element = element.role(role);
        }
        if interactivity.focusable {
            element = element.focusable();
        }
        if let Some(value) = &interactivity.aria.author_id {
            element = element.accessibility_id(value.clone());
        }
        if let Some(value) = &interactivity.aria.label {
            element = element.aria_label(value.clone());
        } else if interactivity.role.is_some()
            && let Some(text) = descendant_text(node)
        {
            // A role with no explicit label: a view styling its own button
            // out of a container still gets a name, taken from the text it
            // drew inside — not left silent with its label one level down.
            element = element.aria_label(text);
        }
        if let Some(value) = &interactivity.aria.description {
            element = element.aria_description(value.clone());
        }
        if let Some(value) = &interactivity.aria.keyshortcuts {
            element = element.aria_keyshortcuts(value.clone());
        }
        if let Some(value) = &interactivity.aria.value {
            element = element.aria_value(value.clone());
        }
        if let Some(value) = &interactivity.aria.placeholder {
            element = element.aria_placeholder(value.clone());
        }
        if let Some(value) = interactivity.aria.selected {
            element = element.aria_selected(value);
        }
        if let Some(value) = interactivity.aria.expanded {
            element = element.aria_expanded(value);
        }
        if let Some(value) = interactivity.aria.disabled {
            element = element.aria_disabled(value);
        }
        if let Some(value) = interactivity.aria.numeric_value {
            element = element.aria_numeric_value(value);
        }
        if let Some(value) = interactivity.aria.numeric_value_step {
            element = element.aria_numeric_value_step(value);
        }
        if let Some(value) = interactivity.aria.min_numeric_value {
            element = element.aria_min_numeric_value(value);
        }
        if let Some(value) = interactivity.aria.max_numeric_value {
            element = element.aria_max_numeric_value(value);
        }
        if let Some(value) = interactivity.aria.level {
            element = element.aria_level(value);
        }
        if let Some(value) = interactivity.aria.position_in_set {
            element = element.aria_position_in_set(value);
        }
        if let Some(value) = interactivity.aria.size_of_set {
            element = element.aria_size_of_set(value);
        }
        if let Some(value) = interactivity.aria.row_index {
            element = element.aria_row_index(value);
        }
        if let Some(value) = interactivity.aria.column_index {
            element = element.aria_column_index(value);
        }
        if let Some(value) = interactivity.aria.row_count {
            element = element.aria_row_count(value);
        }
        if let Some(value) = interactivity.aria.column_count {
            element = element.aria_column_count(value);
        }
        if let Some(value) = interactivity.aria.toggled {
            element = element.aria_toggled(value);
        }
        if let Some(value) = interactivity.aria.orientation {
            element = element.aria_orientation(value);
        }
        if interactivity.aria.active_descendant {
            element = element.aria_active_descendant();
        }
        let focus_handle = interactivity.focus_handle.as_ref().map(|id| {
            self.guest_focus_targets
                .entry(*id)
                .or_insert_with(|| cx.focus_handle())
                .clone()
        });
        element = super::interactivity::apply(element, interactivity, focus_handle, cx);
        if let Some(handler) = interactivity.on_click {
            element = element.on_click(cx.listener(
                move |this, event: &gpui_kit::ClickEvent, _, cx| {
                    this.user_activation.set(Some(handler));
                    cx.emit(wire::Event::Click {
                        handler,
                        event: event.into(),
                    });
                },
            ));
        }
        if !self.authored_path.is_empty() {
            let path = self.authored_path.clone();
            let kind = std::mem::discriminant(node);
            let restore = self
                .presentation
                .focused_container
                .as_ref()
                .is_some_and(|(saved, saved_kind)| saved == &path && *saved_kind == kind);
            if restore {
                self.presentation.focused_container = None;
                let (_, handle) = self
                    .focus_targets
                    .entry(path.clone())
                    .or_insert_with(|| (kind, cx.focus_handle()));
                handle.focus(window, cx);
            }
            if let Some((_, handle)) = self.focus_targets.get(&path) {
                element = element.track_focus(handle);
            }
            element = element.child(self.measure(&path, cx));
        }
        for child in children {
            element = element.child(self.node(child, window, cx));
        }
        // a view's scroller shows its vertical bar: the bar is absolute, over
        // the scroller's bounds, and the handle keeps the offset across frames
        if style.overflow.y == Some(gpui_kit::Overflow::Scroll) && !self.authored_path.is_empty() {
            let handle = self
                .scrolls
                .entry(self.authored_path.clone())
                .or_default()
                .clone();
            element = element.track_scroll(&handle).child(
                gpui_kit::component::scroll::Scrollbar::vertical(&handle)
                    .id("scrollbar")
                    .mode(gpui_kit::component::scroll::ScrollbarMode::Always),
            );
        }
        #[cfg(test)]
        let element = {
            use gpui_kit::test::TestSupportExt as _;
            element.test_support()
        };
        element.into_any_element()
    }

    pub(super) fn responsive(
        &mut self,
        node: &wire::Node,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let wire::Node::Responsive { content, .. } = node else {
            unreachable!()
        };
        let weak = cx.entity().downgrade();
        let key = self.authored_path.clone();
        let measure = canvas(
            move |bounds, _, cx| {
                let size = [
                    f32::from(bounds.size.width) as f64,
                    f32::from(bounds.size.height) as f64,
                ];
                let _ = weak.update(cx, |this, cx| {
                    let changed = this.containers.get(&key) != Some(&size);
                    if changed {
                        this.containers.insert(key, size);
                        cx.notify();
                    }
                });
            },
            |_, _, _, _| {},
        )
        .absolute()
        .inset_0();
        div()
            .relative()
            .child(self.node(content, window, cx))
            .child(measure)
            .into_any_element()
    }

    pub(super) fn when(
        &mut self,
        node: &wire::Node,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let wire::Node::When {
            id,
            condition,
            children,
            ..
        } = node
        else {
            unreachable!()
        };
        let mut element = div().id(native_id(id)).flex().flex_col();
        if condition.matches(&self.containers) {
            for child in children {
                element = element.child(self.node(child, window, cx));
            }
        }
        element.into_any_element()
    }

    pub(super) fn anchored(
        &mut self,
        node: &wire::Node,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let wire::Node::Anchored {
            anchor,
            fit,
            position,
            position_mode,
            offset,
            children,
            ..
        } = node
        else {
            unreachable!()
        };
        let anchor = match anchor {
            wire::Anchor::TopLeft => gpui_kit::Anchor::TopLeft,
            wire::Anchor::TopRight => gpui_kit::Anchor::TopRight,
            wire::Anchor::BottomLeft => gpui_kit::Anchor::BottomLeft,
            wire::Anchor::BottomRight => gpui_kit::Anchor::BottomRight,
            wire::Anchor::TopCenter => gpui_kit::Anchor::TopCenter,
            wire::Anchor::BottomCenter => gpui_kit::Anchor::BottomCenter,
            wire::Anchor::LeftCenter => gpui_kit::Anchor::LeftCenter,
            wire::Anchor::RightCenter => gpui_kit::Anchor::RightCenter,
        };
        // every fit mode keeps the popup inside the slot: a guest cannot
        // see its pane's edges, so the host fits to them for it
        let margin = match fit {
            wire::AnchoredFitMode::SnapToWindowWithMargin([top, right, bottom, left]) => {
                gpui_kit::Edges {
                    top: px(*top),
                    right: px(*right),
                    bottom: px(*bottom),
                    left: px(*left),
                }
            }
            wire::AnchoredFitMode::SnapToWindow | wire::AnchoredFitMode::SwitchAnchor => {
                Default::default()
            }
        };
        super::anchored::Fitted {
            children: children
                .iter()
                .map(|child| self.node(child, window, cx))
                .collect(),
            anchor,
            position: position.map(|[x, y]| point(px(x), px(y))),
            local: *position_mode == wire::AnchoredPositionMode::Local,
            offset: offset.map_or_else(Point::default, |[x, y]| point(px(x), px(y))),
            margin,
        }
        .into_any_element()
    }

    pub(super) fn measure(
        &self,
        path: &[wire::ElementIdWire],
        cx: &Context<Self>,
    ) -> impl IntoElement + use<> {
        let route = path.to_vec();
        let weak = cx.entity().downgrade();
        canvas(
            move |bounds, _, cx| {
                let _ = weak.update(cx, |this, cx| {
                    let changed = this.bounds.get(&route) != Some(&bounds);
                    if changed {
                        this.bounds.insert(route, bounds);
                        cx.notify();
                    }
                });
            },
            |_, _, _, _| {},
        )
        .absolute()
        .inset_0()
    }
}
