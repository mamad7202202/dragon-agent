//! All rendering: sidebar, top bar, chat stream, composer and the panes.

use crate::app::{DragonApp, Item, Tab};
use crate::theme::{self, Theme};
use dragon_core::agent::Mode;
use dragon_core::memory::graph::{Kind, Tier};
use dragon_core::presets;
use gpui::{
    AnyElement, ClickEvent, Context, Div, Entity, KeyDownEvent, SharedString, Stateful, Window,
    div, px, prelude::*, rgba,
};

// ------------------------------------------------------------------ helpers

fn v_flex() -> Div {
    div().flex().flex_col()
}

fn h_flex() -> Div {
    div().flex()
}

/// Strip light markdown so prose reads clean without a renderer.
pub fn md_lite(t: &str) -> String {
    t.replace("**", "").replace('`', "")
}

fn truncate_chars(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}

fn fmt_tok(n: u64) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

#[derive(Clone, Copy)]
enum BtnKind {
    /// solid ember
    Primary,
    /// quiet surface chip
    Ghost,
    /// destructive wash
    Danger,
    /// positive confirm
    Jade,
}

fn button(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    kind: BtnKind,
    small: bool,
    theme: &Theme,
    handler: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> Stateful<Div> {
    let label: SharedString = label.into();
    let (bg, fg, border) = match kind {
        BtnKind::Primary => (theme.ember, Some(gpui::rgb(0x201016)), None),
        BtnKind::Ghost => (theme.surface, Some(theme.muted), Some(theme.line)),
        BtnKind::Danger => (rgba(0xF0545422), Some(theme.blood), None),
        BtnKind::Jade => (theme.jade, Some(gpui::rgb(0x10241A)), None),
    };
    div()
        .id(id)
        .cursor_pointer()
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .px(if small { px(10.) } else { px(14.) })
        .h(if small { px(26.) } else { px(34.) })
        .text_size(if small { px(11.) } else { px(12.5) })
        .font_weight(gpui::FontWeight::MEDIUM)
        .bg(bg)
        .when_some(fg, |el, c| el.text_color(c))
        .when_some(border, |el, c| el.border_1().border_color(c))
        .hover(move |s| match kind {
            BtnKind::Ghost => s.bg(theme.elevated),
            _ => s.opacity(0.85),
        })
        .on_click(handler)
        .child(label)
}

fn switch(
    id: &'static str,
    on: bool,
    theme: &Theme,
    handler: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> Stateful<Div> {
    div()
        .id(id)
        .cursor_pointer()
        .w(px(42.))
        .h(px(23.))
        .flex_shrink_0()
        .rounded_full()
        .p(px(2.5))
        .flex()
        .items_center()
        .bg(if on { theme.ember } else { theme.line })
        .when(on, |el| el.justify_end())
        .hover(|s| s.opacity(0.9))
        .on_click(handler)
        .child(div().size(px(18.)).rounded_full().bg(gpui::rgb(0xFFFFFF)))
}

// --------------------------------------------------------------------- root

pub fn render_root(
    app: &mut DragonApp,
    _window: &mut Window,
    cx: &mut Context<DragonApp>,
) -> impl IntoElement {
    let theme = theme::theme(cx);

    div()
        .id("root")
        .size_full()
        .relative()
        .flex()
        .overflow_hidden()
        .bg(theme.bg)
        .text_color(theme.text)
        .text_size(px(13.))
        .track_focus(&app.focus)
        // ---- app-wide actions ------------------------------------------
        .on_action(cx.listener(|app, _: &crate::app::Cancel, window, cx| {
            if app.busy {
                app.stop(cx);
            } else if app.pending_approval() {
                app.answer(false, false, cx);
            } else {
                window.blur();
            }
        }))
        .on_action(cx.listener(|app, _: &crate::app::NewSessionAction, _window, cx| {
            app.new_session(cx);
        }))
        .on_action(cx.listener(|app, _: &crate::app::ToggleThemeAction, _window, cx| {
            app.toggle_theme(cx);
        }))
        .on_action(cx.listener(|app, _: &crate::app::OpenSettings, _window, cx| {
            app.set_tab(Tab::Settings, cx);
        }))
        .on_action(cx.listener(|app, _: &crate::app::CycleMode, _window, cx| {
            app.cycle_mode(cx);
        }))
        // y / a / n / d answer shortcuts while an approval card is up
        .on_key_down(cx.listener(|app, ev: &KeyDownEvent, window, cx| {
            if !app.pending_approval() || ev.keystroke.modifiers.modified() {
                return;
            }
            if app.typing_in_a_field(window, cx) {
                return;
            }
            match ev.keystroke.key.as_str() {
                "y" => app.answer(true, false, cx),
                "a" => app.answer(true, true, cx),
                "n" => app.answer(false, false, cx),
                "d" => app.explain_pending(cx),
                _ => {}
            }
        }))
        .child(
            h_flex()
                .size_full()
                .min_w_0()
                .child(render_sidebar(app, cx))
                .child(render_main(app, _window, cx)),
        )
        .children(app.toast.clone().map(|msg| render_toast(&theme, msg)))
}

fn render_toast(theme: &Theme, msg: SharedString) -> Div {
    div()
        .absolute()
        .left_0()
        .right_0()
        .bottom_4()
        .flex()
        .justify_center()
        .child(
            div()
                .px_4()
                .py_2()
                .rounded_full()
                .bg(theme.elevated)
                .border_1()
                .border_color(theme.gold)
                .text_color(theme.gold)
                .text_size(px(12.))
                .shadow_md()
                .child(msg),
        )
}

// ------------------------------------------------------------------ sidebar

fn render_sidebar(app: &mut DragonApp, cx: &mut Context<DragonApp>) -> Div {
    let theme = theme::theme(cx);

    let nav_rows = Tab::nav_items()
        .into_iter()
        .map(|(tab, label)| nav_row(app, tab, label, cx))
        .collect::<Vec<_>>();

    let new_session_click = cx.listener(|app, _, _, cx| app.new_session(cx));

    v_flex()
        .w(px(228.))
        .h_full()
        .flex_shrink_0()
        .bg(theme.panel)
        .border_r_1()
        .border_color(theme.line)
        .p_3()
        .gap_3()
        // brand
        .child(
            h_flex()
                .items_center()
                .gap_2()
                .px_1()
                .pt_1()
                .child(div().text_color(theme.ember).text_size(px(17.)).child("◆"))
                .child(
                    div()
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_size(px(14.))
                        .child("DRAGON"),
                )
                .child(
                    div()
                        .text_color(theme.muted)
                        .text_size(px(11.))
                        .child("AGENT"),
                ),
        )
        // new session
        .child(
            div()
                .id("new-session")
                .cursor_pointer()
                .flex()
                .items_center()
                .justify_center()
                .h(px(34.))
                .rounded_md()
                .bg(theme.ember)
                .text_color(gpui::rgb(0x201016))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_size(px(12.5))
                .hover(|s| s.opacity(0.88))
                .on_click(new_session_click)
                .child("+ new session"),
        )
        // navigation
        .child(v_flex().gap_0p5().children(nav_rows))
        // spacer
        .child(div().flex_1())
        // footer card
        .child(
            v_flex()
                .gap_1p5()
                .p_2()
                .rounded_md()
                .bg(theme.surface)
                .border_1()
                .border_color(theme.line)
                .child(
                    h_flex()
                        .items_center()
                        .gap_1p5()
                        .child(status_dot(app, cx))
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(theme.muted)
                                .truncate()
                                .child(app.model_spec.clone()),
                        ),
                )
                .child(
                    div()
                        .text_size(px(10.5))
                        .text_color(theme.faint)
                        .child(format!("↯ {} tokens", fmt_tok(app.usage_total))),
                )
                .child(
                    div()
                        .id("copy-session")
                        .cursor_pointer()
                        .text_size(px(10.))
                        .text_color(theme.faint)
                        .hover(|s| s.text_color(theme.muted))
                        .on_click(cx.listener(|app, _, _, cx| app.copy_session_id(cx)))
                        .child(format!("session {}", truncate_chars(&app.session_id, 10))),
                ),
        )
}

fn status_dot(app: &DragonApp, cx: &Context<DragonApp>) -> Div {
    let theme = theme::theme(cx);
    let color = if app.busy {
        theme.ember
    } else if app.agent_ok {
        theme.jade
    } else {
        theme.blood
    };
    div().size(px(7.)).rounded_full().bg(color)
}

fn nav_row(
    app: &mut DragonApp,
    tab: Tab,
    label: &'static str,
    cx: &mut Context<DragonApp>,
) -> Stateful<Div> {
    let theme = theme::theme(cx);
    let active = app.tab == tab;
    let id: &'static str = match tab {
        Tab::Chat => "nav-chat",
        Tab::Memory => "nav-memory",
        Tab::Providers => "nav-providers",
        Tab::Settings => "nav-settings",
        Tab::About => "nav-about",
    };
    let handler = cx.listener(move |app, _, _, cx| app.set_tab(tab, cx));
    div()
        .id(id)
        .cursor_pointer()
        .flex()
        .items_center()
        .gap_2()
        .px_2()
        .py_1p5()
        .rounded_md()
        .when(active, |el| el.bg(theme.elevated))
        .hover(move |s| if active { s } else { s.bg(theme.elevated) })
        .on_click(handler)
        .child(
            div()
                .size(px(6.))
                .rounded_full()
                .bg(if active { theme.ember } else { theme.line }),
        )
        .child(
            div()
                .text_size(px(12.5))
                .text_color(if active { theme.text } else { theme.muted })
                .font_weight(if active {
                    gpui::FontWeight::SEMIBOLD
                } else {
                    gpui::FontWeight::NORMAL
                })
                .child(label),
        )
}

