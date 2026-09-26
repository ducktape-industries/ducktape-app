use super::*;

pub(super) fn walk_authored_paths(
    node: &wire::Node,
    path: &mut AuthoredPath,
    visit: &mut impl FnMut(&wire::Node, &AuthoredPath),
) {
    let entered_scope = crate::render::enter_scope(node, path);
    // Anonymous primitives (notably List) still own retained host state at
    // their current authored ancestry; they do not add a fabricated segment.
    visit(node, path);
    for child in node.children() {
        walk_authored_paths(child, path, visit);
    }
    if entered_scope {
        path.pop();
    }
}

/// Where focus enters a dialog: a node-less element drawn first in it,
/// tracking `entry`. The frame the dialog opens, focus that is still where
/// it was at the end of that frame moves to the first Tab stop after this
/// one — the dialog's first control — so a keyboard is in the dialog it
/// opened, not behind it. Focus the dialog's own content took is left alone.
pub(crate) fn dialog_entry(
    entry: &FocusHandle,
    opened: bool,
    window: &mut Window,
    cx: &mut App,
) -> Stateful<Div> {
    if opened {
        let before = window.focused(cx);
        let entry = entry.clone();
        window.defer(cx, move |window, cx| {
            if window.focused(cx) == before {
                window.focus(&entry, cx);
                window.focus_next(cx);
            }
        });
    }
    div().id("dialog-entry").track_focus(entry)
}

impl ViewTree {
    pub(crate) fn take_user_activation(&self, event: &wire::Event) -> Option<()> {
        let message = match event {
            wire::Event::Message(message)
            | wire::Event::Click {
                handler: message, ..
            }
            | wire::Event::AuxClick {
                handler: message, ..
            }
            | wire::Event::Select {
                handler: message, ..
            } => message,
            _ => return None,
        };
        let actual_click = self.user_activation.get() == Some(*message);
        if !actual_click {
            return None;
        }
        self.user_activation.take().map(|_| ())
    }

    #[cfg(test)]
    pub(crate) fn measured_bounds(&self, path: &[wire::ElementIdWire]) -> Option<Bounds<Pixels>> {
        self.bounds.get(path).copied()
    }

    pub fn set_editor_store(
        &mut self,
        store: crate::editor::wire::EditorStore,
        cx: &mut Context<Self>,
    ) {
        self.editors.clear();
        self.editor_store = Some(store);
        cx.notify();
    }

