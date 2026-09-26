use super::*;
use crate::render::native_id;

#[derive(Clone, Copy)]
pub(super) enum ScrollRequest {
    Relative(f32, f32),
    Absolute(f32, f32),
    By(f32, f32),
    End,
    Key(u64),
}

#[derive(Clone)]
pub(super) struct VirtualRow {
    pub(super) key: String,
    pub(super) content: wire::Node,
    pub(super) gap: f32,
    pub(super) estimated_height: f32,
}

pub(super) struct VirtualScroll {
    pub(super) state: ListState,
    pub(super) rows: Vec<VirtualRow>,
    pub(super) anchor: wire::ScrollAnchor,
    pub(super) measured_width: Option<Pixels>,
}

/// A virtual column and its surrounding vertical chrome share one native
/// viewport. Keep the wire wrappers on each item, splitting only their outer
/// padding, so prefix controls never become part of the message-key sequence.
pub(super) fn virtual_rows(node: &wire::Node) -> Option<Vec<VirtualRow>> {
    use wire::Node;
    match node {
        Node::Container(view_wire::ContainerNode { children, .. }) if children.len() == 1 => {
            virtual_rows(&children[0]).map(|rows| wrap_virtual_rows(node, rows))
        }
        _ => None,
    }
}

pub(super) fn wrap_virtual_rows(node: &wire::Node, rows: Vec<VirtualRow>) -> Vec<VirtualRow> {
    let mut shell = node.clone();
    match &mut shell {
        wire::Node::Container(view_wire::ContainerNode { children, .. }) => children.clear(),
        _ => unreachable!("only vertical layout wrappers surround virtual rows"),
    }
    rows.into_iter()
        .map(|mut row| {
            let mut wrapped = shell.clone();
            let wire::Node::Container(view_wire::ContainerNode { children, .. }) = &mut wrapped
            else {
                unreachable!("vertical layout wrapper")
            };
            children.push(row.content);
            row.content = wrapped;
            row
        })
        .collect()
}