// ---------------------------------------------------------------- main area

fn render_main(app: &mut DragonApp, window: &mut Window, cx: &mut Context<DragonApp>) -> Div {
    let pane: AnyElement = match app.tab {
        Tab::Chat => render_chat(app, window, cx).into_any_element(),
        Tab::Memory => render_memory(app, cx).into_any_element(),
        Tab::Providers => render_providers(app, window, cx).into_any_element(),
        Tab::Settings => render_settings(app, cx).into_any_element(),
        Tab::About => render_about(app, cx).into_any_element(),
    };
    v_flex()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .child(render_topbar(app, cx))
        .child(pane)
}

fn render_topbar(app: &mut DragonApp, cx: &mut Context<DragonApp>) -> Div {
    let theme = theme::theme(cx);

    // mode segmented control
    let pills: Vec<Stateful<Div>> = [Mode::Chat, Mode::Plan, Mode::Agent]
        .into_iter()
        .map(|m| {
            let active = app.mode == m;
            let id: &'static str = match m {
                Mode::Chat => "mode-chat",
                Mode::Plan => "mode-plan",
                Mode::Agent => "mode-agent",
            };
            let handler = cx.listener(move |app, _, _, cx| app.set_mode(m, cx));
            div()
                .id(id)
                .cursor_pointer()
                .px_3()
                .py_1()
                .rounded_full()
                .text_size(px(11.5))
                .font_weight(if active {
                    gpui::FontWeight::SEMIBOLD
                } else {
                    gpui::FontWeight::MEDIUM
                })
                .text_color(if active { gpui::rgb(0x201016) } else { theme.muted })
                .when(active, |el| el.bg(theme.ember))
                .hover(move |s| if active { s } else { s.bg(theme.elevated) })
                .on_click(handler)
                .child(m.as_str())
        })
        .collect();

    // right side cluster
    let dots = ".".repeat((app.tick % 3) as usize + 1);
    let status_label = if app.busy {
        format!("working{}", dots)
    } else if app.agent_ok {
        "ready".to_string()
    } else {
        "needs setup".to_string()
    };
    let update_chip = app.update_latest.clone().map(|latest| {
        let handler = cx.listener(|app, _, _, cx| app.open_update_page(cx));
        div()
            .id("update-chip")
            .cursor_pointer()
            .px_2p5()
            .py_1()
            .rounded_full()
            .border_1()
            .border_color(theme.gold)
            .text_color(theme.gold)
            .text_size(px(11.))
            .hover(|s| s.bg(theme.elevated))
            .on_click(handler)
            .child(format!("⭡ {} available", latest))
    });
    let theme_btn = cx.listener(|app, _, _, cx| app.toggle_theme(cx));

    h_flex()
        .w_full()
        .h(px(50.))
        .px_4()
        .flex_shrink_0()
        .items_center()
        .justify_between()
        .border_b_1()
        .border_color(theme.line)
        .child(
            h_flex()
                .items_center()
                .p(px(3.))
                .rounded_full()
                .bg(theme.surface)
                .border_1()
                .border_color(theme.line)
                .children(pills),
        )
        .child(
            h_flex()
                .items_center()
                .gap_2()
                .children(update_chip)
                .child(
                    div()
                        .id("theme-btn")
                        .cursor_pointer()
                        .size(px(28.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_md()
                        .text_size(px(13.))
                        .text_color(theme.muted)
                        .hover(|s| s.bg(theme.elevated))
                        .on_click(theme_btn)
                        .child(if theme.is_dark() { "☀" } else { "☾" }),
                )
                .child(
                    h_flex()
                        .items_center()
                        .gap_1p5()
                        .child(status_dot(app, cx))
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(theme.muted)
                                .child(status_label),
                        ),
                ),
        )
}

