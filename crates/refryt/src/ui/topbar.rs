//! The bar across the top of the content: history, search, settings, account, window buttons.
//!
//! It used to be a full-width titlebar painted at the very top of the window, which is what kept
//! the sidebar from reaching the top edge. It is now a panel claimed *after* the sidebar, so the
//! rail runs the whole left side and this bar starts where the rail ends.
//!
//! It also carries the window buttons (everywhere but macOS, whose traffic lights sit top-left
//! over the sidebar). That is the one place this shell departs from the
//! reference, where the queue panel runs full height and the top bar covers only the content:
//! minimise / maximise / close have to sit at the window's top-right corner and must not move
//! when the queue opens, and letting this bar run to the window's right edge is what keeps them
//! there.

use egui::{CornerRadius, Rect, Sense, Ui, vec2};

use crate::app::{App, Route};
use crate::chrome;
use crate::icons::Icon;
use crate::theme;
use crate::widgets;

/// How wide the search pill is at most, and how small it is allowed to get before the bar is
/// just too narrow to hold everything — the magnifier and a few letters. Only a window at its
/// minimum with the sidebar dragged wide gets near it; above that the gap always wins.
const SEARCH_W: f32 = 360.0;
const MIN_SEARCH_W: f32 = 96.0;
const SEARCH_H: f32 = 36.0;

/// The clear space between the search pill and the History clock, kept at every width.
const RIGHT_GAP: f32 = 20.0;

/// Back + Forward and the gaps around them, the fixed part left of the search pill.
const LEFT_W: f32 = 32.0 + 4.0 + 32.0 + 12.0;

pub fn top_bar(app: &mut App, ui: &mut egui::Ui) {
    let p = app.palette;
    let rect = ui.available_rect_before_wrap();

    // The drag surface goes down first so every control painted after it wins the pointer.
    chrome::drag_surface(ui, rect, app.decorated, "topbar");

    // The right-hand controls go down *first*, so the search can size itself to whatever is left
    // of the bar instead of running under them when the window is dragged narrow.
    let mut right = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    // On macOS the window buttons are the system's traffic lights, at the other end of the
    // window over the sidebar (`chrome::TrafficLights`).
    if !app.decorated && !chrome::NATIVE_WINDOW_BUTTONS {
        let close_clicked = chrome::window_buttons(&mut right, &p);
        if close_clicked {
            app.request_close(ui.ctx());
        }
        right.add_space(10.0);
    }
    let signed_in = app.signed_in();
    let chip = account_chip(app, &mut right);
    // The chip opens the account menu in every state and on every platform — View profile,
    // Switch account, Settings, Sign out when signed in; Log in and Settings when not. Log in
    // leads to the sign-in window on Windows and to Settings' browser import elsewhere, but
    // the menu itself no longer differs by platform. Button-anchored: `Toggle` records no
    // pointer position, so a pointer-anchored popup would never render.
    account_menu(app, &mut right, &chip, signed_in);
    right.add_space(2.0);
    // Listening history beside the chip, as a clock — it is a page, not a setting.
    // Settings itself lives only under the chip, which carries it in every state. One door,
    // one keyhole — a second gear beside the chip was two doors to the same room.
    if widgets::icon_button(&mut right, &p, Icon::History, 18.0, false)
        .on_hover_text(crate::ui::tips::HISTORY)
        .clicked()
    {
        app.navigate(Route::History);
    }
    // Whatever the right group took, plus a gap so the search never touches it.
    let reserved = (rect.right() - right.min_rect().left()).max(0.0) + RIGHT_GAP;

    let mut bar = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );

    let can_back = app.can_go_back();
    let can_forward = app.can_go_forward();
    let back = history_button(&mut bar, &p, Icon::ChevronLeft, can_back);
    bar.add_space(4.0);
    let forward = history_button(&mut bar, &p, Icon::ChevronRight, can_forward);
    bar.add_space(12.0);
    // The gap wins over the search's comfortable minimum: in a narrow window the pill keeps
    // shrinking rather than running up against the History clock.
    let search_w = (rect.width() - LEFT_W - reserved).clamp(MIN_SEARCH_W, SEARCH_W);
    search_field(app, &mut bar, search_w);

    if back.clicked() && can_back {
        app.back();
    }
    if forward.clicked() && can_forward {
        app.forward();
    }
}

/// Back / Forward, as round buttons on the window colour — dimmed and inert with nowhere to go.
fn history_button(
    ui: &mut Ui,
    palette: &theme::Palette,
    icon: Icon,
    enabled: bool,
) -> egui::Response {
    let d = 32.0;
    let (rect, response) = ui.allocate_exact_size(vec2(d, d), Sense::click());
    if ui.is_rect_visible(rect) {
        let t = if enabled {
            widgets::lit(ui, &response)
        } else {
            0.0
        };
        // No disc: it was painted in the window colour onto a transparent top bar that
        // already shows the window colour, so it drew nothing — and once the shell is
        // translucent it would have drawn an opaque coin floating over the blur.
        let fg = if enabled {
            widgets::mix(palette.secondary, palette.text, t)
        } else {
            palette.dim
        };
        let nudge = if enabled {
            widgets::press_offset(&response)
        } else {
            egui::Vec2::ZERO
        };
        icon.image(fg, 18.0).paint_at(
            ui,
            Rect::from_center_size(rect.center() + nudge, vec2(18.0, 18.0)),
        );
        if enabled && response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }
    response
}

