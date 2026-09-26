//! The bell's panel: the notification centre's rows.

use super::*;
use desk::BAR;
use screens::Facts;

impl DesktopWindow {
    /// The bell's panel (the NotifCenter board): 400 wide under the bell,
    /// kept inside the window; the rows, newest first, under Today and
    /// Earlier.
    pub(super) fn notifications(
        &self,
        state: &Facts,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        use super::ink::*;
        use gpui_kit::*;
        const WIDTH: f32 = 400.;
        let ink = Ink::of(state.dark);
        let viewport = window.viewport_size();
        let (wide, high) = (f32::from(viewport.width), f32::from(viewport.height));
        let width = WIDTH.min(wide - 16.).max(0.);
        let now = crate::runtime::notify::wall();
        let midnight = local_midnight(now);
        let center = crate::runtime::notify::center();
        let unread = center.unread();
        let entries: Vec<_> = center.entries().cloned().collect();
        drop(center);
        let small_link = |id: &'static str, text: &'static str, message: fn() -> Message| {
            let model = self.model.clone();
            let hover = ink.ink;
            crate::a11y::keyboard(
                sans(400, 13.)
                    .id(id)
                    .control(Role::Button, text)
                    .text_color(ink.muted)
                    .cursor_pointer()
                    .hover(move |style| style.text_color(hover))
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        model.update(cx, |model, cx| model.dispatch(message(), cx))
                    }),
            )
        };
        let header = div()
            .flex()
            .items_center()
            .gap(px(10.))
            .px(px(16.))
            .h(px(46.))
            .flex_shrink_0()
            .border_b_1()
            .border_color(ink.line)
            .child(sans(500, 14.).child("Notifications"))
            .child(
                mono(400, 12.)
                    .flex_1()
                    .text_color(ink.muted)
                    .child(format!("{unread} unread")),
            )
            .when(unread > 0, |header| {
                header.child(
                    small_link("notif-mark-all", "Mark all read", || {
                        Message::NotifyMarkAllRead
                    })
                    .child("Mark all read")
                    .text_color(ink.ink)
                    .underline(),
                )
            });
        let mut list = div()
            .id("notif-rows")
            .role(Role::Menu)
            .aria_label("Notifications")
            .flex()
            .flex_col()
            .max_h(px((high - BAR - 4. - 46. - 38. - 8.).max(80.)))
            .overflow_y_scroll();
        let mut section = None;
        for entry in &entries {
            let today = entry.at >= midnight;
            if section != Some(today) {
                section = Some(today);
                list = list.child(
                    mono(400, 12.)
                        .text_color(ink.muted)
                        .px(px(16.))
                        .pt(px(12.))
                        .pb(px(6.))
                        .border_b_1()
                        .border_color(ink.line)
                        .child(if today { "Today" } else { "Earlier" }),
                );
            }
            let model = self.model.clone();
            let id = entry.id;
            let surface = ink.surface;
            let initial: String = entry
                .title
                .chars()
                .next()
                .into_iter()
                .flat_map(char::to_uppercase)
                .collect();
            let source = match entry.tag.is_empty() {
                true => entry.module.clone(),
                false => format!("{} · {}", entry.module, entry.tag),
            };
            let said = format!(
                "{}{}: {}",
                if entry.read { "" } else { "Unread. " },
                entry.title,
                entry.body
            );
            let line = |text: String, color: Hsla| {
                sans(400, 13.)
                    .text_color(color)
                    .whitespace_nowrap()
                    .truncate()
                    .child(text)
            };
            list = list.child(crate::a11y::keyboard(
                div()
                    .id(SharedString::from(format!("notif/{id}")))
                    .control(Role::MenuItem, SharedString::from(said))
                    .flex()
                    .gap(px(10.))
                    .pl(px(8.))
                    .pr(px(16.))
                    .py(px(10.))
                    .border_b_1()
                    .border_color(ink.line)
                    .cursor_pointer()
                    .hover(move |style| style.bg(surface))
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        model.update(cx, |model, cx| model.dispatch(Message::NotifyOpen(id), cx))
                    })
                    .child(
                        div()
                            .w(px(8.))
                            .h(px(28.))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .when(!entry.read, |dot| {
                                dot.child(div().size(px(5.)).rounded_full().bg(ink.ink))
                            }),
                    )
                    .child(
                        sans(500, 12.)
                            .size(px(28.))
                            .flex_shrink_0()
                            .rounded_full()
                            .bg(ink.surface)
                            .text_color(ink.muted)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(initial),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.))
                                    .child(
                                        sans(if entry.read { 400 } else { 500 }, 14.)
                                            .min_w_0()
                                            .truncate()
                                            .text_color(match entry.read {
                                                true => ink.muted,
                                                false => ink.ink,
                                            })
                                            .child(entry.title.clone()),
                                    )
                                    .when(entry.count > 1, |row| {
                                        row.child(
                                            mono(400, 11.)
                                                .px(px(5.))
                                                .border_1()
                                                .border_color(ink.line)
                                                .text_color(ink.muted)
                                                .child(entry.count.to_string()),
                                        )
                                    })
                                    .child(div().flex_1())
                                    .child(
                                        mono(400, 12.)
                                            .flex_shrink_0()
                                            .text_color(ink.muted)
                                            .child(ago(entry.at, now)),
                                    ),
                            )
                            .when(!entry.body.is_empty(), |text| {
                                text.child(line(
                                    entry.body.replace('\n', " "),
                                    match entry.read {
                                        true => ink.muted,
                                        false => ink.figure,
                                    },
                                ))
                            })
                            .child(mono(400, 11.).text_color(ink.muted).child(source)),
                    ),
            ));
        }
        if entries.is_empty() {
            list = list.child(
                div()
                    .id("notif-empty")
                    .role(Role::Status)
                    .aria_label("You\u{2019}re all caught up")
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(6.))
                    .py(px(44.))
                    .child(sans(500, 14.).child("You\u{2019}re all caught up"))
                    .child(
                        sans(400, 13.)
                            .text_color(ink.muted)
                            .child("Mentions and messages land here."),
                    ),
            );
        }
        let footer = div()
            .flex()
            .items_center()
            .justify_between()
            .px(px(16.))
            .h(px(38.))
            .flex_shrink_0()
            .border_t_1()
            .border_color(ink.line)
            // only while something read is there to clear
            .when(entries.iter().any(|entry| entry.read), |footer| {
                footer.child(
                    small_link("notif-clear-read", "Clear read", || {
                        Message::NotifyClearRead
                    })
                    .child("Clear read"),
                )
            })
            .child(
                small_link("notif-settings", "Notification settings", || {
                    Message::NotifySettings
                })
                .ml_auto()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(
                    gpui_kit::component::Icon::new(gpui_kit::assets::IconName::Settings)
                        .size(px(13.)),
                )
                .child("Notification settings"),
            );
        let body = div()
            .flex()
            .flex_col()
            .child(header)
            .child(list)
            .child(footer);
        self.hanging(crate::Popover::Notifications, width, body, window, cx)
    }
}