// --------------------------------------------------------------------- chat

fn render_chat(app: &mut DragonApp, window: &mut Window, cx: &mut Context<DragonApp>) -> Div {
    let theme = theme::theme(cx);
    let empty = app.row_count() == 0;
    let composer_focused = app.composer.read(cx).focus_handle.is_focused(window);

    let list = gpui::list(
        app.list.clone(),
        cx.processor(
            |app: &mut DragonApp,
             ix: usize,
             window: &mut Window,
             cx: &mut Context<DragonApp>| {
                message_row(app, ix, window, cx).into_any_element()
            },
        ),
    )
    .flex_1()
    .min_h_0()
    .w_full();

    let enter_handler = cx.listener(|app, ev: &KeyDownEvent, _window, cx| {
        if ev.keystroke.key == "enter" && !ev.keystroke.modifiers.modified() {
            app.send_message(cx);
        }
    });
    let send_click = cx.listener(|app, _, _, cx| app.send_message(cx));
    let stop_click = cx.listener(|app, _, _, cx| app.stop(cx));

    let draft_empty = app.composer.read(cx).text().trim().is_empty();
    let composer = app.composer.clone();
    let composer_click = {
        let composer = composer.clone();
        move |_: &gpui::MouseDownEvent, window: &mut Window, cx: &mut gpui::App| {
            window.focus(&composer.read(cx).focus_handle);
        }
    };
    let busy = app.busy;

    v_flex()
        .flex_1()
        .min_h_0()
        .bg(theme.bg)
        .child(
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .child(list)
                .when(empty, |el| {
                    el.child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                v_flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_size(px(44.))
                                            .text_color(theme.ember)
                                            .opacity(0.5)
                                            .child("◆"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(13.))
                                            .text_color(theme.faint)
                                            .child("wake the dragon — say something"),
                                    ),
                            ),
                    )
                }),
        )
        // ------------------------------------------------------- composer
        .child(
            v_flex()
                .flex_shrink_0()
                .p_3()
                .pt_2()
                .gap_1p5()
                .border_t_1()
                .border_color(theme.line)
                .bg(theme.panel)
                .child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .h(px(42.))
                                .px_2()
                                .flex()
                                .items_center()
                                .rounded_lg()
                                .bg(theme.surface)
                                .border_1()
                                .border_color(if composer_focused {
                                    theme.ember
                                } else {
                                    theme.line
                                })
                                .on_mouse_down(gpui::MouseButton::Left, composer_click)
                                .on_key_down(enter_handler)
                                .child(composer),
                        )
                        .child(if busy {
                            button("stop", "■ stop", BtnKind::Danger, false, &theme, stop_click)
                                .h(px(42.))
                                .into_any_element()
                        } else {
                            button(
                                "send",
                                "send ▸",
                                if draft_empty {
                                    BtnKind::Ghost
                                } else {
                                    BtnKind::Primary
                                },
                                false,
                                &theme,
                                send_click,
                            )
                            .h(px(42.))
                            .into_any_element()
                        }),
                )
                .child(
                    div()
                        .text_size(px(10.5))
                        .text_color(theme.faint)
                        .child("enter sends · esc stops or dismisses · ctrl/cmd+n new session"),
                ),
        )
}