/// The search box as a pill with the magnifier inside it, rather than egui's default field.
fn search_field(app: &mut App, ui: &mut Ui, width: f32) {
    let p = app.palette;
    let (rect, response) = ui.allocate_exact_size(vec2(width, SEARCH_H), Sense::hover());
    let focused = ui.memory(|m| m.has_focus(search_id()));
    let t = widgets::lit(ui, &response).max(if focused { 1.0 } else { 0.0 });

    ui.painter().rect(
        rect,
        CornerRadius::same((SEARCH_H / 2.0) as u8),
        p.glass(widgets::mix(p.surface, p.surface_hover, t)),
        egui::Stroke::new(
            1.0,
            if focused {
                p.outline
            } else {
                p.glass(p.surface)
            },
        ),
        egui::StrokeKind::Inside,
    );
    Icon::Search.image(p.secondary, 16.0).paint_at(
        ui,
        Rect::from_center_size(rect.left_center() + vec2(18.0, 0.0), vec2(16.0, 16.0)),
    );

    let inner = Rect::from_min_max(
        rect.left_center() + vec2(34.0, -SEARCH_H / 2.0),
        rect.right_center() + vec2(-12.0, SEARCH_H / 2.0),
    );
    let mut state = BoxState {
        query: &mut app.query,
        suggestions: &app.suggestions,
        suggestions_for: app.suggestions_for.as_deref(),
        history: &mut app.search_history,
        cursor: &mut app.suggestion_cursor,
        dismissed: &mut app.recent_dismissed,
    };
    let out = search_box(ui, &p, &response, inner, width, focused, &mut state);
    if out.typed {
        app.query_changed_at = Some(std::time::Instant::now());
    }
    if out.clear_suggestions {
        app.clear_suggestions();
    }
    if out.history_changed {
        app.save_search_history();
    }
    if out.submit {
        app.submit_search();
    }
}

/// What the search box needs from the app, borrowed for one frame: the text, YouTube's
/// autocomplete and the query it answers, the recent searches, and the list's own state.
pub(crate) struct BoxState<'a> {
    pub query: &'a mut String,
    pub suggestions: &'a [String],
    pub suggestions_for: Option<&'a str>,
    pub history: &'a mut crate::search_history::SearchHistory,
    /// Where the arrow keys have moved the highlight; see `App::suggestion_cursor`.
    pub cursor: &'a mut Option<usize>,
    /// See `App::recent_dismissed`.
    pub dismissed: &'a mut bool,
}

/// What a frame of the search box asks the app to do, applied by `search_field` in this order.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct BoxOutcome {
    /// The text changed: autocomplete is due for a refetch.
    pub typed: bool,
    pub clear_suggestions: bool,
    /// The recent searches changed and need saving.
    pub history_changed: bool,
    pub submit: bool,
    /// Where the list's rows, their ✕ buttons and Clear were drawn this frame, empty with no
    /// list open. Only the tests read these: they click what a person would.
    pub rows: Vec<Rect>,
    pub removes: Vec<Rect>,
    pub clear: Option<Rect>,
    /// The part of the list that is in view, which is less than all of it once it scrolls.
    pub list: Option<Rect>,
    /// The whole dropdown, frame included.
    pub popup: Option<Rect>,
}

