//! The ⌘K palette: jump to any table or run any command by typing a few letters.

use gpui::{
    Action, Context, Entity, EventEmitter, FocusHandle, IntoElement, Render, ScrollStrategy,
    SharedString, Subscription, UniformListScrollHandle, Window, div, prelude::*, px, svg,
    uniform_list,
};
use gpui_component::input::{Escape, Input, InputEvent, InputState};

use gpui_component::Sizable as _;

use crate::theme::ActiveTheme;
use crate::widgets::{self, RADIUS_CONTROL, kbd};
use crate::{PaletteDown, PaletteUp};

const ROW_HEIGHT: f32 = 36.;
const VISIBLE_ROWS: usize = 10;

pub enum Target {
    Table {
        session: usize,
        schema: String,
        table: String,
    },
    Action(Box<dyn Action>),
}

pub struct Item {
    /// Short group label: TABLE, VIEW, COMMAND.
    pub kind: &'static str,
    pub label: SharedString,
    pub detail: Option<SharedString>,
    pub keys: Option<SharedString>,
    pub target: Target,
}

pub enum PaletteEvent {
    Pick(Target),
    Dismiss,
}

pub struct Palette {
    focus_handle: FocusHandle,
    input: Entity<InputState>,
    items: Vec<Item>,
    /// Indices into `items` that match the query, best first.
    matches: Vec<usize>,
    selected: usize,
    scroll: UniformListScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<PaletteEvent> for Palette {}

impl Palette {
    pub fn new(items: Vec<Item>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Go to a table or run a command…"));
        let subscription =
            cx.subscribe_in(
                &input,
                window,
                |this, _, event: &InputEvent, _, cx| match event {
                    InputEvent::Change => this.refilter(cx),
                    InputEvent::PressEnter { .. } => this.confirm(cx),
                    _ => {}
                },
            );
        input.update(cx, |input, cx| input.focus(window, cx));

        let mut palette = Self {
            focus_handle: cx.focus_handle(),
            input,
            matches: (0..items.len()).collect(),
            items,
            selected: 0,
            scroll: UniformListScrollHandle::new(),
            _subscriptions: vec![subscription],
        };
        palette.refilter(cx);
        palette
    }

    fn refilter(&mut self, cx: &mut Context<Self>) {
        let query = self.input.read(cx).value().to_string();
        let mut scored: Vec<(i32, usize)> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(ix, item)| fuzzy_score(&query, &item.label).map(|score| (score, ix)))
            .collect();
        // Best score first; ties keep the original order (tables before commands).
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        self.matches = scored.into_iter().map(|(_, ix)| ix).collect();
        self.selected = 0;
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.matches.is_empty() {
            return;
        }
        let len = self.matches.len() as isize;
        self.selected = (self.selected as isize + delta).rem_euclid(len) as usize;
        self.scroll
            .scroll_to_item(self.selected, ScrollStrategy::Nearest);
        cx.notify();
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        let Some(&ix) = self.matches.get(self.selected) else {
            return;
        };
        let item = self.items.swap_remove(ix);
        cx.emit(PaletteEvent::Pick(item.target));
    }

    fn pick(&mut self, position: usize, cx: &mut Context<Self>) {
        self.selected = position;
        self.confirm(cx);
    }
}

impl Render for Palette {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let selected = self.selected;
        let rows: Vec<_> = self
            .matches
            .iter()
            .map(|&ix| {
                let item = &self.items[ix];
                (
                    item.kind,
                    item.label.clone(),
                    item.detail.clone(),
                    item.keys.clone(),
                )
            })
            .collect();
        let row_count = rows.len();
        let list_theme = theme.clone();
        let this = cx.entity().downgrade();

        let list = uniform_list("palette-items", row_count, move |range, _, _| {
            range
                .map(|position| {
                    let (kind, label, detail, keys) = rows[position].clone();
                    let active = position == selected;
                    let this = this.clone();
                    let icon = match kind {
                        "TABLE" => "table-2",
                        "VIEW" => "eye",
                        "CONNECTION" => "database",
                        _ => "play",
                    };
                    div()
                        .id(position)
                        .w_full()
                        .h(px(ROW_HEIGHT))
                        .px(px(10.))
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .rounded(px(RADIUS_CONTROL))
                        .cursor_pointer()
                        .when(active, |el| {
                            el.bg(list_theme.acc_wash).text_color(list_theme.text)
                        })
                        .when(!active, |el| {
                            el.text_color(list_theme.t2).hover(|s| s.bg(list_theme.s3))
                        })
                        .on_click(move |_, _, cx| {
                            this.update(cx, |this, cx| this.pick(position, cx)).ok();
                        })
                        .child(
                            svg()
                                .path(crate::assets::icon(icon))
                                .size(px(15.))
                                .flex_none()
                                .text_color(if active {
                                    list_theme.acc
                                } else {
                                    list_theme.t3
                                }),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_size(crate::theme::text(13.))
                                .child(label),
                        )
                        .when_some(detail, |el, detail| {
                            el.child(
                                div()
                                    .min_w(px(0.))
                                    .truncate()
                                    .text_size(crate::theme::text(11.5))
                                    .text_color(list_theme.t3)
                                    .child(detail),
                            )
                        })
                        .child(div().flex_1())
                        .when(!matches!(kind, "COMMAND"), |el| {
                            el.child(
                                div()
                                    .flex_none()
                                    .text_size(crate::theme::text(11.))
                                    .text_color(list_theme.t3)
                                    .child(match kind {
                                        "TABLE" => "Table",
                                        "VIEW" => "View",
                                        _ => "",
                                    }),
                            )
                        })
                        .when_some(keys, |el, keys| el.child(kbd(&list_theme, &keys)))
                })
                .collect()
        })
        .track_scroll(&self.scroll)
        .px(px(6.))
        .h(px(ROW_HEIGHT * row_count.clamp(1, VISIBLE_ROWS) as f32));