fn message_row(
    app: &DragonApp,
    ix: usize,
    _window: &mut Window,
    cx: &mut Context<DragonApp>,
) -> Div {
    let theme = theme::theme(cx);
    let items_len = app.items.len();
    let cap = px(760.);
    let gap = px(12.);

    if ix < items_len {
        match &app.items[ix] {
            Item::User(text) => h_flex()
                .w_full()
                .mb(gap)
                .justify_end()
                .child(
                    div()
                        .max_w(cap * 0.72)
                        .bg(theme.user_bubble())
                        .rounded_lg()
                        .px_3p5()
                        .py_2p5()
                        .child(
                            div()
                                .text_size(px(12.8))
                                .line_height(px(19.))
                                .child(md_lite(text)),
                        ),
                ),
            Item::Assistant(text) => v_flex()
                .w_full()
                .max_w(cap)
                .mb(gap)
                .gap_1()
                .child(
                    div()
                        .text_size(px(10.))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(theme.ember_text)
                        .child("DRAGON"),
                )
                .child(
                    div()
                        .bg(theme.surface)
                        .border_1()
                        .border_color(theme.line)
                        .rounded_lg()
                        .px_4()
                        .py_3()
                        .child(
                            div()
                                .text_size(px(13.))
                                .line_height(px(20.))
                                .child(md_lite(text)),
                        ),
                ),
            Item::Tool(name, detail) => h_flex()
                .max_w(cap)
                .mb(px(6.))
                .items_start()
                .gap_2()
                .px_1()
                .py(px(2.))
                .child(
                    div()
                        .text_size(px(11.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme.violet)
                        .flex_shrink_0()
                        .child(format!("» {name}")),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(theme.muted)
                        .truncate()
                        .child(truncate_chars(detail, 140)),
                ),
            Item::Approval(_id, tool, detail) => {
                let allow = cx.listener(|app, _, _, cx| app.answer(true, false, cx));
                let always_label = format!("always {}", truncate_chars(tool, 16));
                let always = cx.listener(|app, _, _, cx| app.answer(true, true, cx));
                let deny = cx.listener(|app, _, _, cx| app.answer(false, false, cx));
                let why = cx.listener(|app, _, _, cx| app.explain_pending(cx));
                v_flex()
                    .w_full()
                    .max_w(cap)
                    .mb(gap)
                    .gap_2()
                    .p_3()
                    .rounded_lg()
                    .bg(rgba(0xFFCD700D))
                    .border_1()
                    .border_color(theme.gold)
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(theme.gold)
                            .child("⚠ PERMISSION REQUESTED"),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .flex_shrink_0()
                                    .child(tool.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(theme.muted)
                                    .truncate()
                                    .child(truncate_chars(detail, 110)),
                            ),
                    )
                    .child(
                        h_flex()
                            .flex_wrap()
                            .gap_2()
                            .child(button(
                                gpui::ElementId::named_usize("ap-allow", ix),
                                "allow",
                                BtnKind::Jade,
                                true,
                                &theme,
                                allow,
                            ))
                            .child(button(
                                gpui::ElementId::named_usize("ap-always", ix),
                                always_label,
                                BtnKind::Ghost,
                                true,
                                &theme,
                                always,
                            ))
                            .child(button(
                                gpui::ElementId::named_usize("ap-deny", ix),
                                "deny",
                                BtnKind::Danger,
                                true,
                                &theme,
                                deny,
                            ))
                            .child(button(
                                gpui::ElementId::named_usize("ap-why", ix),
                                "what does this do?",
                                BtnKind::Ghost,
                                true,
                                &theme,
                                why,
                            )),
                    )
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(theme.faint)
                            .child("y allow · a always · n deny · d explain"),
                    )
            }
            Item::System(text) => h_flex()
                .w_full()
                .my(px(6.))
                .justify_center()
                .child(
                    div()
                        .px_3()
                        .py_1p5()
                        .rounded_full()
                        .bg(theme.surface)
                        .text_color(theme.muted)
                        .text_size(px(11.5))
                        .child(md_lite(text)),
                ),
            Item::Tasks(rows) => {
                let mut rows_el: Vec<AnyElement> = Vec::new();
                for (text, done) in rows {
                    rows_el.push(
                        h_flex()
                            .items_start()
                            .gap_2()
                            .child(if *done {
                                div()
                                    .mt(px(2.))
                                    .size(px(14.))
                                    .rounded_sm()
                                    .bg(theme.jade)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_size(px(10.))
                                    .text_color(gpui::rgb(0x10241A))
                                    .child("✓")
                            } else {
                                div()
                                    .mt(px(2.))
                                    .size(px(14.))
                                    .rounded_sm()
                                    .border_1()
                                    .border_color(theme.line)
                            })
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .line_height(px(17.))
                                    .text_color(if *done { theme.muted } else { theme.text })
                                    .child(text.clone()),
                            )
                            .into_any_element(),
                    );
                }
                v_flex()
                    .w_full()
                    .max_w(cap)
                    .mb(gap)
                    .gap_1p5()
                    .p_3()
                    .rounded_lg()
                    .bg(theme.surface)
                    .border_1()
                    .border_color(theme.line)
                    .child(
                        div()
                            .text_size(px(10.))
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(theme.gold)
                            .child("TASK BOARD"),
                    )
                    .children(rows_el)
            }
        }
    } else {
        // streaming bubble — the extra virtual row beyond `items`
        let text = md_lite(&app.streaming_text());
        v_flex()
            .w_full()
            .max_w(cap)
            .gap_1()
            .child(
                div()
                    .text_size(px(10.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(theme.ember_text)
                    .child("DRAGON"),
            )
            .child(
                div()
                    .bg(theme.surface)
                    .border_1()
                    .border_color(theme.line)
                    .rounded_lg()
                    .px_4()
                    .py_3()
                    .child(
                        div()
                            .text_size(px(13.))
                            .line_height(px(20.))
                            .child(format!("{text}▌")),
                    ),
            )
    }
}

// ------------------------------------------------------------------- memory

fn pane_header(title: &str, subtitle: String, theme: &Theme) -> Div {
    v_flex().gap_1().child(
        div()
            .text_size(px(17.))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .child(title.to_string()),
    ).child(
        div()
            .text_size(px(11.5))
            .text_color(theme.muted)
            .child(subtitle),
    )
}

fn render_memory(app: &mut DragonApp, cx: &mut Context<DragonApp>) -> Stateful<Div> {
    let theme = theme::theme(cx);
    let engine = app.cfg.settings.memory_engine.clone();
    let snapshot = app
        .graph
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .snapshot(Some(&app.session_id));

    let mut sections: Vec<AnyElement> = Vec::new();
    for (label, bullets) in snapshot {
        let mut bullet_els: Vec<AnyElement> = Vec::new();
        for (b, _mine) in bullets {
            let tag = match b.kind {
                Kind::Decision => ("!", theme.ember),
                Kind::Lesson => ("L", theme.violet),
                Kind::Task => ("~", theme.gold),
                Kind::Context => ("?", theme.sky),
                Kind::Fact => ("·", theme.muted),
            };
            let color = match b.tier() {
                Tier::Active => theme.text,
                Tier::Cooling => theme.muted,
                Tier::Archival => theme.faint,
            };
            bullet_els.push(
                h_flex()
                    .items_start()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(tag.1)
                            .w(px(12.))
                            .flex_shrink_0()
                            .child(tag.0.to_string()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(12.5))
                            .line_height(px(18.))
                            .text_color(color)
                            .child(b.text.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(theme.faint)
                            .child(format!("{:.0}%", b.confidence * 100.0)),
                    )
                    .into_any_element(),
            );
        }
        sections.push(
            v_flex()
                .gap_2()
                .p_4()
                .rounded_lg()
                .bg(theme.surface)
                .border_1()
                .border_color(theme.line)
                .child(
                    div()
                        .text_size(px(10.5))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(theme.gold)
                        .child(label.to_uppercase()),
                )
                .children(bullet_els)
                .into_any_element(),
        );
    }

    let mut pane = v_flex()
        .id("memory-pane")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .p_6()
        .gap_4()
        .child(pane_header("Memory graph", format!("engine: {engine} · confidence decays with disuse; archival fades away"), &theme));

    if sections.is_empty() {
        pane = pane.child(
            div()
                .text_size(px(12.5))
                .text_color(theme.faint)
                .child("(empty — the agent maintains it via graph_set_section as it works)"),
        );
    } else {
        pane = pane.children(sections);
    }
    pane
}

// ---------------------------------------------------------------- providers

fn field_shell(field: Entity<TextField>, focused: bool, theme: &Theme) -> Div {
    let focus_on_click = {
        let field = field.clone();
        move |_: &gpui::MouseDownEvent, window: &mut Window, cx: &mut gpui::App| {
            window.focus(&field.read(cx).focus_handle);
        }
    };
    div()
        .w_full()
        .h(px(38.))
        .px_2()
        .flex()
        .items_center()
        .rounded_md()
        .bg(theme.surface)
        .border_1()
        .border_color(if focused { theme.ember } else { theme.line })
        .on_mouse_down(gpui::MouseButton::Left, focus_on_click)
        .child(field)
}

fn render_providers(app: &mut DragonApp, window: &mut Window, cx: &mut Context<DragonApp>) -> Stateful<Div> {
    let theme = theme::theme(cx);

    let mut cards: Vec<AnyElement> = Vec::new();
    for (card_ix, p) in app.cfg.providers.clone().into_iter().enumerate() {
        let is_default = app
            .cfg
            .default_model
            .as_deref()
            .map(|d| d.starts_with(&format!("{}/", p.name)))
            .unwrap_or(false);
        let name_for_delete = p.name.clone();
        let delete_click =
            cx.listener(move |app, _, _, cx| app.delete_provider(name_for_delete.clone(), cx));
        let models = p
            .models
            .iter()
            .take(2)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        cards.push(
            v_flex()
                .gap_1p5()
                .p_4()
                .rounded_lg()
                .bg(theme.surface)
                .border_1()
                .border_color(theme.line)
                .child(
                    h_flex()
                        .items_center()
                        .justify_between()
                        .child(
                            h_flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .text_size(px(13.5))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child(p.name.clone()),
                                )
                                .when(is_default, |el| {
                                    el.child(
                                        div()
                                            .px_2()
                                            .py(px(1.5))
                                            .rounded_full()
                                            .text_size(px(9.5))
                                            .font_weight(gpui::FontWeight::BOLD)
                                            .text_color(theme.gold)
                                            .border_1()
                                            .border_color(theme.gold)
                                            .child("DEFAULT"),
                                    )
                                }),
                        )
                        .child(button(
                            gpui::ElementId::named_usize("del-provider", card_ix),
                            "remove",
                            BtnKind::Danger,
                            true,
                            &theme,
                            delete_click,
                        )),
                )
                .child(
                    div()
                        .text_size(px(11.5))
                        .text_color(theme.muted)
                        .truncate()
                        .child(p.base_url.clone()),
                )
                .child(div().text_size(px(11.)).text_color(theme.flame).child(models))
                .into_any_element(),
        );
    }

    let add_click = cx.listener(|app, _, _, cx| app.toggle_form(cx));

    let mut pane = v_flex()
        .id("providers-pane")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .p_6()
        .gap_4()
        .child(
            h_flex()
                .items_start()
                .justify_between()
                .child(pane_header(
                    "Providers",
                    "bring your own key — stored locally only".to_string(),
                    &theme,
                ))
                .child(
                    button(
                        "toggle-form",
                        if app.form_open { "close form" } else { "+ add provider" },
                        BtnKind::Ghost,
                        true,
                        &theme,
                        add_click,
                    )
                    .w(px(120.)),
                ),
        )
        .children(cards);

    if app.form_open {
        pane = pane.child(provider_form(app, window, cx));
    }
    pane
}