/// The text field and the list under it: YouTube's autocomplete while typing, the recent
/// searches while the box is focused and empty. Split from `search_field` so a headless test can
/// drive it without a whole `App`.
pub(crate) fn search_box(
    ui: &mut Ui,
    p: &theme::Palette,
    response: &egui::Response,
    inner: Rect,
    width: f32,
    focused: bool,
    state: &mut BoxState<'_>,
) -> BoxOutcome {
    let p = *p;
    let mut out = BoxOutcome::default();
    let mut field_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(inner)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    // Autocomplete is offered only while the field has focus and the reply matches the text in
    // the box, so a late answer for an older query never flashes up.
    let suggesting = focused
        && !state.suggestions.is_empty()
        && state.suggestions_for == Some(state.query.as_str());
    // With nothing typed, the same list offers the recent searches instead, until Escape.
    let recent = focused
        && !suggesting
        && state.query.trim().is_empty()
        && !state.history.entries.is_empty()
        && !*state.dismissed;
    let show = suggesting || recent;
    // Consume the list-navigation keys *before* the field so they drive the popup rather than the
    // caret; Left/Right are left alone, so editing still feels normal.
    let (mut up, mut down, mut enter, mut esc) = (false, false, false, false);
    if show {
        ui.input_mut(|i| {
            up = i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp);
            down = i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown);
            enter = i.consume_key(egui::Modifiers::NONE, egui::Key::Enter);
            esc = i.consume_key(egui::Modifiers::NONE, egui::Key::Escape);
        });
    }

    let field = egui::TextEdit::singleline(&mut *state.query)
        .id(search_id())
        .hint_text("What do you want to play?")
        .desired_width(inner.width())
        .frame(egui::Frame::NONE)
        .font(theme::regular(13.0));
    let field_response = field_ui.add(field);
    if field_response.changed() {
        // Typing refetches autocomplete only; the search waits for a submit. The cursor resets
        // so a stale arrow selection cannot attach to new text.
        out.typed = true;
        *state.cursor = None;
    }
    if field_response.changed() || field_response.gained_focus() {
        *state.dismissed = false;
    }

    // Submit on a bare Enter. When a list is open it consumes Enter above, so this path only
    // runs with no list up: search exactly what was typed (a blank box searches nothing).
    if !show {
        if field_response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            out.submit = !state.query.trim().is_empty();
        }
        return out;
    }

    // The items are cloned out so the popup closure does not borrow `state` while the actions
    // below still need it. `p` is a copy for the same reason.
    let items = if recent {
        state.history.entries.clone()
    } else {
        state.suggestions.to_vec()
    };
    // `None` until an arrow key picks a row: the list opens with nothing highlighted, so Enter
    // searches what was typed and the first Down lands on the first suggestion.
    let cursor = state.cursor.filter(|i| *i < items.len());
    let mut picked = None;
    let mut removed = None;
    let mut cleared = false;
    let popup = egui::Popup::from_response(response)
        .id(ui.id().with("ytm-search-suggestions"))
        .kind(egui::PopupKind::Menu)
        // No margin above or below, so scrolled rows run to the edges; the room at the top is
        // part of the list instead and scrolls away with it.
        .frame(widgets::menu_frame(ui, &p).inner_margin(egui::Margin::symmetric(6, 0)))
        .layout(egui::Layout::top_down_justified(egui::Align::Min))
        .align(egui::RectAlign::BOTTOM_START)
        .width(width)
        .gap(4.0)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .open_memory(Some(egui::SetOpenCommand::Bool(true)))
        .show(|mui| {
            // Recent searches from the sixth on scroll, heading and all, with half of the sixth
            // peeking out so the list looks scrollable. The suggestions are a short list from
            // YouTube and keep their look.
            let gap = mui.spacing().item_spacing.y;
            let max_height = if recent {
                LIST_TOP_PAD + RECENT_HEADER_H + gap + 5.0 * (30.0 + gap) + 15.0
            } else {
                f32::INFINITY
            };
            let shown_id = mui.id().with("cursor-scrolled-to");
            if cursor.is_none() {
                mui.data_mut(|d| d.remove::<usize>(shown_id));
            }
            let shown = egui::ScrollArea::vertical()
                .max_height(max_height)
                .auto_shrink([false, true])
                // An arrow press lands on its row at once, the way a menu moves.
                .animated(false)
                .show(mui, |mui| {
                    mui.add_space(LIST_TOP_PAD);
                    if recent {
                        let (clicked, rect) = recent_header(mui, &p);
                        cleared = clicked;
                        out.clear = Some(rect);
                    }
                    for (i, item) in items.iter().enumerate() {
                        let (row, resp) = mui
                            .allocate_exact_size(vec2(mui.available_width(), 30.0), Sense::click());
                        // The remove button sits on top of the row, so a click on it is the button's
                        // alone and never also searches.
                        let remove = recent.then(|| {
                            let rect = Rect::from_center_size(
                                row.right_center() - vec2(18.0, 0.0),
                                vec2(24.0, 24.0),
                            );
                            (
                                rect,
                                mui.interact(rect, resp.id.with("remove"), Sense::click()),
                            )
                        });
                        out.rows.push(row);
                        if let Some((rect, _)) = &remove {
                            out.removes.push(*rect);
                        }
                        if mui.is_rect_visible(row) {
                            let selected = cursor == Some(i);
                            // Once per arrow move, so the wheel is free while a row stays picked.
                            if selected && mui.data(|d| d.get_temp::<usize>(shown_id)) != Some(i) {
                                resp.scroll_to_me(None);
                                mui.data_mut(|d| d.insert_temp(shown_id, i));
                            }
                            // Over the ✕ the pointer is on the button, not the row; the row stays lit.
                            let on_x = remove.as_ref().is_some_and(|(_, x)| x.hovered());
                            let t = widgets::lit(mui, &resp).max(if selected || on_x {
                                1.0
                            } else {
                                0.0
                            });
                            if t > 0.001 {
                                mui.painter().rect_filled(
                                    row,
                                    CornerRadius::same(theme::RADIUS_SMALL + 2),
                                    widgets::mix(p.surface, p.surface_hover, t),
                                );
                            }
                            let text_x = 14.0;
                            let mut text_w = row.width() - 24.0;
                            if let Some((rect, x)) = &remove {
                                // The ✕ only shows on the row being pointed at or picked with the arrows,
                                // so the list reads as a plain list; its own hover gets a round backing.
                                if resp.hovered() || on_x || selected {
                                    let tx = widgets::lit(mui, x);
                                    if tx > 0.001 {
                                        mui.painter().circle_filled(
                                            rect.center(),
                                            11.0,
                                            widgets::mix(p.surface_hover, p.surface_active, tx),
                                        );
                                    }
                                    // Drawn rather than the icon's 12 px raster, which came out spiky:
                                    // two short strokes with rounded ends read softer at this size.
                                    let stroke = egui::Stroke::new(
                                        1.5,
                                        widgets::mix(p.secondary, p.text, tx),
                                    );
                                    let c = rect.center();
                                    for d in [vec2(3.5, 3.5), vec2(3.5, -3.5)] {
                                        mui.painter().line_segment([c - d, c + d], stroke);
                                        for end in [c - d, c + d] {
                                            mui.painter().circle_filled(end, 0.75, stroke.color);
                                        }
                                    }
                                    if on_x {
                                        mui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                    }
                                }
                                // Room for the ✕ is kept either way, so the text never shifts under it.
                                text_w -= 32.0;
                            }
                            // Suggestions are arbitrary strings from YouTube; a long one would paint
                            // straight through the popup's right edge. Ellipsise it to the row, the
                            // same way a row's own title is shortened.
                            let font = theme::regular(13.0);
                            let label =
                                widgets::truncate_to(mui, item, font.clone(), text_w.max(0.0));
                            crate::emoji::paint_line(
                                mui,
                                row.left_center() + vec2(text_x, 0.0),
                                &label,
                                font,
                                p.text,
                            );
                        }
                        if remove.as_ref().is_some_and(|(_, x)| x.clicked()) {
                            removed = Some(i);
                        } else if resp.clicked() {
                            picked = Some(i);
                        }
                    }
                });
            out.list = Some(shown.inner_rect);
        });
    out.popup = popup.map(|r| r.response.rect);

    // A click accepts and searches that row outright. Enter searches what was typed, unless the
    // arrow keys moved the highlight — then the highlighted row wins. Escape hides the list
    // without changing the query; Up/Down move the cursor.
    // Clicking the list takes focus from the field, which would close it; after a removal the
    // rest of the list stays open, so focus goes back.
    if cleared {
        state.history.clear();
        *state.cursor = None;
        out.history_changed = true;
        ui.memory_mut(|m| m.request_focus(search_id()));
    } else if let Some(i) = removed {
        state.history.remove(i);
        *state.cursor = None;
        out.history_changed = true;
        ui.memory_mut(|m| m.request_focus(search_id()));
    } else if let Some(i) = picked {
        state.query.clone_from(&items[i]);
        caret_to_end(ui.ctx(), state.query);
        out.clear_suggestions = true;
        out.submit = true;
    } else if enter {
        // The highlighted row wins when there is one; otherwise this is the typed text.
        if let Some(i) = cursor {
            state.query.clone_from(&items[i]);
            caret_to_end(ui.ctx(), state.query);
        }
        out.clear_suggestions = true;
        out.submit = !state.query.trim().is_empty();
    } else if esc {
        if recent {
            *state.dismissed = true;
            *state.cursor = None;
        } else {
            out.clear_suggestions = true;
        }
    } else if up {
        *state.cursor = step_cursor(cursor, false, items.len());
    } else if down {
        *state.cursor = step_cursor(cursor, true, items.len());
    }
    out
}