impl ViewTree {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn virtual_scroll(
        &mut self,
        path: &[wire::ElementIdWire],
        rows: Vec<VirtualRow>,
        anchor: wire::ScrollAnchor,
        follow: bool,
        handler: Option<u32>,
        style: &gpui_kit::StyleRefinement,
        restored: Option<ScrollPresentation>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let restored = restored
            .filter(|saved| {
                saved.rows.as_ref().is_some_and(|keys| {
                    keys.iter()
                        .map(String::as_str)
                        .eq(rows.iter().map(|row| row.key.as_str()))
                })
            })
            .map(|saved| saved.offset);
        let list = self
            .lists
            .entry(path.to_vec())
            .or_insert_with(|| VirtualScroll {
                state: ListState::new(
                    0,
                    if anchor == wire::ScrollAnchor::End {
                        ListAlignment::Bottom
                    } else {
                        ListAlignment::Top
                    },
                    px(160.),
                ),
                rows: Vec::new(),
                anchor,
                measured_width: None,
            });
        let prefix = list
            .rows
            .iter()
            .zip(&rows)
            .take_while(|(old, new)| old.key == new.key)
            .count();
        let suffix = list.rows[prefix..]
            .iter()
            .rev()
            .zip(rows[prefix..].iter().rev())
            .take_while(|(old, new)| old.key == new.key)
            .count();
        let old_end = list.rows.len() - suffix;
        let new_end = rows.len() - suffix;
        if prefix != old_end || prefix != new_end {
            if list.rows.is_empty() {
                let estimate = rows
                    .first()
                    .map_or(44., |row| row.estimated_height + row.gap);
                list.state
                    .reset_with_uniform_height(rows.len(), px(estimate));
            } else {
                list.state.splice(prefix..old_end, new_end - prefix);
            }
        }
        for (index, row) in rows.iter().enumerate() {
            let old = if index < prefix {
                list.rows.get(index)
            } else if index >= new_end {
                list.rows.get(old_end + index - new_end)
            } else {
                None
            };
            if old.is_some_and(|old| old.content != row.content || old.gap != row.gap) {
                list.state.remeasure_items(index..index + 1);
            }
        }
        list.rows = rows;
        list.anchor = anchor;
        list.state.set_follow_mode(if follow {
            FollowMode::Tail
        } else {
            FollowMode::Normal
        });
        let state = list.state.clone();
        let weak = cx.entity().downgrade();
        let route = path.to_vec();
        state.set_scroll_handler(move |_, _, cx| {
            let Some(handler) = handler else {
                return;
            };
            let weak = weak.clone();
            let route = route.clone();
            // List invokes this callback while borrowing its layout state.
            // Read pixel measurements after that borrow ends, on this turn.
            cx.defer(move |cx| {
                let _ = weak.update(cx, |this, cx| {
                    let Some(list) = this.lists.get(&route) else {
                        return;
                    };
                    let maximum = f32::from(list.state.max_offset_for_scrollbar().y);
                    let offset = f32::from(list.state.scroll_px_offset_for_scrollbar().y);
                    let y = match anchor {
                        wire::ScrollAnchor::End => maximum + offset,
                        _ => -offset,
                    };
                    cx.emit(wire::Event::ScrollOffset {
                        handler,
                        x: 0.,
                        y,
                        relative_x: 0.,
                        relative_y: y / maximum.max(1.),
                    });
                });
            });
        });
        let weak = cx.entity().downgrade();
        let route = path.to_vec();
        let native = gpui_kit::list(state, move |index, window, cx| {
            weak.update(cx, |this, cx| {
                let Some(row) = this
                    .lists
                    .get(&route)
                    .and_then(|list| list.rows.get(index))
                    .cloned()
                else {
                    return div().into_any_element();
                };
                div()
                    .relative()
                    .w_full()
                    .pb(px(row.gap))
                    .child(this.node(&row.content, window, cx))
                    .into_any_element()
            })
            .unwrap_or_else(|_| div().into_any_element())
        })
        .with_sizing_behavior(if style.size.height.is_some() {
            ListSizingBehavior::Auto
        } else {
            ListSizingBehavior::Infer
        })
        .w_full()
        .h_full();
        let route = path.to_vec();
        let weak = cx.entity().downgrade();
        let retain_estimates = canvas(
            move |bounds, _, cx| {
                let _ = weak.update(cx, |this, cx| {
                    let Some(list) = this.lists.get_mut(&route) else {
                        return;
                    };
                    if list.measured_width == Some(bounds.size.width) {
                        return;
                    }
                    list.measured_width = Some(bounds.size.width);
                    // GPUI invalidates every height hint on first prepaint and
                    // width changes. Restore the guest's estimate after that pass;
                    // measured rows keep their actual height via size_hint().
                    let estimate = list
                        .rows
                        .first()
                        .map_or(44., |row| row.estimated_height + row.gap);
                    list.state.clone().with_uniform_item_height(px(estimate));
                    if let Some(offset) = restored {
                        let maximum = list.state.max_offset_for_scrollbar();
                        list.state.set_offset_from_scrollbar(point(
                            px(0.),
                            offset.y.clamp(-maximum.y, px(0.)),
                        ));
                    }
                    cx.notify();
                });
            },
            |_, _, _, _| {},
        )
        .absolute()
        .inset_0();
        // The native list owns scrolling, including off-screen measurements;
        // the wire scroll remains the identity addressed by widget commands.
        div()
            .relative()
            .min_h_0()
            .refine_style(style)
            .id(native_id(path.last().unwrap()))
            .child(native)
            .child(retain_estimates)
            .child(self.measure(path, cx))
            .into_any_element()
    }