fn provider_form(app: &mut DragonApp, window: &mut Window, cx: &mut Context<DragonApp>) -> Div {
    let theme = theme::theme(cx);
    let custom = app.pv_idx >= presets::PRESETS.len();

    // preset chips
    let mut chips: Vec<AnyElement> = Vec::new();
    for (i, p) in presets::PRESETS.iter().enumerate() {
        let selected = app.pv_idx == i;
        let click = cx.listener(move |app, _, _, cx| app.select_preset(i, cx));
        chips.push(
            div()
                .id(gpui::ElementId::named_usize("pv-chip", i))
                .cursor_pointer()
                .px_2p5()
                .py_1()
                .rounded_full()
                .text_size(px(10.5))
                .text_color(if selected { gpui::rgb(0x201016) } else { theme.muted })
                .bg(if selected { theme.ember } else { theme.elevated })
                .hover(|s| s.opacity(0.85))
                .on_click(click)
                .child(p.name)
                .into_any_element(),
        );
    }
    let custom_selected = custom;
    let custom_click = cx.listener(|app, _, _, cx| app.select_preset(presets::PRESETS.len(), cx));
    chips.push(
        div()
            .id("pv-chip-custom")
            .cursor_pointer()
            .px_2p5()
            .py_1()
            .rounded_full()
            .text_size(px(10.5))
            .text_color(if custom_selected {
                gpui::rgb(0x201016)
            } else {
                theme.muted
            })
            .bg(if custom_selected { theme.ember } else { theme.elevated })
            .hover(|s| s.opacity(0.85))
            .on_click(custom_click)
            .child("custom")
            .into_any_element(),
    );

    let note = if custom {
        "any OpenAI-compatible endpoint works".to_string()
    } else {
        presets::PRESETS
            .get(app.pv_idx)
            .map(|p| format!("{} — {}", p.label, p.note))
            .unwrap_or_default()
    };

    let name_focused = app.f_name.read(cx).focus_handle.is_focused(window);
    let url_focused = app.f_url.read(cx).focus_handle.is_focused(window);
    let key_focused = app.f_key.read(cx).focus_handle.is_focused(window);
    let models_focused = app.f_models.read(cx).focus_handle.is_focused(window);
    let save_click = cx.listener(|app, _, _, cx| app.save_provider(cx));

    v_flex()
        .gap_2p5()
        .p_4()
        .rounded_lg()
        .bg(theme.panel)
        .border_1()
        .border_color(theme.ember)
        .child(h_flex().flex_wrap().gap_1p5().children(chips))
        .child(div().text_size(px(11.)).text_color(theme.gold).child(note))
        .when(custom, |el| el.child(field_shell(app.f_name.clone(), name_focused, &theme)))
        .child(field_shell(app.f_url.clone(), url_focused, &theme))
        .child(field_shell(app.f_key.clone(), key_focused, &theme))
        .child(field_shell(app.f_models.clone(), models_focused, &theme))
        .child(button("save-provider", "save provider", BtnKind::Primary, true, &theme, save_click).w(px(140.)))
}