/// The recent list's heading, with a Clear button on its right: whether Clear was clicked, and
/// where it is.
const RECENT_HEADER_H: f32 = 26.0;
/// The menu's usual top margin, kept inside the search list so it scrolls off with the rows.
const LIST_TOP_PAD: f32 = 6.0;

fn recent_header(ui: &mut Ui, p: &theme::Palette) -> (bool, Rect) {
    let (row, _) =
        ui.allocate_exact_size(vec2(ui.available_width(), RECENT_HEADER_H), Sense::hover());
    let font = theme::regular(12.0);
    ui.painter().text(
        row.left_center() + vec2(14.0, 0.0),
        egui::Align2::LEFT_CENTER,
        "Recent searches",
        font.clone(),
        p.secondary,
    );
    let galley = ui
        .painter()
        .layout_no_wrap("Clear".to_owned(), font.clone(), p.secondary);
    let rect = Rect::from_min_size(
        row.right_center() - vec2(14.0 + galley.size().x, galley.size().y / 2.0),
        galley.size(),
    )
    .expand(4.0);
    let clear = ui.interact(rect, ui.id().with("recent-clear"), Sense::click());
    let tint = if clear.hovered() { p.text } else { p.secondary };
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "Clear",
        font,
        tint,
    );
    if clear.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    (clear.clicked(), rect)
}

/// Move the suggestion highlight: Down from nothing picks the first row, Up from nothing the
/// last; Up from the first clears the highlight again, so it can always be dismissed without
/// touching the text. `None` means nothing is highlighted — the state the list opens in.
/// After the field's text is swapped for a picked row, put its caret at the end with nothing
/// selected. egui keeps the old caret otherwise, left where the typed prefix ended.
fn caret_to_end(ctx: &egui::Context, text: &str) {
    let mut field = egui::TextEdit::load_state(ctx, search_id()).unwrap_or_default();
    let end = egui::text::CCursor::new(text.chars().count());
    field
        .cursor
        .set_char_range(Some(egui::text::CCursorRange::one(end)));
    field.store(ctx, search_id());
}

fn step_cursor(cursor: Option<usize>, down: bool, len: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    if down {
        Some(match cursor {
            None => 0,
            Some(i) => (i + 1).min(len - 1),
        })
    } else {
        match cursor {
            None => Some(len - 1),
            Some(0) => None,
            Some(i) => Some(i - 1),
        }
    }
}