    pub(super) fn scroll(
        &mut self,
        node: &wire::Node,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let wire::Node::Scroll {
            content,
            style,
            direction,
            anchor_x,
            anchor_y,
            auto_scroll,
            on_scroll,
            bar_hidden,
            ..
        } = node
        else {
            unreachable!()
        };
        let path = self.authored_path.clone();
        let restored = self.presentation.scrolls.remove(&path).filter(|saved| {
            saved.direction == *direction && saved.anchors == (*anchor_x, *anchor_y)
        });
        if *direction == wire::ScrollDirection::Vertical
            && let Some(rows) = virtual_rows(content)
        {
            return self.virtual_scroll(
                &path,
                rows,
                *anchor_y,
                *auto_scroll,
                *on_scroll,
                style,
                restored,
                cx,
            );
        }
        let handle = self.scrolls.entry(path.clone()).or_default().clone();
        let element = div()
            .relative()
            .refine_style(style)
            .id(native_id(path.last().unwrap()))
            .track_scroll(&handle);
        let element = match direction {
            wire::ScrollDirection::Vertical => element.overflow_y_scroll(),
            wire::ScrollDirection::Horizontal => element.overflow_x_scroll(),
            wire::ScrollDirection::Both => element.overflow_scroll(),
        };
        let route = path.clone();
        let anchors = (*anchor_x, *anchor_y);
        let follow = *auto_scroll;
        let handler = *on_scroll;
        let weak = cx.entity().downgrade();
        let observe = canvas(
            |_, _, _| (),
            move |_, _, _, cx| {
                let maximum = handle.max_offset();
                let offset = handle.offset();
                let _ = weak.update(cx, |this, cx| {
                    let previous = this.scroll_positions.get(&route).copied();
                    let restored = restored
                        .as_ref()
                        .filter(|saved| previous.is_none() && saved.rows.is_none());
                    let mut next = offset;
                    for (position, maximum, previous, anchor) in [
                        (
                            &mut next.x,
                            maximum.x,
                            previous.map(|(offset, max)| (offset.x, max.x)),
                            anchors.0,
                        ),
                        (
                            &mut next.y,
                            maximum.y,
                            previous.map(|(offset, max)| (offset.y, max.y)),
                            anchors.1,
                        ),
                    ] {
                        let at_end = previous
                            .is_some_and(|(offset, max)| f32::from(offset + max).abs() < 2.0);
                        let initialize_end =
                            previous.is_none() && anchor == wire::ScrollAnchor::End;
                        if initialize_end || (follow && at_end) {
                            *position = -maximum;
                        } else if anchor == wire::ScrollAnchor::Keep
                            && let Some((offset, old_maximum)) = previous
                            && offset < px(0.0)
                        {
                            *position =
                                (*position - (maximum - old_maximum)).clamp(-maximum, px(0.0));
                        }
                    }
                    if let Some(saved) = restored {
                        next = point(
                            saved.offset.x.clamp(-maximum.x, px(0.)),
                            saved.offset.y.clamp(-maximum.y, px(0.)),
                        );
                    }
                    if next != offset {
                        handle.set_offset(next);
                        cx.notify();
                    }
                    let changed =
                        previous.is_none_or(|(offset, max)| offset != next || max != maximum);
                    this.scroll_positions.insert(route.clone(), (next, maximum));
                    if changed && let Some(handler) = handler {
                        let distance =
                            |offset: Pixels, maximum: Pixels, anchor: wire::ScrollAnchor| {
                                match anchor {
                                    wire::ScrollAnchor::End => f32::from(maximum + offset),
                                    _ => -f32::from(offset),
                                }
                            };
                        let x = distance(next.x, maximum.x, anchors.0);
                        let y = distance(next.y, maximum.y, anchors.1);
                        let relative_x = x / f32::from(maximum.x).max(1.0);
                        let relative_y = y / f32::from(maximum.y).max(1.0);
                        cx.emit(wire::Event::ScrollOffset {
                            handler,
                            x,
                            y,
                            relative_x,
                            relative_y,
                        });
                    }
                });
            },
        )
        .absolute()
        .inset_0();
        let content = element
            .child(self.node(content, window, cx))
            .child(observe)
            .child(self.measure(&path, cx));
        let handle = self.scrolls[&path].clone();
        // a vertical bar is the scroller's own child: absolute, it paints over
        // the scroller's bounds and leaves its layout (flex and all) untouched
        if *direction == wire::ScrollDirection::Vertical && !bar_hidden {
            return content
                .child(
                    Scrollbar::vertical(&handle)
                        .id("scrollbar")
                        .mode(ScrollbarMode::Always),
                )
                .into_any_element();
        }
        let scrollbar = match direction {
            wire::ScrollDirection::Vertical => None,
            wire::ScrollDirection::Horizontal => Some(Scrollbar::horizontal(&handle)),
            wire::ScrollDirection::Both => Some(Scrollbar::new(&handle)),
        }
        .filter(|_| !bar_hidden);
        let Some(scrollbar) = scrollbar else {
            return content.into_any_element();
        };
        let frame_style = gpui_kit::StyleRefinement {
            size: style.size.clone(),
            min_size: style.min_size.clone(),
            max_size: style.max_size.clone(),
            ..Default::default()
        };
        div()
            .relative()
            .refine_style(&frame_style)
            .child(content)
            .child(
                div().absolute().inset_0().child(
                    scrollbar
                        .id("scrollbar")
                        .viewport_from_layout()
                        .mode(ScrollbarMode::Always),
                ),
            )
            .into_any_element()
    }
}