// ----------------------------------------------------------------- settings

#[allow(clippy::too_many_arguments)]
fn settings_row(
    id: &'static str,
    title: String,
    sub: String,
    on: bool,
    theme: &Theme,
    toggle: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> Div {
    h_flex()
        .w_full()
        .items_center()
        .justify_between()
        .gap_4()
        .p_4()
        .rounded_lg()
        .bg(theme.surface)
        .border_1()
        .border_color(theme.line)
        .child(
            v_flex()
                .gap_1()
                .child(
                    div()
                        .text_size(px(13.))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child(title),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(theme.muted)
                        .child(sub),
                ),
        )
        .child(switch(id, on, theme, toggle))
}

fn render_settings(app: &mut DragonApp, cx: &mut Context<DragonApp>) -> Stateful<Div> {
    let theme = theme::theme(cx);

    let shell_toggle = cx.listener(|app, _, _, cx| {
        app.toggle_shell(cx);
    });
    let engine_toggle = cx.listener(|app, _, _, cx| {
        app.toggle_engine(cx);
    });
    let thinking_toggle = cx.listener(|app, _, _, cx| {
        app.cycle_thinking(cx);
    });
    let theme_toggle = cx.listener(|app, _, _, cx| app.toggle_theme(cx));

    v_flex()
        .id("settings-pane")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .p_6()
        .gap_4()
        .child(
            pane_header(
                "Settings",
                format!(
                    "config: {} · data: {}",
                    dragon_core::config::Config::path().display(),
                    dragon_core::config::Config::data_dir().display()
                ),
                &theme,
            ),
        )
        .child(settings_row(
            "sw-shell",
            "Allow shell commands".into(),
            "run_shell can execute commands here (still asks per action)".into(),
            app.cfg.settings.allow_commands,
            &theme,
            shell_toggle,
        ))
        .child(settings_row(
            "sw-engine",
            format!("Graph memory engine ({})", app.cfg.settings.memory_engine),
            "graph = info-graph maintained by the model · hybrid = scored facts".into(),
            app.cfg.settings.graph_memory(),
            &theme,
            engine_toggle,
        ))
        .child(settings_row(
            "sw-thinking",
            format!("Deep thinking ({})", app.cfg.settings.thinking),
            "cycles off → low → medium → high reasoning effort".into(),
            app.cfg.settings.thinking != "off",
            &theme,
            thinking_toggle,
        ))
        .child(settings_row(
            "sw-theme",
            "Dark theme".into(),
            "switch between the ember dark and light palettes".into(),
            theme.is_dark(),
            &theme,
            theme_toggle,
        ))
}

// -------------------------------------------------------------------- about

fn render_about(app: &mut DragonApp, cx: &mut Context<DragonApp>) -> Div {
    let theme = theme::theme(cx);
    let gh = cx.listener(|app, _, _, cx| app.open_homepage(cx));

    v_flex()
        .flex_1()
        .min_h_0()
        .items_center()
        .justify_center()
        .gap_3()
        .bg(theme.bg)
        .child(div().text_size(px(52.)).text_color(theme.ember).child("◆"))
        .child(
            h_flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_size(px(24.))
                        .font_weight(gpui::FontWeight::BOLD)
                        .child("DRAGON"),
                )
                .child(
                    div()
                        .text_size(px(24.))
                        .font_weight(gpui::FontWeight::NORMAL)
                        .text_color(theme.muted)
                        .child("AGENT"),
                ),
        )
        .child(
            div()
                .text_size(px(12.5))
                .text_color(theme.muted)
                .child("a fast AI agent with a long memory"),
        )
        .child(
            div()
                .text_size(px(11.))
                .text_color(theme.faint)
                .child(format!(
                    "v{} · MIT · mamad720220 · built with GPUI",
                    dragon_core::VERSION
                )),
        )
        .child(
            h_flex()
                .gap_2()
                .mt_2()
                .child(button("open-github", "GitHub", BtnKind::Ghost, true, &theme, gh))
                .child(button(
                    "open-telegram",
                    format!("Telegram {}", dragon_core::TELEGRAM),
                    BtnKind::Ghost,
                    true,
                    &theme,
                    |_, _, _| {
                        let handle = dragon_core::TELEGRAM.trim_start_matches('@');
                        let _ = dragon_core::update::open_browser(&format!("https://t.me/{handle}"));
                    },
                )),
        )
}