/// A stable id, so focus can be read before the field is added — and requested from outside
/// this module entirely, by the global "jump to search" keyboard shortcut in `App::handle_keys`.
pub(crate) fn search_id() -> egui::Id {
    egui::Id::new("ytm-search-field")
}

/// The account chip: the signed-in account's avatar, opening the account menu. A generic glyph
/// signed out.
fn account_chip(app: &mut App, ui: &mut Ui) -> egui::Response {
    let name = match &app.auth {
        crate::auth::AuthState::SignedIn { account } => Some(account.clone()),
        _ => None,
    };
    avatar_circle(app, ui, name.as_deref())
}

/// The circular avatar on its own: the photo, the initial while it loads, a glyph signed out.
fn avatar_circle(app: &mut App, ui: &mut Ui, name: Option<&str>) -> egui::Response {
    let p = app.palette;
    // The photo YouTube already sent with the channel list.
    let photo = name.and_then(|_| {
        let current = app
            .accounts
            .iter()
            .find(|a| a.page_id == app.settings.account_page_id)
            .or_else(|| app.accounts.iter().find(|a| a.selected))
            .or(app.accounts.first());
        let url = current?.thumbnail.clone()?;
        let crate::app::App {
            images, backend, ..
        } = app;
        crate::ui::art_texture(images, backend, ui.ctx(), Some(&url), 28.0)
    });
    let d = 28.0;
    let (rect, response) = ui.allocate_exact_size(vec2(d, d), Sense::click());
    if ui.is_rect_visible(rect) {
        let t = widgets::lit(ui, &response);
        match (name, photo) {
            (Some(_), Some(tex)) => {
                widgets::paint_art(ui, &p, rect, Some(tex), widgets::ArtShape::Circle);
            }
            (Some(account), None) => {
                ui.painter().circle_filled(
                    rect.center(),
                    d / 2.0,
                    p.glass(widgets::mix(p.surface_active, p.accent, t)),
                );
                let initial = account
                    .chars()
                    .next()
                    .map(|c| c.to_uppercase().to_string())
                    .unwrap_or_default();
                ui.painter().text(
                    rect.center() + widgets::press_offset(&response),
                    egui::Align2::CENTER_CENTER,
                    initial,
                    theme::semibold(13.0),
                    p.text,
                );
            }
            (None, _) => {
                let fg = widgets::mix(p.secondary, p.text, t);
                Icon::CircleUser.image(fg, 20.0).paint_at(
                    ui,
                    Rect::from_center_size(
                        rect.center() + widgets::press_offset(&response),
                        vec2(20.0, 20.0),
                    ),
                );
            }
        }
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }
    response.on_hover_text(name.unwrap_or("Not signed in"))
}