/// Unix seconds at the start of the local day `wall` falls in.
fn local_midnight(wall: i64) -> i64 {
    let offset = local_offset(wall);
    (wall + offset).div_euclid(86_400) * 86_400 - offset
}

fn local_offset(wall: i64) -> i64 {
    let time = wall as libc::time_t;
    // SAFETY: `localtime_r` writes only the `tm` it is handed.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        match libc::localtime_r(&time, &mut tm).is_null() {
            true => 0,
            false => tm.tm_gmtoff as i64,
        }
    }
}

/// "now", "14m", "2h", "Yesterday", "3d".
fn ago(at: i64, now: i64) -> String {
    let seconds = (now - at).max(0);
    match seconds {
        ..60 => "now".into(),
        60..3_600 => format!("{}m", seconds / 60),
        _ if at >= local_midnight(now) => format!("{}h", seconds / 3_600),
        _ if at >= local_midnight(now) - 86_400 => "Yesterday".into(),
        _ => format!("{}d", seconds / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_read_short() {
        let now = local_midnight(1_790_121_600) + 12 * 3_600;
        assert_eq!(ago(now - 5, now), "now");
        assert_eq!(ago(now - 14 * 60, now), "14m");
        assert_eq!(ago(now - 2 * 3_600, now), "2h");
        assert_eq!(ago(now - 20 * 3_600, now), "Yesterday");
        assert_eq!(ago(now - 4 * 86_400, now), "4d");
    }
}
