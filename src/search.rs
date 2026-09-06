use eframe::egui;
use std::time::{Duration, Instant};

pub const SEARCH_DELAY: Duration = Duration::from_millis(150);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchQuery {
    pub text: String,
    pub version: u64,
}

pub fn normalize(text: &str) -> String {
    text.trim().to_ascii_lowercase()
}

pub fn matches(file_name: &str, query: &str) -> bool {
    query.is_empty() || file_name.to_ascii_lowercase().contains(query)
}

/// Draft, submitted and displayed searches have separate lifetimes. In particular,
/// returning to an earlier string must not revive an earlier asynchronous result.
#[derive(Default)]
pub struct SearchState {
    pub draft: String,
    pub submitted: SearchQuery,
    pub displayed: SearchQuery,
    version: u64,
    due: Option<Instant>,
    changed_at: Option<Instant>,
    composing: bool,
    composition_base: Option<String>,
    focused_last_frame: bool,
    pub keyboard_used: bool,
}

impl SearchState {
    pub fn pending(&self) -> bool {
        self.composing || self.version != self.displayed.version
    }

    pub fn accepts(&self, query: &SearchQuery) -> bool {
        !self.composing
            && self.due.is_none()
            && query.version == self.version
            && query == &self.submitted
    }

    pub fn applied(&mut self, query: SearchQuery) -> bool {
        let changed = self.displayed.version != query.version;
        self.displayed = query;
        if changed {
            if let Some(started) = self.changed_at.take() {
                crate::performance::elapsed("search_apply_ms", started);
            }
        }
        changed
    }

    fn edited(&mut self, now: Instant) {
        self.version = self.version.wrapping_add(1);
        self.changed_at = Some(now);
        self.due = (!self.composing).then_some(now + SEARCH_DELAY);
    }

    fn submit_due(&mut self, now: Instant, immediate: bool) -> bool {
        if self.composing || !self.due.is_some_and(|due| immediate || now >= due) {
            return false;
        }
        self.submitted = SearchQuery {
            text: normalize(&self.draft),
            version: self.version,
        };
        self.due = None;
        true
    }

    pub fn clear(&mut self) {
        self.draft.clear();
        self.composing = false;
        self.composition_base = None;
        self.edited(Instant::now());
        self.submit_due(Instant::now(), true);
    }