/// What the chip opens: signed in, the channels this Google account can browse as — each with
/// its photo and a tick on the current one — then Settings and Sign out; signed out, Log in and
/// Settings. Log in opens the sign-in window on Windows and navigates to Settings' browser
/// import everywhere else.
fn account_menu(app: &mut App, ui: &mut Ui, chip: &egui::Response, signed_in: bool) {
    let popup = egui::Popup::new(
        ui.id().with("ytm-account-menu"),
        ui.ctx().clone(),
        chip,
        ui.layer_id(),
    )
    .kind(egui::PopupKind::Menu)
    .frame(widgets::menu_frame(ui, &app.palette))
    .layout(egui::Layout::top_down_justified(egui::Align::Min))
    .width(widgets::MENU_WIDTH)
    .gap(0.0)
    .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
    .open_memory(chip.clicked().then_some(egui::SetOpenCommand::Toggle));

    enum Choice {
        LogIn,
        Profile(String),
        Switch(Option<String>),
        Settings,
        Help,
        SignOut,
    }
    // A failed attempt is reported where it was started — right here — not only in Settings,
    // which nobody thinks to open after clicking Log in in this menu.
    let sign_in_error = match &app.auth {
        crate::auth::AuthState::Failed(msg) if !signed_in => Some(msg.clone()),
        _ => None,
    };
    let mut choice = None;
    popup.show(|mui| {
        let p = app.palette;
        // Cloned, not borrowed: the picks below take `&mut App` after the popup closes.
        let accounts = app.accounts.clone();
        let current_id = app.settings.account_page_id.clone();
        // The current channel's profile id, for the View profile item below the list.
        let current_channel_id = accounts
            .iter()
            .find(|a| a.page_id == current_id)
            .or_else(|| accounts.iter().find(|a| a.selected))
            .or(accounts.first())
            .and_then(|a| a.channel_id.clone());
        // Signed out, this item is the menu's reason to exist: on Windows the sign-in window is
        // the one path that works when every browser on the machine is a modern Chromium, and
        // on Linux and macOS it is the door to Settings' browser import.
        if !signed_in && widgets::menu_item(mui, &p, Icon::CircleUser, "Log in", None).clicked() {
            choice = Some(Choice::LogIn);
            mui.close();
        }
        if let Some(error) = &sign_in_error {
            mui.add_space(2.0);
            mui.add(
                egui::Label::new(
                    egui::RichText::new(error)
                        .font(theme::regular(11.0))
                        .color(p.danger),
                )
                .wrap(),
            );
            mui.add_space(2.0);
        }
        // The channels, inline and directly on top: photo, name, a tick on the current one. No
        // byline — the subscriber count is noise here. Never signed out: this menu opens then
        // too on Windows, and the account list is session data.
        for account in accounts.into_iter().filter(|_| signed_in) {
            let current = current_id == account.page_id;
            let (rect, resp) =
                mui.allocate_exact_size(vec2(mui.available_width(), 40.0), Sense::click());
            if mui.is_rect_visible(rect) {
                let t = widgets::lit(mui, &resp);
                if resp.is_pointer_button_down_on() {
                    mui.painter().rect_filled(
                        rect,
                        CornerRadius::same(theme::RADIUS_SMALL + 2),
                        p.surface_active,
                    );
                } else if t > 0.001 {
                    mui.painter().rect_filled(
                        rect,
                        CornerRadius::same(theme::RADIUS_SMALL + 2),
                        widgets::mix(p.surface, p.surface_hover, t),
                    );
                }
                let nudge = widgets::press_offset(&resp);
                let photo_rect = Rect::from_min_size(
                    rect.left_center() + vec2(6.0, -12.0) + nudge,
                    vec2(24.0, 24.0),
                );
                let (images, backend) = (&mut app.images, &app.backend);
                match account.thumbnail.as_ref().and_then(|url| {
                    crate::ui::art_texture(images, backend, mui.ctx(), Some(url), 24.0)
                }) {
                    Some(tex) => widgets::paint_art(
                        mui,
                        &p,
                        photo_rect,
                        Some(tex),
                        widgets::ArtShape::Circle,
                    ),
                    None => {
                        mui.painter()
                            .circle_filled(photo_rect.center(), 12.0, p.surface_active);
                    }
                }
                let fg = widgets::mix(p.secondary, p.text, t);
                mui.painter().text(
                    photo_rect.right_center() + vec2(8.0, 0.0) + nudge,
                    egui::Align2::LEFT_CENTER,
                    &account.name,
                    theme::medium(13.0),
                    fg,
                );
                if current {
                    Icon::Check.image(p.accent, 15.0).paint_at(
                        mui,
                        Rect::from_center_size(
                            rect.right_center() - vec2(16.0, 0.0),
                            vec2(15.0, 15.0),
                        ),
                    );
                }
                if resp.hovered() {
                    mui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
            }
            if resp.clicked() && !current {
                choice = Some(Choice::Switch(account.page_id.clone()));
                mui.close();
            }
        }
        if let Some(id) = current_channel_id
            && widgets::menu_item(mui, &p, Icon::CircleUser, "View profile", None).clicked()
        {
            choice = Some(Choice::Profile(id));
            mui.close();
        }
        if widgets::menu_item(mui, &p, Icon::Settings, "Settings", None).clicked() {
            choice = Some(Choice::Settings);
            mui.close();
        }
        if widgets::menu_item(mui, &p, Icon::Info, "Keyboard shortcuts", None).clicked() {
            choice = Some(Choice::Help);
            mui.close();
        }
        if signed_in && widgets::menu_item(mui, &p, Icon::LogOut, "Sign out", None).clicked() {
            choice = Some(Choice::SignOut);
            mui.close();
        }
    });
    match choice {
        Some(Choice::LogIn) => app.sign_in(),
        Some(Choice::Profile(id)) => app.navigate(Route::Artist(id)),
        Some(Choice::Switch(page_id)) => {
            let ctx = ui.ctx().clone();
            app.update_settings(&ctx, |s| s.account_page_id = page_id);
            app.forget_account_data();
        }
        Some(Choice::Settings) => app.navigate(Route::Settings),
        Some(Choice::Help) => app.help_open = true,
        Some(Choice::SignOut) => app.backend.sign_out(),
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bug this fixes: the list opened with its first row highlighted, so Enter used the
    /// typed text while Down skipped the first suggestion entirely. Nothing is highlighted
    /// until an arrow key asks for it.
    #[test]
    fn the_first_down_reaches_the_first_suggestion() {
        assert_eq!(step_cursor(None, true, 3), Some(0));
        assert_eq!(step_cursor(Some(0), true, 3), Some(1));
        assert_eq!(step_cursor(Some(2), true, 3), Some(2), "the end holds");
    }

    #[test]
    fn up_walks_back_and_can_clear_the_highlight() {
        assert_eq!(
            step_cursor(None, false, 3),
            Some(2),
            "from nothing, the last"
        );
        assert_eq!(step_cursor(Some(1), false, 3), Some(0));
        assert_eq!(step_cursor(Some(0), false, 3), None);
    }

    #[test]
    fn an_empty_list_has_nothing_to_highlight() {
        assert_eq!(step_cursor(None, true, 0), None);
        assert_eq!(step_cursor(None, false, 0), None);
    }
}

#[cfg(test)]
mod search_box_tests {
    use super::*;
    use crate::search_history::SearchHistory;
    use egui::{Event, Key, Modifiers, PointerButton, Pos2};

    /// The search box alone in a headless `Context`, with the state `App` would lend it. A
    /// submit is applied the way `App::submit_search` and `poll_search` do: recorded, and noted
    /// in `submitted`.
    struct Harness {
        ctx: egui::Context,
        palette: theme::Palette,
        t: f64,
        query: String,
        suggestions: Vec<String>,
        suggestions_for: Option<String>,
        history: SearchHistory,
        cursor: Option<usize>,
        dismissed: bool,
        submitted: Vec<String>,
        saves: usize,
        last: BoxOutcome,
    }

    impl Harness {
        fn new(history: &[&str]) -> Self {
            let ctx = egui::Context::default();
            crate::theme::install_fonts(&ctx);
            let mut h = SearchHistory::default();
            for q in history.iter().rev() {
                h.record(q);
            }
            Self {
                ctx,
                palette: theme::Palette::dark(),
                t: 0.0,
                query: String::new(),
                suggestions: Vec::new(),
                suggestions_for: None,
                history: h,
                cursor: None,
                dismissed: false,
                submitted: Vec::new(),
                saves: 0,
                last: BoxOutcome::default(),
            }
        }

        fn pass(&mut self, events: Vec<Event>) -> &BoxOutcome {
            self.t += 1.0 / 60.0;
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(900.0, 700.0))),
                time: Some(self.t),
                events,
                ..Default::default()
            };
            let mut out = BoxOutcome::default();
            let p = self.palette;
            let mut state = BoxState {
                query: &mut self.query,
                suggestions: &self.suggestions,
                suggestions_for: self.suggestions_for.as_deref(),
                history: &mut self.history,
                cursor: &mut self.cursor,
                dismissed: &mut self.dismissed,
            };
            self.ctx
                .run_ui(input, |ui| {
                    let (rect, response) =
                        ui.allocate_exact_size(vec2(SEARCH_W, SEARCH_H), Sense::hover());
                    let inner = rect.shrink2(vec2(12.0, 0.0));
                    let focused = ui.memory(|m| m.has_focus(search_id()));
                    out = search_box(ui, &p, &response, inner, SEARCH_W, focused, &mut state);
                })
                .drop_without_applying_deltas();
            if out.clear_suggestions {
                self.suggestions.clear();
                self.suggestions_for = None;
                self.cursor = None;
            }
            if out.history_changed {
                self.saves += 1;
            }
            if out.submit && !self.query.trim().is_empty() {
                self.history.record(&self.query);
                self.submitted.push(self.query.clone());
            }
            self.last = out;
            &self.last
        }

        /// Focus the box and let the list settle: a popup's first frame only measures it.
        fn focus(&mut self) {
            self.ctx.memory_mut(|m| m.request_focus(search_id()));
            self.pass(vec![]);
            self.pass(vec![]);
        }

        fn click(&mut self, at: Pos2) {
            self.pass(vec![Event::PointerMoved(at)]);
            for pressed in [true, false] {
                self.pass(vec![Event::PointerButton {
                    pos: at,
                    button: PointerButton::Primary,
                    pressed,
                    modifiers: Modifiers::NONE,
                }]);
            }
            self.pass(vec![]);
        }

        fn key(&mut self, key: Key) {
            self.pass(vec![Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }]);
            self.pass(vec![]);
        }

        fn rows(&self) -> usize {
            self.last.rows.len()
        }
    }

    #[test]
    fn the_recent_list_opens_on_an_empty_focused_box_newest_first() {
        let mut h = Harness::new(&["mary j blige", "travis scott"]);
        h.pass(vec![]);
        assert_eq!(h.rows(), 0, "the list showed before the box was focused");
        h.focus();
        assert_eq!(h.rows(), 2, "the recent list did not open on focus");
        assert!(h.last.clear.is_some(), "no Clear button");
        assert_eq!(h.history.entries, ["mary j blige", "travis scott"]);
        // The dropdown ends at the last row: nothing under it but the 1 px outline.
        let gap = h.last.popup.unwrap().bottom() - h.last.rows[1].bottom();
        assert!(gap <= 1.5, "{gap} px under the last row");
        // Nothing recent, nothing to show.
        let mut empty = Harness::new(&[]);
        empty.focus();
        assert_eq!(empty.rows(), 0);
    }

    #[test]
    fn clicking_a_recent_search_runs_it_and_moves_it_to_the_top() {
        let mut h = Harness::new(&["mary j blige", "travis scott"]);
        h.focus();
        let second = h.last.rows[1].left_center() + vec2(60.0, 0.0);
        h.click(second);
        assert_eq!(h.submitted, ["travis scott"]);
        assert_eq!(h.query, "travis scott");
        assert_eq!(h.history.entries, ["travis scott", "mary j blige"]);
    }

    #[test]
    fn the_x_removes_one_without_searching_and_the_list_stays_open() {
        let mut h = Harness::new(&["a", "b", "c"]);
        h.focus();
        let x = h.last.removes[1].center();
        h.click(x);
        assert_eq!(h.history.entries, ["a", "c"]);
        assert!(h.submitted.is_empty(), "removing also ran the search");
        assert_eq!(h.saves, 1);
        assert_eq!(h.rows(), 2, "the list closed after a removal");
    }

    #[test]
    fn clear_empties_the_list_without_searching() {
        let mut h = Harness::new(&["a", "b"]);
        h.focus();
        let clear = h.last.clear.unwrap().center();
        h.click(clear);
        assert!(h.history.entries.is_empty());
        assert!(h.submitted.is_empty());
        assert_eq!(h.saves, 1);
        assert_eq!(h.rows(), 0);
    }

    #[test]
    fn down_and_enter_pick_the_newest_and_escape_hides_the_list() {
        let mut h = Harness::new(&["newest", "older"]);
        h.focus();
        h.key(Key::ArrowDown);
        h.key(Key::Enter);
        assert_eq!(h.submitted, ["newest"]);

        let mut h = Harness::new(&["newest", "older"]);
        h.focus();
        h.key(Key::Escape);
        assert_eq!(h.rows(), 0, "Escape did not hide the list");
        assert!(h.submitted.is_empty());
        // Typing brings autocomplete back into play and clears the dismissal.
        h.pass(vec![Event::Text("x".into())]);
        assert!(!h.dismissed);
    }

    #[test]
    fn enter_on_a_blank_box_searches_nothing() {
        // With recent searches on show and nothing highlighted.
        let mut h = Harness::new(&["travis scott"]);
        h.focus();
        h.key(Key::Enter);
        assert!(h.submitted.is_empty(), "blank Enter ran {:?}", h.submitted);
        assert!(!h.last.submit);
        // And with no list at all.
        let mut h = Harness::new(&[]);
        h.focus();
        h.query = "   ".into();
        h.key(Key::Enter);
        assert!(h.submitted.is_empty());
    }

    #[test]
    fn typing_swaps_the_recent_list_for_autocomplete() {
        let mut h = Harness::new(&["travis scott"]);
        h.focus();
        assert!(h.last.clear.is_some());
        h.pass(vec![Event::Text("ma".into())]);
        h.suggestions = vec!["mary j blige".into(), "madonna".into()];
        h.suggestions_for = Some("ma".into());
        h.pass(vec![]);
        h.pass(vec![]);
        assert_eq!(h.rows(), 2);
        assert!(
            h.last.clear.is_none(),
            "the recent heading showed over autocomplete"
        );
        assert!(
            h.last.removes.is_empty(),
            "autocomplete rows grew a remove button"
        );
    }

    #[test]
    fn a_long_list_scrolls_from_the_sixth_and_follows_the_arrows() {
        let names: Vec<String> = (0..15).map(|i| format!("search {i}")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let mut h = Harness::new(&names);
        h.focus();
        assert_eq!(h.rows(), 15);
        let rows = h.last.rows.clone();
        let view = h.last.list.unwrap();
        // Unscrolled, the heading sits the menu's usual 6 px under the top.
        let room = h.last.clear.unwrap().top() - h.last.popup.unwrap().top();
        assert!(room > 5.0, "{room} px above the heading");
        // Five rows and half the sixth are in view; the rest wait below.
        assert!(rows[4].bottom() <= view.bottom() + 0.5);
        assert!(rows[5].top() < view.bottom() && rows[5].bottom() > view.bottom());
        assert!(rows[6].top() >= view.bottom() - 0.5);
        // Arrowing down to the tenth brings it into view.
        for _ in 0..10 {
            h.key(Key::ArrowDown);
        }
        h.pass(vec![]);
        let tenth = h.last.rows[9];
        assert!(tenth.top() >= view.top() - 0.5 && tenth.bottom() <= view.bottom() + 0.5);
        // The heading went up with the rows rather than staying pinned.
        assert!(h.last.clear.unwrap().bottom() < view.top());
        // The list's top room scrolled away too: rows now run up to the outline.
        let top = h.last.popup.unwrap().top();
        assert!(
            view.top() - top <= 1.5,
            "{} px above the list",
            view.top() - top
        );
        // And a click where it now sits searches it.
        h.click(tenth.center());
        assert_eq!(h.submitted, ["search 9"]);
    }

    /// Where the field's caret is and what it has selected, as character indices.
    fn caret(h: &Harness) -> (usize, usize) {
        let range = egui::TextEdit::load_state(&h.ctx, search_id())
            .and_then(|s| s.cursor.char_range())
            .expect("the field has a caret");
        (range.primary.index.into(), range.secondary.index.into())
    }

    #[test]
    fn a_picked_suggestion_leaves_the_caret_at_its_end_with_nothing_selected() {
        for select_all in [false, true] {
            let mut h = Harness::new(&[]);
            h.focus();
            h.pass(vec![Event::Text("lil".into())]);
            if select_all {
                h.pass(vec![Event::Key {
                    key: Key::A,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::COMMAND,
                }]);
                assert_eq!(caret(&h), (3, 0), "Ctrl+A did not select the text");
            }
            h.suggestions = vec!["lil baby".into(), "lil uzi vert".into()];
            h.suggestions_for = Some("lil".into());
            h.pass(vec![]);
            h.pass(vec![]);
            h.key(Key::ArrowDown);
            h.key(Key::ArrowDown);
            h.key(Key::Enter);
            assert_eq!(h.submitted, ["lil uzi vert"]);
            assert_eq!(caret(&h), (12, 12), "select_all = {select_all}");
        }
    }
}