    pub fn execute_widget_command(
        &mut self,
        mut command: wire::WidgetCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Vec<u8>, String> {
        use wire::WidgetCommand as C;
        command.validate()?;
        match command {
            C::Focused { target } => Ok(wire::encode(&self.target_focused(&target, window, cx))),
            C::FocusHandle { handle } => {
                let focus = self
                    .guest_focus_targets
                    .get(&handle)
                    .ok_or_else(|| "focus handle is not mounted".to_string())?
                    .clone();
                focus.focus(window, cx);
                Ok(wire::encode(&()))
            }
            C::FocusPrevious => self.focus_relative(false, window, cx),
            C::FocusNext => self.focus_relative(true, window, cx),
            C::EditorAction { ref target, .. }
            | C::Focus { ref target }
            | C::CursorFront { ref target }
            | C::CursorEnd { ref target }
            | C::Cursor { ref target, .. }
            | C::SelectAll { ref target }
            | C::Select { ref target, .. } => self.input_command(target, &command, window, cx),
            C::Snap { target, x, y } => {
                self.scroll_command(&target, ScrollRequest::Relative(x, y), cx)
            }
            C::SnapEnd { target } => self.scroll_command(&target, ScrollRequest::End, cx),
            C::ScrollTo { target, x, y } => {
                self.scroll_command(&target, ScrollRequest::Absolute(x, y), cx)
            }
            C::ScrollBy { target, x, y } => {
                self.scroll_command(&target, ScrollRequest::By(x, y), cx)
            }
            C::ScrollToKey { target, key } => {
                self.scroll_command(&target, ScrollRequest::Key(key), cx)
            }
        }
    }

    /// The full authored path a widget command's target names, when one is
    /// mounted. A guest composes a target from context it already holds —
    /// an editor's own key (`"draft-general/editor"`), a list's own key —
    /// never the named ancestors above it, which live in other modules and
    /// other files entirely (the pane, the room and the composer's own
    /// wrapper each carry an id of their own between the tree's root and
    /// it). `self.mounted`, `self.editors` and the other target-keyed maps
    /// are indexed by the FULL walked ancestry (`walk_authored_paths`), so
    /// a short target is the SUFFIX of the path it names, not the whole of
    /// it — the same idiom `scroll_command` already uses to find a list row
    /// by its own trailing `"@row:"` key, extended to every target here.
    pub(super) fn resolve_target(&self, target: &[wire::ElementIdWire]) -> Option<AuthoredPath> {
        if target.is_empty() {
            return None;
        }
        let mut found = None;
        walk_authored_paths(&self.root, &mut Vec::new(), &mut |_, path| {
            if found.is_none() && path.ends_with(target) {
                found = Some(path.clone());
            }
        });
        found
    }

    pub(super) fn target_focused(
        &self,
        target: &[wire::ElementIdWire],
        window: &Window,
        cx: &App,
    ) -> bool {
        let Some(target) = self.resolve_target(target) else {
            return false;
        };
        let target = target.as_slice();
        if let Some((_, handle)) = self.focus_targets.get(target) {
            return handle.is_focused(window);
        }
        if let Some(field) = self.fields.get(target) {
            return field.state.read(cx).focus_handle(cx).is_focused(window);
        }
        if let Some(picker) = self.pickers.get(target) {
            return picker
                .state
                .read(cx)
                .focus_handle(cx)
                .contains_focused(window, cx);
        }
        self.editors
            .get(target)
            .is_some_and(|editor| editor.view.is_focused(window, cx))
    }

    pub(super) fn focus_relative(
        &mut self,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Vec<u8>, String> {
        let mut targets = Vec::new();
        walk_authored_paths(&self.root, &mut Vec::new(), &mut |_, path| {
            let available = self.mounted.contains(path)
                && (self.pickers.contains_key(path)
                    || self.focus_targets.contains_key(path)
                    || self.editors.contains_key(path));
            if available {
                targets.push(path.clone());
            }
        });
        if targets.is_empty() {
            return Ok(wire::encode(&()));
        }
        let current = targets
            .iter()
            .position(|key| self.target_focused(key, window, cx));
        let index = match (current, forward) {
            (Some(index), true) => (index + 1) % targets.len(),
            (Some(index), false) => (index + targets.len() - 1) % targets.len(),
            (None, true) => 0,
            (None, false) => targets.len() - 1,
        };
        let target = &targets[index];
        self.input_command(
            target,
            &wire::WidgetCommand::Focus {
                target: target.clone(),
            },
            window,
            cx,
        )
    }

    pub(super) fn input_command(
        &mut self,
        target: &[wire::ElementIdWire],
        command: &wire::WidgetCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Vec<u8>, String> {
        use wire::WidgetCommand as C;
        let Some(target) = self.resolve_target(target) else {
            return Ok(wire::encode(&()));
        };
        let target = target.as_slice();
        if matches!(command, C::Focus { .. }) {
            let mut kind = None;
            walk_authored_paths(&self.root, &mut Vec::new(), &mut |node, path| {
                if path == target
                    && matches!(node, wire::Node::Container(view_wire::ContainerNode { .. }))
                {
                    kind = Some(std::mem::discriminant(node));
                }
            });
            if let Some(kind) = kind {
                let (_, handle) = self
                    .focus_targets
                    .entry(target.to_vec())
                    .or_insert_with(|| (kind, cx.focus_handle()));
                handle.focus(window, cx);
                cx.notify();
                return Ok(wire::encode(&()));
            }
        }
        // A toolbar press names a tag, not an edit: it goes to the guest's
        // binding as an interaction on that field's document, whichever
        // editor draws it. Neither native editor may decide what a view's
        // tag means.
        if let C::EditorAction { tag, .. } = command {
            let editor_mounted = self.editors.contains_key(target);
            let Some(store) = editor_mounted
                .then_some(self.editor_store.as_ref())
                .flatten()
            else {
                return Err("editor action target is not a mounted editor".into());
            };
            store.act(target, tag.clone());
            return Ok(wire::encode(&()));
        }
        if let Some(editor) = self.editors.get(target) {
            editor.view.widget_command(command, window, cx);
            return Ok(wire::encode(&()));
        }
        if let Some(field) = self.fields.get(target) {
            if matches!(command, C::Focus { .. }) {
                field.state.update(cx, |field, cx| field.focus(window, cx));
            }
            return Ok(wire::encode(&()));
        }
        if let Some(picker) = self.pickers.get(target) {
            if matches!(command, C::Focus { .. }) {
                picker
                    .state
                    .update(cx, |picker, cx| picker.focus(window, cx));
            }
            return Ok(wire::encode(&()));
        }
        Ok(wire::encode(&()))
    }

    pub(super) fn scroll_command(
        &mut self,
        target: &[wire::ElementIdWire],
        request: ScrollRequest,
        cx: &mut Context<Self>,
    ) -> Result<Vec<u8>, String> {
        let Some(target) = self.resolve_target(target) else {
            return Ok(wire::encode(&()));
        };
        let target = target.as_slice();
        if let Some(list) = self.lists.get(target) {
            let maximum = list.state.max_offset_for_scrollbar().y;
            let offset = list.state.scroll_px_offset_for_scrollbar().y;
            let from_anchor = |value: f32| match list.anchor {
                wire::ScrollAnchor::End => px(value) - maximum,
                _ => -px(value),
            };
            match request {
                ScrollRequest::Key(key) => {
                    let suffix = format!("/@row:{key}");
                    if let Some(index) = list.rows.iter().position(|row| row.key.ends_with(&suffix))
                    {
                        list.state.scroll_to_reveal_item(index);
                    }
                }
                ScrollRequest::End => list.state.scroll_to_end(),
                ScrollRequest::Relative(_, y) => list
                    .state
                    .set_offset_from_scrollbar(point(px(0.), from_anchor(y * f32::from(maximum)))),
                ScrollRequest::Absolute(_, y) => list
                    .state
                    .set_offset_from_scrollbar(point(px(0.), from_anchor(y))),
                ScrollRequest::By(_, y) => {
                    let sign = if list.anchor == wire::ScrollAnchor::End {
                        1.
                    } else {
                        -1.
                    };
                    list.state
                        .set_offset_from_scrollbar(point(px(0.), offset + px(sign * y)));
                }
            }
            cx.notify();
            return Ok(wire::encode(&()));
        }
        let Some(handle) = self.scrolls.get(target) else {
            return Ok(wire::encode(&()));
        };
        let maximum = handle.max_offset();
        let mut anchors = (wire::ScrollAnchor::Start, wire::ScrollAnchor::Start);
        walk_authored_paths(&self.root, &mut Vec::new(), &mut |node, path| {
            let wire::Node::Scroll {
                anchor_x, anchor_y, ..
            } = node
            else {
                return;
            };
            if path != target {
                return;
            }
            anchors = (*anchor_x, *anchor_y);
        });
        let from_anchor = |distance: f32, maximum: Pixels, anchor: wire::ScrollAnchor| match anchor
        {
            wire::ScrollAnchor::End => px(distance) - maximum,
            wire::ScrollAnchor::Start | wire::ScrollAnchor::Keep => -px(distance),
        };
        let next = match request {
            ScrollRequest::Relative(x, y) => point(
                from_anchor(x * f32::from(maximum.x), maximum.x, anchors.0),
                from_anchor(y * f32::from(maximum.y), maximum.y, anchors.1),
            ),
            ScrollRequest::Absolute(x, y) => point(
                from_anchor(x, maximum.x, anchors.0),
                from_anchor(y, maximum.y, anchors.1),
            ),
            ScrollRequest::By(x, y) => {
                let direction = |delta: f32, anchor: wire::ScrollAnchor| match anchor {
                    wire::ScrollAnchor::End => px(delta),
                    _ => -px(delta),
                };
                handle.offset() + point(direction(x, anchors.0), direction(y, anchors.1))
            }
            ScrollRequest::End => -maximum,
            ScrollRequest::Key(_) => return Ok(wire::encode(&())),
        };
        handle.set_offset(point(
            next.x.clamp(-maximum.x, px(0.0)),
            next.y.clamp(-maximum.y, px(0.0)),
        ));
        cx.notify();
        Ok(wire::encode(&()))
    }

    pub fn replace(&mut self, mut root: wire::Node, cx: &mut Context<Self>) {
        let mut focusable = HashMap::new();
        let mut guest_focus_ids = std::collections::HashSet::new();
        let mut inputs = std::collections::HashSet::new();
        let mut scrolls = std::collections::HashSet::new();
        let mut uniform_lists = std::collections::HashSet::new();
        let mut variable_lists = std::collections::HashSet::new();
        let mut pickers = std::collections::HashSet::new();
        let mut drags = std::collections::HashSet::new();
        let mut dialogs = std::collections::HashSet::new();
        let mut containers = std::collections::HashSet::new();
        let mut retained = std::collections::HashSet::new();
        let mut sensors = std::collections::HashSet::new();
        let mut mounted = std::collections::HashSet::new();
        walk_authored_paths(&root, &mut Vec::new(), &mut |node, path| {
            mounted.insert(path.clone());
            // a scrolling container with an id keeps its handle, as a Scroll
            // node does; an id-less one has no path of its own to keep it at
            if let wire::Node::Container(view_wire::ContainerNode {
                id: Some(_), style, ..
            }) = node
                && style.overflow.y == Some(gpui_kit::Overflow::Scroll)
            {
                scrolls.insert(path.clone());
            }
            if matches!(
                node,
                wire::Node::Container(view_wire::ContainerNode { id: Some(_), .. })
            ) {
                focusable.insert(path.clone(), std::mem::discriminant(node));
            }
            match node {
                wire::Node::Container(view_wire::ContainerNode { interactivity, .. })
                | wire::Node::Image { interactivity, .. }
                | wire::Node::Svg { interactivity, .. } => {
                    if let Some(id) = interactivity.focus_handle {
                        guest_focus_ids.insert(id);
                    }
                }
                wire::Node::UniformList {
                    path,
                    interactivity,
                    ..
                } => {
                    if let Some(id) = interactivity.focus_handle {
                        guest_focus_ids.insert(id);
                    }
                    uniform_lists.insert(path.clone());
                }
                wire::Node::List { path, state, .. } => {
                    variable_lists.insert(VariableListKey {
                        path: path.clone(),
                        state: *state,
                    });
                }
                wire::Node::Input { .. } => {
                    inputs.insert(path.clone());
                }
                wire::Node::Scroll { .. } => {
                    scrolls.insert(path.clone());
                }
                wire::Node::PickList { .. } | wire::Node::ComboBox { .. } => {
                    pickers.insert(path.clone());
                }
                wire::Node::ResizeHandle { .. } => {
                    drags.insert(path.clone());
                }
                wire::Node::Overlay {
                    label, children, ..
                } if named_overlay(label, children) => {
                    dialogs.insert(path.clone());
                }
                wire::Node::Responsive { .. } => {
                    containers.insert(path.clone());
                }
                wire::Node::Sensor {
                    reset,
                    on_show,
                    on_resize,
                    on_hide,
                    ..
                } => {
                    sensors.insert(path.clone());
                    if let Some(sensor) = self.sensors.get_mut(path) {
                        if sensor.reset != *reset {
                            sensor.reset = reset.clone();
                            sensor.size = None;
                            sensor.pending = None;
                        }
                        // Delayed measurements resolve these current-frame
                        // routes even before the next native draw runs.
                        sensor.on_show = *on_show;
                        sensor.on_resize = *on_resize;
                        sensor.on_hide = *on_hide;
                    }
                }
                wire::Node::Slider { .. }
                | wire::Node::Surface { .. }
                | wire::Node::Editor { .. }
                | wire::Node::ImageViewer { .. } => {
                    retained.insert(path.clone());
                }
                _ => {}
            }
        });
        root.for_each_mut(&mut |node| match node {
            wire::Node::Image {
                hash,
                data: Some(data),
                ..
            } => {
                self.remember_image(*hash, data);
            }
            wire::Node::ImageViewer {
                hash,
                data: Some(data),
                ..
            } => {
                self.remember_image(*hash, data);
            }
            wire::Node::Svg {
                source:
                    wire::SvgSource::Data {
                        hash,
                        bytes: Some(bytes),
                    },
                ..
            } => {
                self.remember_vector(*hash, bytes);
            }
            _ => {}
        });
        self.bounds.retain(|key, _| mounted.contains(key));
        self.focus_targets
            .retain(|key, (kind, _)| focusable.get(key) == Some(kind));
        self.guest_focus_targets
            .retain(|id, _| guest_focus_ids.contains(id));
        self.fields.retain(|key, _| inputs.contains(key));
        self.scrolls.retain(|key, _| scrolls.contains(key));
        self.lists.retain(|key, _| scrolls.contains(key));
        self.uniform_lists.retain(|id, list| {
            list.rows.clear();
            uniform_lists.contains(id)
        });
        self.variable_lists.retain(|id, list| {
            list.rows.clear();
            variable_lists.contains(id)
        });
        self.scroll_positions.retain(|key, _| scrolls.contains(key));
        self.pickers.retain(|key, _| pickers.contains(key));
        self.drags.retain(|key, _| drags.contains(key));
        self.dialogs.retain(|key, _| dialogs.contains(key));
        self.containers.retain(|key, _| containers.contains(key));
        self.ranges.retain(|key, _| retained.contains(key));
        self.editors.retain(|key, _| retained.contains(key));
        self.viewers.retain(|key, _| retained.contains(key));
        // on_hide observes viewport exit while mounted, not destruction.
        // Removed nodes own old-frame IDs; emitting one now could activate
        // an unrelated route in the replacement frame's handler table.
        self.sensors.retain(|key, _| sensors.contains(key));
        self.root = root;
        cx.notify();
    }
}