    /// Returns true when a new query should be submitted to the sort worker.
    pub fn ui(&mut self, ui: &mut egui::Ui, enabled: bool) -> bool {
        let now = Instant::now();
        let id = egui::Id::new("filename-search");
        // egui clears text focus for Escape before constructing widgets. Restore
        // it here so this field (and its IME) gets the first chance to handle it.
        if enabled && self.focused_last_frame && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            ui.memory_mut(|m| m.request_focus(id));
        }
        let focused = ui.memory(|m| m.has_focus(id));
        let focus_requested =
            enabled && ui.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::F));
        self.keyboard_used = focused || focus_requested;
        let was_composing = self.composing;
        let mut ime_frame = was_composing;
        let mut ime_committed = false;
        if focused {
            ui.input(|i| {
                for event in &i.events {
                    match event {
                        egui::Event::Ime(egui::ImeEvent::Preedit(text)) => {
                            self.composing = !text.is_empty();
                            ime_frame = true;
                        }
                        egui::Event::Ime(egui::ImeEvent::Commit(_)) => {
                            self.composing = false;
                            ime_committed = true;
                            ime_frame = true;
                        }
                        egui::Event::Ime(egui::ImeEvent::Disabled) => {
                            self.composing = false;
                            ime_frame = true;
                        }
                        _ => {}
                    }
                }
            });
        }
        if !focused {
            self.composing = false;
        }
        if self.composing && !was_composing {
            self.composition_base = Some(self.draft.clone());
        }
        let escape = enabled
            && focused
            && !ime_frame
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        let enter =
            enabled && focused && !ime_frame && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let mut cleared = false;
        if escape {
            if self.draft.is_empty() {
                ui.memory_mut(|m| m.surrender_focus(id));
            } else {
                self.clear();
                egui::text_edit::TextEditState::default().store(ui.ctx(), id);
                cleared = true;
            }
        }
        // Allocate the group before drawing it so a wrapped toolbar cannot split
        // the label, edit and clear button across rows. Measure using the active font.
        let font = egui::TextStyle::Body.resolve(ui.style());
        let label_width = ui
            .painter()
            .layout_no_wrap("文件名搜索".into(), font.clone(), ui.visuals().text_color())
            .size()
            .x;
        let clear_width = ui
            .painter()
            .layout_no_wrap(
                "清空".into(),
                egui::TextStyle::Button.resolve(ui.style()),
                ui.visuals().text_color(),
            )
            .size()
            .x
            + 2.0 * ui.spacing().button_padding.x;
        let overhead = label_width
            + clear_width
            + 2.0 * ui.spacing().item_spacing.x
            + 2.0 * ui.spacing().button_padding.x;
        let width = (240.0 + overhead).min(ui.available_width());
        ui.allocate_ui_with_layout(
            egui::vec2(width, ui.spacing().interact_size.y),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.add_enabled_ui(enabled, |ui| {
                    let label = ui.label("文件名搜索");
                    let mut output = egui::TextEdit::singleline(&mut self.draft)
                        .id(id)
                        .hint_text("输入文件名片段 (Ctrl+F)")
                        .desired_width((width - overhead).clamp(1.0, 240.0))
                        .show(ui);
                    output.response.clone().labelled_by(label.id);
                    if focus_requested {
                        output.response.request_focus();
                        output
                            .state
                            .cursor
                            .set_char_range(Some(egui::text::CCursorRange::two(
                                egui::text::CCursor::new(0),
                                egui::text::CCursor::new(self.draft.chars().count()),
                            )));
                        output.state.clone().store(ui.ctx(), id);
                    }
                    self.keyboard_used |=
                        output.response.has_focus() || output.response.lost_focus();
                    self.focused_last_frame = output.response.has_focus();
                    if output.response.lost_focus() {
                        self.composing = false;
                    }
                    if was_composing && !self.composing {
                        if let Some(base) = self.composition_base.take() {
                            if !ime_committed {
                                // TextEdit leaves preedit text in the buffer on focus loss.
                                // A cancelled composition must never become a filename query.
                                self.draft = base;
                                egui::text_edit::TextEditState::default().store(ui.ctx(), id);
                            }
                        }
                    }
                    if output.response.changed() || (was_composing && !self.composing) {
                        self.edited(now);
                    }
                    if ui
                        .add_enabled(!self.draft.is_empty(), egui::Button::new("清空"))
                        .clicked()
                    {
                        self.clear();
                        egui::text_edit::TextEditState::default().store(ui.ctx(), id);
                        cleared = true;
                    }
                });
            },
        );
        let submitted = enabled && self.submit_due(now, enter);
        if enabled {
            if let Some(due) = self.due {
                ui.ctx()
                    .request_repaint_after(due.saturating_duration_since(now));
            }
        }
        cleared || submitted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    fn frame(ctx: &egui::Context, state: &mut SearchState, events: Vec<egui::Event>) -> bool {
        let modifiers = events
            .iter()
            .find_map(|e| match e {
                egui::Event::Key { modifiers, .. } => Some(*modifiers),
                _ => None,
            })
            .unwrap_or_default();
        let mut submitted = false;
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(600.0, 300.0),
                )),
                events,
                modifiers,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        submitted = state.ui(ui, true);
                    });
                });
            },
        );
        submitted
    }

    #[test]
    fn filename_matching_is_literal_and_case_insensitive_for_english() {
        for (name, query, expected) in [
            ("旅行 IMG 012.JPG", "  img 012  ", true),
            ("旅行 IMG 012.JPG", "旅行", true),
            ("旅行 IMG 012.JPG", ".jpg", true),
            ("旅行 IMG 012.JPG", "img012", false),
            ("旅行 IMG 012.JPG", "IMG.*", false),
            ("旅行 IMG 012.JPG", "img  012", false),
            ("旅行 IMG 012.JPG", "  ", true),
            ("photo.png", "旅行", false),
        ] {
            assert_eq!(
                matches(name, &normalize(query)),
                expected,
                "{name}: {query}"
            );
        }
    }

    #[test]
    fn debounce_and_versions_reject_drafts_and_returned_old_queries() {
        let mut state = SearchState::default();
        let now = Instant::now();
        state.draft = "cat".into();
        state.edited(now);
        assert!(state.pending());
        assert!(!state.submit_due(now + Duration::from_millis(149), false));
        assert!(state.submit_due(now + SEARCH_DELAY, false));
        let first = state.submitted.clone();
        assert!(state.accepts(&first));
        assert!(state.applied(first.clone()));
        assert!(!state.pending());
        assert!(
            !state.applied(first.clone()),
            "catalog batches must not reset scrolling"
        );
        state.draft = "dog".into();
        state.edited(now);
        assert!(
            !state.accepts(&first),
            "a draft invalidates old results before debounce expires"
        );
        state.draft = "cat".into();
        state.edited(now);
        assert!(state.submit_due(now, true));
        assert!(!state.accepts(&first));
        assert!(state.accepts(&state.submitted));
        state.clear();
        assert!(state.submitted.text.is_empty());
        assert!(state.pending());
        state.applied(state.submitted.clone());
        assert!(!state.pending());
    }

    #[test]
    fn text_widget_focus_selection_delete_enter_and_escape() {
        let ctx = egui::Context::default();
        let mut state = SearchState {
            draft: "旅行abc.jpg".into(),
            ..Default::default()
        };
        frame(
            &ctx,
            &mut state,
            vec![key(egui::Key::F, egui::Modifiers::CTRL)],
        );
        assert!(state.keyboard_used);
        let id = egui::Id::new("filename-search");
        assert!(ctx.memory(|m| m.has_focus(id)));
        let text_state = egui::TextEdit::load_state(&ctx, id).unwrap();
        let selection = text_state.cursor.char_range().unwrap();
        assert_eq!(
            selection.primary.index.abs_diff(selection.secondary.index),
            state.draft.chars().count()
        );
        frame(
            &ctx,
            &mut state,
            vec![key(egui::Key::A, egui::Modifiers::CTRL)],
        );
        frame(
            &ctx,
            &mut state,
            vec![key(egui::Key::Delete, egui::Modifiers::NONE)],
        );
        assert!(state.draft.is_empty());
        assert!(state.keyboard_used, "Delete belongs to text editing");
        frame(&ctx, &mut state, vec![egui::Event::Text("猫.jpg".into())]);
        assert!(frame(
            &ctx,
            &mut state,
            vec![key(egui::Key::Enter, egui::Modifiers::NONE)]
        ));
        assert_eq!(state.submitted.text, "猫.jpg");
        assert!(
            state.keyboard_used,
            "Enter losing text focus must not leak shortcuts"
        );
        frame(
            &ctx,
            &mut state,
            vec![key(egui::Key::F, egui::Modifiers::CTRL)],
        );
        assert!(frame(
            &ctx,
            &mut state,
            vec![key(egui::Key::Escape, egui::Modifiers::NONE)]
        ));
        assert!(state.submitted.text.is_empty());
        assert!(state.keyboard_used);
        frame(
            &ctx,
            &mut state,
            vec![key(egui::Key::Escape, egui::Modifiers::NONE)],
        );
        assert!(!ctx.memory(|m| m.has_focus(id)));
        assert!(state.keyboard_used);
    }

    #[test]
    fn ime_preedit_never_submits_and_commit_starts_a_fresh_delay() {
        let ctx = egui::Context::default();
        let mut state = SearchState::default();
        frame(
            &ctx,
            &mut state,
            vec![key(egui::Key::F, egui::Modifiers::CTRL)],
        );
        frame(
            &ctx,
            &mut state,
            vec![
                egui::Event::Ime(egui::ImeEvent::Enabled),
                egui::Event::Ime(egui::ImeEvent::Preedit("lvxing".into())),
            ],
        );
        assert!(state.composing && state.pending());
        assert!(!state.submit_due(Instant::now() + Duration::from_secs(5), true));
        frame(
            &ctx,
            &mut state,
            vec![egui::Event::Ime(egui::ImeEvent::Commit("旅行".into()))],
        );
        assert_eq!(state.draft, "旅行");
        assert!(!state.composing);
        let due = state.due.unwrap();
        assert!(!state.submit_due(due - Duration::from_millis(1), false));
        assert!(state.submit_due(due, false));
        assert_eq!(state.submitted.text, "旅行");
        state.applied(state.submitted.clone());
        frame(
            &ctx,
            &mut state,
            vec![
                egui::Event::Ime(egui::ImeEvent::Enabled),
                egui::Event::Ime(egui::ImeEvent::Preedit("mao".into())),
            ],
        );
        frame(
            &ctx,
            &mut state,
            vec![
                egui::Event::Ime(egui::ImeEvent::Preedit(String::new())),
                key(egui::Key::Escape, egui::Modifiers::NONE),
            ],
        );
        assert_eq!(
            state.draft, "旅行",
            "IME Escape must not clear committed text"
        );
        assert!(state.keyboard_used);
        frame(
            &ctx,
            &mut state,
            vec![
                egui::Event::Ime(egui::ImeEvent::Enabled),
                egui::Event::Ime(egui::ImeEvent::Preedit("uncommitted".into())),
            ],
        );
        ctx.memory_mut(|m| m.surrender_focus(egui::Id::new("filename-search")));
        frame(&ctx, &mut state, vec![]);
        assert_eq!(
            state.draft, "旅行",
            "focus loss cancels uncommitted preedit"
        );
        assert!(!state.composing);
    }
}