        let hint = |keys: &str, text: &'static str| {
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(kbd(&theme, keys))
                .child(text)
        };

        let panel = widgets::overlay_panel(&theme)
            .id("palette-panel")
            .w(px(620.))
            // Clicking anywhere outside the panel closes the dialog.
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(PaletteEvent::Dismiss)))
            .child(
                div()
                    .h(px(52.))
                    .px(px(18.))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .border_b_1()
                    .border_color(theme.line_2)
                    .child(
                        svg()
                            .path(crate::assets::icon("search"))
                            .size(px(17.))
                            .flex_none()
                            .text_color(theme.acc),
                    )
                    .child(
                        div().flex_1().child(
                            Input::new(&self.input)
                                .with_size(gpui_component::Size::Size(px(14. / 0.875)))
                                .appearance(false),
                        ),
                    ),
            )
            .child(div().pt(px(6.)).when(row_count == 0, |el| {
                el.child(
                    div()
                        .h(px(ROW_HEIGHT * 2.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(theme.t3)
                        .child("Nothing matches"),
                )
            }))
            .when(row_count > 0, |el| el.child(list))
            .child(
                div()
                    .mt(px(6.))
                    .h(px(36.))
                    .px(px(16.))
                    .flex()
                    .items_center()
                    .gap(px(16.))
                    .border_t_1()
                    .border_color(theme.line_2)
                    .text_size(crate::theme::text(11.))
                    .text_color(theme.t3)
                    .child(hint("↑↓", "to move"))
                    .child(hint("↵", "to open"))
                    .child(hint("esc", "to close")),
            );

        div()
            .id("palette-scrim")
            .key_context("Palette")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|_, _: &Escape, _, cx| cx.emit(PaletteEvent::Dismiss)))
            .on_action(cx.listener(|this, _: &PaletteUp, _, cx| this.step(-1, cx)))
            .on_action(cx.listener(|this, _: &PaletteDown, _, cx| this.step(1, cx)))
            .absolute()
            .inset_0()
            .occlude()
            .flex()
            .flex_col()
            .items_center()
            .pt(px(120.))
            .bg(theme.scrim)
            .child(panel)
    }
}

/// Subsequence match of `query` in `text`, case-insensitive. Higher is better;
/// `None` when the letters don't all appear in order. Consecutive letters, word
/// starts and an early first hit score higher; an empty query matches everything.
pub fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    let query: Vec<char> = query
        .trim()
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    if query.is_empty() {
        return Some(0);
    }
    let text: Vec<char> = text.chars().collect();
    let lower: Vec<char> = text.iter().flat_map(|c| c.to_lowercase()).collect();
    if lower.len() != text.len() {
        // Rare case-folding that changes length; fall back to a plain substring test.
        let haystack: String = lower.iter().collect();
        let needle: String = query.iter().collect();
        return haystack.contains(&needle).then_some(1);
    }

    let mut score = 0;
    let mut position = 0;
    let mut previous: Option<usize> = None;
    for &wanted in &query {
        let found = (position..lower.len()).find(|&i| lower[i] == wanted)?;
        let word_start = found == 0
            || !text[found - 1].is_alphanumeric()
            || (text[found].is_uppercase() && text[found - 1].is_lowercase());
        score += 1;
        if previous == Some(found.wrapping_sub(1)) {
            score += 5;
        }
        if word_start {
            score += 8;
        }
        if previous.is_none() {
            score -= found.min(10) as i32;
        }
        previous = Some(found);
        position = found + 1;
    }
    // Prefer shorter labels when everything else is equal.
    Some(score * 10 - text.len() as i32)
}

#[cfg(test)]
mod tests {
    use super::fuzzy_score;

    #[test]
    fn matches_subsequences_only() {
        assert!(fuzzy_score("ord", "orders").is_some());
        assert!(fuzzy_score("ors", "orders").is_some());
        assert!(fuzzy_score("xyz", "orders").is_none());
        assert!(fuzzy_score("sro", "orders").is_none());
        assert_eq!(fuzzy_score("", "anything"), Some(0));
    }

    #[test]
    fn ranks_prefixes_and_word_starts_higher() {
        let score = |q, t| fuzzy_score(q, t).unwrap();
        assert!(score("ord", "orders") > score("ord", "word_order"));
        assert!(score("nt", "New Tab") > score("nt", "Explain statement"));
        assert!(score("ru", "Run All") > score("ru", "true_users"));
        // Case doesn't matter; spaces in the query are ignored.
        assert_eq!(
            fuzzy_score("RUN all", "Run All"),
            fuzzy_score("runall", "Run All")
        );
    }

    #[test]
    fn prefers_shorter_labels_on_ties() {
        assert!(
            fuzzy_score("users", "users").unwrap() > fuzzy_score("users", "users_archive").unwrap()
        );
    }
}
