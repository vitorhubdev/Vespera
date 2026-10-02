//! The left panel: the chat list.

use egui::{Frame, Margin, Rect, Sense, Vec2, pos2, vec2};

use crate::app::App;
use crate::model::{Action, Chat, Contact, Dialog, Message, Page};
use crate::theme::{self, Icon, Palette};

use super::widgets;

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let panel = egui::Panel::left("chats")
        .resizable(true)
        .default_size(app.settings.sidebar_width)
        .size_range(if theme::macos_chrome(ui.ctx()) {
            (theme::traffic_light_inset(ui.ctx()) + 210.0).max(280.0)..=520.0
        } else {
            260.0..=520.0
        })
        .show_separator_line(false)
        .frame(Frame::new().fill(palette.panel).inner_margin(Margin::ZERO));
    let response = panel.show(ui, |ui| {
        header(app, ui);
        list(app, ui);
    });
    let width = response.response.rect.width();
    if (width - app.settings.sidebar_width).abs() > 1.0 {
        app.settings.sidebar_width = width;
        app.actions.push(Action::SettingsChanged);
    }
    // Separate the panel from the conversation.
    let rect = response.response.rect;
    ui.painter().vline(
        rect.right(),
        rect.y_range(),
        egui::Stroke::new(1.0, palette.outline),
    );
}

fn header(app: &mut App, ui: &mut egui::Ui) {
    let _ = header_row(app, ui);
}

/// One action the header row can offer, in the order they keep their slot:
/// the first that does not fit goes to the overflow menu.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum HeaderAction {
    Settings,
    Status,
    Calls,
    Favorites,
    NewContact,
    ToggleSidebar,
}

impl HeaderAction {
    /// Priority order: what stays on the row while there is room.
    const PRIORITY: [HeaderAction; 5] = [
        HeaderAction::Settings,
        HeaderAction::Status,
        HeaderAction::Calls,
        HeaderAction::Favorites,
        HeaderAction::NewContact,
    ];

    fn icon(self) -> Icon {
        match self {
            HeaderAction::Settings => Icon::Settings,
            HeaderAction::Status => Icon::Image,
            HeaderAction::Calls => Icon::Phone,
            HeaderAction::Favorites => Icon::Star,
            HeaderAction::NewContact => Icon::SquarePen,
            HeaderAction::ToggleSidebar => Icon::PanelLeft,
        }
    }

    /// Menu name. Every string goes through the catalog so the translation
    /// round reaches it.
    fn label(self, language: crate::i18n::Language) -> String {
        let key = match self {
            HeaderAction::Settings => "chatlist.settings",
            HeaderAction::Status => "chatlist.status",
            HeaderAction::Calls => "chatlist.calls",
            HeaderAction::Favorites => "chatlist.favorites",
            HeaderAction::NewContact => "chatlist.new_contact",
            HeaderAction::ToggleSidebar => "chatlist.hide_list",
        };
        crate::i18n::t(language, key)
    }

    fn tooltip(self, language: crate::i18n::Language) -> String {
        let keys = match self {
            HeaderAction::ToggleSidebar => "Ctrl+B",
            HeaderAction::NewContact => "Ctrl+N",
            HeaderAction::Settings => "Ctrl+,",
            _ => "",
        };
        let label = self.label(language);
        if keys.is_empty() {
            label
        } else {
            format!("{label} ({})", super::keys::label(keys))
        }
    }

    /// What the button asks for when clicked.
    fn push(self, app: &mut App) {
        match self {
            HeaderAction::Settings => app.actions.push(Action::Open(Page::Settings)),
            HeaderAction::Status => app.actions.push(Action::Open(Page::Status)),
            HeaderAction::Calls => app.actions.push(Action::Open(Page::Calls)),
            HeaderAction::Favorites => app.actions.push(Action::Open(Page::Favorites)),
            HeaderAction::NewContact => app
                .actions
                .push(Action::ShowDialog(crate::model::Dialog::NewContact)),
            HeaderAction::ToggleSidebar => app.actions.push(Action::ToggleSidebar),
        }
    }
}

/// Width of one icon button: the icon plus its own padding, matching
/// `theme::icon_button`.
const HEADER_SLOT: f32 = 30.0;
/// The profile picture beside the title.
const HEADER_AVATAR: f32 = 34.0;
/// Gap between the picture and the title.
const HEADER_GAP: f32 = 6.0;
/// A title wider than this stops growing: long chat names and long titles do
/// not push the buttons around.
const HEADER_TITLE_MAX: f32 = 190.0;
/// Below this the title stops being worth the space and goes away before
/// anything overlaps.
const HEADER_TITLE_MIN: f32 = 36.0;

/// Where the header row puts things at one width. Pure geometry: the drawing
/// follows it, and the test checks it.
#[derive(Clone, Debug)]
struct HeaderLayout {
    /// Buttons on the row, left to right starting next to the title.
    inline: Vec<HeaderAction>,
    /// What the "⋯" button holds, in the same order.
    overflow: Vec<HeaderAction>,
    /// The list toggle never moves to the menu, so its space is reserved
    /// before anything else is measured.
    toggle: bool,
    avatar: Option<Rect>,
    title_width: f32,
    /// Where the row really put things, recorded while drawing. The test
    /// reads these instead of trusting the plan.
    drawn_avatar: Option<Rect>,
    drawn_title: Option<Rect>,
    drawn_buttons: Vec<Rect>,
}

/// Measures the row before anything is drawn. Widest that still leaves the
/// title its minimum width wins; with no such count, the title goes away and
/// the widest set that fits is taken.
fn plan_header(row: Rect, inset: f32, avatar: bool, archived: bool) -> HeaderLayout {
    let left = row.left()
        + inset
        + if avatar {
            HEADER_AVATAR + HEADER_GAP
        } else {
            0.0
        }
        // The archived row draws a back button where the avatar would be, and
        // the plan has to know: otherwise the row is one slot too optimistic
        // and the last action is clipped outside the panel.
        + if archived && !avatar {
            HEADER_SLOT + HEADER_GAP
        } else {
            0.0
        };
    let mut fallback: Option<HeaderLayout> = None;
    // Five actions down to one: each step moves the lowest-priority action
    // into the overflow menu, which costs one slot and keeps the rest.
    for keep in (1..=HeaderAction::PRIORITY.len()).rev() {
        let inline: Vec<HeaderAction> = HeaderAction::PRIORITY[..keep].to_vec();
        let overflow: Vec<HeaderAction> = HeaderAction::PRIORITY[keep..]
            .iter()
            .rev()
            .copied()
            .collect();
        let menu = keep < HeaderAction::PRIORITY.len();
        let slots = inline.len() + usize::from(menu) + 1;
        let free = row.right() - left - slots as f32 * HEADER_SLOT;
        let title_width = free.clamp(0.0, HEADER_TITLE_MAX);
        let plan = HeaderLayout {
            inline,
            overflow,
            toggle: true,
            avatar: avatar.then(|| {
                Rect::from_min_size(
                    pos2(row.left() + inset, row.center().y - HEADER_AVATAR / 2.0),
                    vec2(HEADER_AVATAR, HEADER_AVATAR),
                )
            }),
            title_width,
            drawn_avatar: None,
            drawn_title: None,
            drawn_buttons: Vec::new(),
        };
        if title_width >= HEADER_TITLE_MIN {
            return plan;
        }
        if fallback.is_none() && free >= 0.0 {
            fallback = Some(HeaderLayout {
                title_width: 0.0,
                ..plan
            });
        }
    }
    fallback.unwrap_or(HeaderLayout {
        inline: Vec::new(),
        overflow: HeaderAction::PRIORITY.to_vec(),
        toggle: true,
        avatar: avatar.then(|| {
            Rect::from_min_size(
                pos2(row.left() + inset, row.center().y - HEADER_AVATAR / 2.0),
                vec2(HEADER_AVATAR, HEADER_AVATAR),
            )
        }),
        title_width: 0.0,
        drawn_avatar: None,
        drawn_title: None,
        drawn_buttons: Vec::new(),
    })
}

/// Draws the panel header and hands back the rects it used, so the test can
/// prove that nothing covers anything at any width.
fn header_row(app: &mut App, ui: &mut egui::Ui) -> HeaderLayout {
    let macos = theme::macos_chrome(ui.ctx());
    let inset = if macos {
        theme::traffic_light_inset(ui.ctx())
    } else {
        0.0
    };
    let palette = app.palette;
    let language = app.settings.language;
    let archived = app.show_archived;
    let search = app.search.clone();
    let focus_search = app.focus_search;
    let mut click: Option<HeaderAction> = None;
    let mut go_back = false;
    let mut focus_done = false;
    let mut title = String::new();
    let mut plan = HeaderLayout {
        inline: Vec::new(),
        overflow: Vec::new(),
        toggle: false,
        avatar: None,
        title_width: 0.0,
        drawn_avatar: None,
        drawn_title: None,
        drawn_buttons: Vec::new(),
    };
    Frame::new()
        .inner_margin(if macos {
            Margin {
                left: 18,
                right: 14,
                top: 8,
                bottom: 8,
            }
        } else {
            Margin {
                left: 18,
                right: 10,
                top: 12,
                bottom: 8,
            }
        })
        .show(ui, |ui| {
            let mut row = ui.max_rect();
            if macos {
                row.max.y = row.min.y + 60.0;
                // The custom title bar needs a drag region: without this the
                // panel header is the only thing on top of the window and
                // the window cannot be moved from the sidebar.
                super::titlebar_drag(ui, row);
            } else {
                row.max.y = row.min.y + HEADER_AVATAR + 18.0;
            }
            plan = plan_header(row, inset, !macos && !archived, archived);
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.horizontal(|ui| {
                ui.set_min_height(row.height());
                if macos {
                    ui.add_space((inset - 14.0).max(0.0));
                }
                if archived {
                    if theme::icon_button(
                        ui,
                        Icon::ArrowLeft,
                        18.0,
                        palette.secondary,
                        palette.text,
                        &crate::i18n::t(language, "chatlist.back_to_chats"),
                    )
                    .clicked()
                    {
                        go_back = true;
                    }
                    title = crate::i18n::t(language, "chatlist.archived");
                } else {
                    if !macos {
                        let me = app.me.clone().unwrap_or_default();
                        let name = app.me_name.clone().unwrap_or_else(|| "You".to_owned());
                        let picture = app.avatar(&me);
                        let tooltip = match &app.me_about {
                            Some(about) => format!("{name}\n{about}"),
                            None => name.clone(),
                        };
                        let response = widgets::avatar(
                            ui,
                            &palette,
                            &name,
                            &me,
                            HEADER_AVATAR,
                            picture.as_deref(),
                        )
                        .interact(Sense::click())
                        .on_hover_text(tooltip)
                        .on_hover_cursor(egui::CursorIcon::PointingHand);
                        if response.clicked() {
                            app.actions.push(Action::Open(Page::Settings));
                        }
                        plan.avatar = Some(response.rect);
                        plan.drawn_avatar = Some(response.rect);
                        ui.add_space(HEADER_GAP);
                    }
                    title = crate::i18n::t(language, "chatlist.chats");
                }
                if plan.title_width > 0.0 {
                    // Truncation with the ellipsis the label already carries:
                    // a long title shortens before it ever reaches a button.
                    let title_response = ui.add_sized(
                        [plan.title_width, row.height()],
                        egui::Label::new(
                            egui::RichText::new(&title)
                                .font(theme::bold(if archived && macos { 16.0 } else { 20.0 }))
                                .color(palette.text),
                        )
                        .truncate()
                        .selectable(false),
                    );
                    plan.drawn_title = Some(title_response.rect);
                }
                // Measured space, so the row is laid out right to left by
                // hand: the toggle sits next to the title and the menu takes
                // what the hidden actions would have needed.
                let mut actions: Vec<HeaderAction> = Vec::new();
                if plan.toggle {
                    actions.push(HeaderAction::ToggleSidebar);
                }
                if !plan.overflow.is_empty() {
                    actions.push(HeaderAction::Settings);
                }
                actions.extend(plan.inline.iter().copied());
                let mut menu_button = None;
                for action in actions {
                    let icon = if action == HeaderAction::Settings && !plan.overflow.is_empty() {
                        Icon::Ellipsis
                    } else {
                        action.icon()
                    };
                    let tip = if icon == Icon::Ellipsis {
                        crate::i18n::t(language, "chatlist.more")
                    } else {
                        action.tooltip(language)
                    };
                    let response =
                        theme::icon_button(ui, icon, 18.0, palette.secondary, palette.text, &tip);
                    plan.drawn_buttons.push(response.rect);
                    if icon == Icon::Ellipsis {
                        menu_button = Some(response);
                    } else if response.clicked() {
                        click = Some(action);
                    }
                }
                if let Some(button) = menu_button {
                    let labels: Vec<String> = plan
                        .overflow
                        .iter()
                        .map(|action| action.label(language))
                        .collect();
                    let width = widgets::menu_width(
                        ui,
                        &labels.iter().map(String::as_str).collect::<Vec<_>>(),
                        true,
                    );
                    let mut picked = None;
                    egui::Popup::menu(&button)
                        .width(width)
                        .frame(widgets::menu_frame(&palette))
                        // Submitted again on every frame, which is what keeps
                        // an egui popup open: building it only on the clicked
                        // frame closes it before the user reaches an entry.
                        .show(|ui| {
                            for action in &plan.overflow {
                                if widgets::menu_item(
                                    ui,
                                    &palette,
                                    Some(action.icon()),
                                    &action.label(language),
                                ) {
                                    picked = Some(*action);
                                }
                            }
                        });
                    if picked.is_some() {
                        click = picked;
                    }
                }
            });
            ui.add_space(6.0);
            let id = egui::Id::new("chat-search");
            let width = ui.available_width();
            let mut text = search.clone();
            let response = widgets::search_field(
                ui,
                &palette,
                id,
                &mut text,
                &crate::i18n::t(language, "chatlist.search"),
                width,
            );
            if text != search {
                app.actions.push(Action::Search(text));
            }
            if focus_search {
                response.request_focus();
                // The request is one-shot: without clearing it, every later
                // frame steals the focus back from whatever the user clicked.
                focus_done = true;
            }
        });
    if go_back {
        app.show_archived = false;
    }
    if focus_done {
        app.focus_search = false;
    }
    if let Some(action) = click {
        action.push(app);
    }
    plan
}

fn list(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    if !app.search.trim().is_empty() {
        results(app, ui);
        return;
    }
    if !app.show_archived {
        Frame::new()
            .inner_margin(Margin {
                left: 14,
                right: 14,
                top: 8,
                bottom: 0,
            })
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui.selectable_label(!app.show_channels, "Chats").clicked() {
                        app.show_channels = false;
                    }
                    if ui.selectable_label(app.show_channels, "Channels").clicked() {
                        app.show_channels = true;
                    }
                });
            });
        ui.add_space(6.0);
    }
    let chats: Vec<Chat> = app.visible_chats().into_iter().cloned().collect();
    let archived = app.archived_count();
    let show_archive_row = !app.show_archived && archived > 0;
    if chats.is_empty() && !show_archive_row {
        let (title, body) = if app.show_archived {
            ("Nothing archived", "Archived chats appear here.")
        } else if app.syncing {
            ("Loading your chats", "Receiving history from your phone.")
        } else {
            (
                "No chats yet",
                "New chats appear here. You can start one from your phone.",
            )
        };
        widgets::empty_state(ui, &palette, Icon::MessageCircle, title, body);
        return;
    }
    if !chats.is_empty() {
        crate::timing::milestone("chat list first frame");
    }
    let row_height = theme::ROW_HEIGHT;
    let total = chats.len() + usize::from(show_archive_row);
    let mut scroll_area = egui::ScrollArea::vertical()
        .id_salt("chat-list")
        .auto_shrink([false, false]);
    let target_row = app.scroll_chat_into_view.as_ref().and_then(|target| {
        chats
            .iter()
            .position(|chat| chat.id == *target)
            .map(|index| index + usize::from(show_archive_row))
    });
    if let Some(target_row) = target_row {
        let id = ui.make_persistent_id(egui::IdSalt::new("chat-list"));
        let current = egui::scroll_area::State::load(ui.ctx(), id)
            .unwrap_or_default()
            .offset
            .y;
        let offset = row_scroll_offset(
            current,
            ui.available_height(),
            target_row,
            row_height,
            ui.spacing().item_spacing.y,
        );
        scroll_area = scroll_area.vertical_scroll_offset(offset);
        app.scroll_chat_into_view = None;
    }
    scroll_area.show_rows(ui, row_height, total, |ui, range| {
        for index in range {
            if show_archive_row && index == 0 {
                archive_row(app, ui, archived);
                continue;
            }
            let chat = &chats[index - usize::from(show_archive_row)];
            // Key by chat so an open menu survives list reordering.
            ui.push_id(("chat", &chat.id), |ui| row(app, ui, chat));
        }
    });
}

/// Returns the smallest offset that fully reveals a fixed-height row.
fn row_scroll_offset(
    current: f32,
    viewport_height: f32,
    row: usize,
    row_height: f32,
    spacing: f32,
) -> f32 {
    let top = row as f32 * (row_height + spacing);
    let bottom = top + row_height;
    if top < current {
        top
    } else if bottom > current + viewport_height {
        (bottom - viewport_height).max(0.0)
    } else {
        current
    }
}

/// Search results grouped into chats, messages, and contacts.
fn results(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let chats: Vec<Chat> = app.visible_chats().into_iter().cloned().collect();
    let hits: Vec<Message> = app.search_hits.clone();
    let contacts: Vec<Contact> = app.matching_contacts().into_iter().cloned().collect();
    if chats.is_empty() && hits.is_empty() && contacts.is_empty() {
        widgets::empty_state(
            ui,
            &palette,
            Icon::Search,
            "No results",
            "Try another name, number, or message text.",
        );
        return;
    }
    egui::ScrollArea::vertical()
        .id_salt("search-results")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if !chats.is_empty() {
                section(ui, &palette, "Chats");
                for chat in &chats {
                    let reveal = app.scroll_chat_into_view.as_deref() == Some(chat.id.as_str());
                    let response = ui
                        .push_id(("chat", &chat.id), |ui| row(app, ui, chat))
                        .inner;
                    if reveal {
                        response.scroll_to_me(None);
                        app.scroll_chat_into_view = None;
                    }
                }
            }
            if !hits.is_empty() {
                section(ui, &palette, "Messages");
                for hit in &hits {
                    ui.push_id(("hit", &hit.chat, &hit.id), |ui| hit_row(app, ui, hit));
                }
            }
            if !contacts.is_empty() {
                section(ui, &palette, "Contacts");
                for contact in &contacts {
                    ui.push_id(("contact", &contact.id), |ui| contact_row(app, ui, contact));
                }
            }
            ui.add_space(8.0);
        });
}

fn section(ui: &mut egui::Ui, palette: &Palette, label: &str) {
    ui.add_space(10.0);
    Frame::new()
        .inner_margin(Margin {
            left: 14,
            right: 14,
            top: 0,
            bottom: 4,
        })
        .show(ui, |ui| {
            theme::text(ui, label, theme::semibold(12.5), palette.accent);
        });
}

/// A message search result. Clicking it opens the chat at that message.
fn hit_row(app: &mut App, ui: &mut egui::Ui, hit: &Message) {
    let palette = app.palette;
    let title = match app.chat(&hit.chat) {
        Some(chat) => app.chat_title(&chat.clone()),
        None => app.display_name_or(&hit.chat, None),
    };
    let (rect, response) = ui.allocate_exact_size(
        vec2(ui.available_width(), theme::ROW_HEIGHT),
        Sense::click(),
    );
    if ui.is_rect_visible(rect) {
        if response.hovered() {
            ui.painter().rect_filled(rect, 0.0, palette.surface_hover);
        }
        let avatar_rect =
            Rect::from_center_size(pos2(rect.left() + 38.0, rect.center().y), Vec2::splat(48.0));
        let picture = app.avatar(&hit.chat);
        widgets::paint_avatar(
            ui,
            &palette,
            avatar_rect,
            &title,
            &hit.chat,
            picture.as_deref(),
        );
        let left = rect.left() + 76.0;
        let right = rect.right() - 14.0;
        let stamp_galley = ui.painter().layout_no_wrap(
            crate::util::chat_stamp(hit.timestamp),
            theme::regular(11.5),
            palette.dim,
        );
        let name_top = rect.top() + 14.0;
        ui.painter().galley(
            pos2(right - stamp_galley.size().x, name_top + 1.0),
            stamp_galley.clone(),
            palette.dim,
        );
        let name_width = (right - stamp_galley.size().x - 8.0 - left).max(0.0);
        let name = widgets::line(ui, &title, theme::medium(14.5), palette.text, name_width, 1);
        name.paint(ui, pos2(left, name_top), palette.text);
        // Show the sender for group messages.
        let line_y = rect.top() + 38.0;
        let mut x = left;
        if hit.from_me {
            let who = widgets::line(
                ui,
                "You: ",
                theme::regular(13.0),
                palette.dim,
                (right - x) * 0.5,
                1,
            );
            who.paint(ui, pos2(x, line_y), palette.dim);
            x += who.size().x;
        } else if crate::model::ChatKind::from_id(&hit.chat) == crate::model::ChatKind::Group {
            let sender = app.display_name_or(&hit.sender, hit.sender_name.as_deref());
            let first = sender.split_whitespace().next().unwrap_or(&sender);
            let who = widgets::line(
                ui,
                &format!("{first}: "),
                theme::regular(13.0),
                palette.dim,
                (right - x) * 0.5,
                1,
            );
            who.paint(ui, pos2(x, line_y), palette.dim);
            x += who.size().x;
        }
        let words = widgets::line(
            ui,
            &crate::markup::plain(&app.resolve_mention_tokens(&hit.summary()), &[]),
            theme::regular(13.0),
            palette.dim,
            (right - x).max(0.0),
            1,
        );
        words.paint(ui, pos2(x, line_y), palette.dim);
        ui.painter().hline(
            left..=rect.right(),
            rect.bottom() - 0.5,
            egui::Stroke::new(1.0, palette.outline),
        );
    }
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    if response.clicked() {
        app.actions.push(Action::OpenMessage {
            chat: hit.chat.clone(),
            message: hit.id.clone(),
        });
    }
}

/// A contact without a chat. Clicking starts one.
fn contact_row(app: &mut App, ui: &mut egui::Ui, contact: &Contact) {
    let palette = app.palette;
    let name = contact
        .display_name()
        .map(str::to_owned)
        .unwrap_or_else(|| app.display_name_or(&contact.id, None));
    let (rect, response) = ui.allocate_exact_size(
        vec2(ui.available_width(), theme::ROW_HEIGHT),
        Sense::click(),
    );
    if ui.is_rect_visible(rect) {
        if response.hovered() {
            ui.painter().rect_filled(rect, 0.0, palette.surface_hover);
        }
        let avatar_rect =
            Rect::from_center_size(pos2(rect.left() + 38.0, rect.center().y), Vec2::splat(48.0));
        let picture = app.avatar(&contact.id);
        widgets::paint_avatar(
            ui,
            &palette,
            avatar_rect,
            &name,
            &contact.id,
            picture.as_deref(),
        );
        let left = rect.left() + 76.0;
        let name_line = widgets::line(
            ui,
            &name,
            theme::medium(14.5),
            palette.text,
            rect.right() - 14.0 - left,
            1,
        );
        name_line.paint(ui, pos2(left, rect.top() + 14.0), palette.text);
        if let Some(phone) = crate::model::phone_of(&contact.id) {
            let phone_line = widgets::line(
                ui,
                &crate::util::phone(phone),
                theme::regular(13.0),
                palette.dim,
                rect.right() - 14.0 - left,
                1,
            );
            phone_line.paint(ui, pos2(left, rect.top() + 38.0), palette.dim);
        }
        ui.painter().hline(
            left..=rect.right(),
            rect.bottom() - 0.5,
            egui::Stroke::new(1.0, palette.outline),
        );
    }
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    if response.clicked() {
        app.actions.push(Action::StartChat {
            id: contact.id.clone(),
            name,
        });
    }
}

fn archive_row(app: &mut App, ui: &mut egui::Ui, count: usize) {
    let palette = app.palette;
    let (rect, response) = ui.allocate_exact_size(
        vec2(ui.available_width(), theme::ROW_HEIGHT),
        Sense::click(),
    );
    if ui.is_rect_visible(rect) {
        if response.hovered() {
            ui.painter().rect_filled(rect, 0.0, palette.surface_hover);
        }
        let icon_rect =
            Rect::from_center_size(pos2(rect.left() + 38.0, rect.center().y), Vec2::splat(22.0));
        Icon::Archive
            .image(palette.accent, 22.0)
            .paint_at(ui, icon_rect);
        ui.painter().text(
            pos2(rect.left() + 76.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            "Archived",
            theme::medium(14.5),
            palette.text,
        );
        ui.painter().text(
            pos2(rect.right() - 16.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            count.to_string(),
            theme::regular(12.5),
            palette.accent,
        );
        ui.painter().hline(
            (rect.left() + 76.0)..=rect.right(),
            rect.bottom() - 0.5,
            egui::Stroke::new(1.0, palette.outline),
        );
    }
    if response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
    {
        app.show_archived = true;
    }
}

fn row(app: &mut App, ui: &mut egui::Ui, chat: &Chat) -> egui::Response {
    let palette = app.palette;
    let title = app.chat_title(chat);
    let selected = app.open_chat.as_deref() == Some(chat.id.as_str());
    let now = crate::util::now();
    let muted = chat.muted(now);
    let (rect, response) = ui.allocate_exact_size(
        vec2(ui.available_width(), theme::ROW_HEIGHT),
        Sense::click(),
    );
    // The preview area and the whole last message, when the row cuts it short.
    let mut full_preview: Option<(Rect, String, String)> = None;
    if ui.is_rect_visible(rect) {
        if selected {
            ui.painter().rect_filled(rect, 0.0, palette.surface_active);
        } else if response.hovered() {
            ui.painter().rect_filled(rect, 0.0, palette.surface_hover);
        }
        let avatar_rect =
            Rect::from_center_size(pos2(rect.left() + 38.0, rect.center().y), Vec2::splat(48.0));
        let picture = app.avatar(&chat.id);
        widgets::paint_avatar(
            ui,
            &palette,
            avatar_rect,
            &title,
            &chat.id,
            picture.as_deref(),
        );
        if chat.ephemeral_expiration.is_some() {
            widgets::paint_disappearing_badge(ui, &palette, avatar_rect);
        }

        let left = rect.left() + 76.0;
        let right = rect.right() - 14.0;
        let stamp = if chat.last_activity > 0 {
            crate::util::chat_stamp(chat.last_activity)
        } else {
            String::new()
        };
        let unread = chat.unread > 0;
        let stamp_color = if unread && !muted {
            palette.accent
        } else {
            palette.dim
        };
        let stamp_galley = ui
            .painter()
            .layout_no_wrap(stamp, theme::regular(11.5), stamp_color);
        let name_top = rect.top() + 14.0;
        ui.painter().galley(
            pos2(right - stamp_galley.size().x, name_top + 1.0),
            stamp_galley.clone(),
            stamp_color,
        );
        let name_width = (right - stamp_galley.size().x - 8.0 - left).max(0.0);
        let name_font = if unread {
            theme::semibold(14.5)
        } else {
            theme::medium(14.5)
        };
        let name = widgets::line(ui, &title, name_font, palette.text, name_width, 1);
        name.paint(ui, pos2(left, name_top), palette.text);

        // Leave room for badges beside the latest-message preview.
        let mut badge_right = right;
        let line_y = rect.top() + 38.0;
        if unread {
            let width = widgets::badge(
                ui,
                &palette,
                pos2(badge_right - 10.0, line_y + 8.0),
                chat.unread,
                muted,
            );
            badge_right -= width + 6.0;
        }
        if muted {
            let icon_rect =
                Rect::from_center_size(pos2(badge_right - 8.0, line_y + 8.0), Vec2::splat(15.0));
            Icon::VolumeX
                .image(palette.dim, 15.0)
                .paint_at(ui, icon_rect);
            badge_right -= 20.0;
        }
        if chat.pinned {
            let icon_rect =
                Rect::from_center_size(pos2(badge_right - 8.0, line_y + 8.0), Vec2::splat(14.0));
            Icon::Pin.image(palette.dim, 14.0).paint_at(ui, icon_rect);
            badge_right -= 20.0;
        }
        let mut x = left;
        let typing = app.typing_in(&chat.id);
        let preview_color = if unread && !muted {
            palette.secondary
        } else {
            palette.dim
        };
        let preview = if !typing.is_empty() {
            let who = if chat.is_group() {
                format!("{} is typing…", typing[0].1.trim_start_matches('~'))
            } else {
                "typing…".to_owned()
            };
            widgets::line(
                ui,
                &who,
                theme::medium(13.0),
                palette.accent,
                badge_right - x,
                1,
            )
        } else if let Some(last) = &chat.last {
            let mut prefix = String::new();
            if last.from_me {
                let tick_rect =
                    Rect::from_center_size(pos2(x + 8.0, line_y + 8.0), Vec2::splat(16.0));
                widgets::ticks(ui, &palette, tick_rect, last.status);
                x += 20.0;
            } else if chat.is_group() {
                let sender = app.display_name_or(&last.sender, last.sender_name.as_deref());
                let first = sender.split_whitespace().next().unwrap_or(&sender);
                prefix = format!("{first}: ");
                let sender = widgets::line(
                    ui,
                    &prefix,
                    theme::regular(13.0),
                    preview_color,
                    (badge_right - x) * 0.5,
                    1,
                );
                let width = sender.size().x;
                sender.paint(ui, pos2(x, line_y), preview_color);
                x += width;
            }
            let words = widgets::line(
                ui,
                &crate::markup::plain(&app.resolve_mention_tokens(&last.summary), &[]),
                theme::regular(13.0),
                preview_color,
                (badge_right - x).max(0.0),
                1,
            );
            // The row shows one line: offer the whole message when that line
            // was cut short or the message has more lines than it.
            if words.galley.elided || last.full.trim_end() != last.summary {
                let area = Rect::from_min_max(
                    pos2(left, line_y - 4.0),
                    pos2(badge_right, line_y + words.size().y.max(16.0) + 4.0),
                );
                full_preview = Some((area, prefix, last.full.clone()));
            }
            words
        } else {
            widgets::line(ui, "", theme::regular(13.0), preview_color, 1.0, 1)
        };
        preview.paint(ui, pos2(x, line_y), preview_color);
        ui.painter().hline(
            left..=rect.right(),
            rect.bottom() - 0.5,
            egui::Stroke::new(1.0, palette.outline),
        );
    }
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    if let Some((area, prefix, full)) = full_preview {
        full_preview_tooltip(app, ui, &chat.id, area, &prefix, &full);
    }
    if response.clicked() {
        app.actions.push(Action::OpenChat(chat.id.clone()));
    }
    let menu_palette = palette;
    egui::Popup::context_menu(&response)
        .frame(widgets::menu_frame(&menu_palette))
        .show(|ui| {
            ui.set_min_width(190.0);
            context_menu(app, ui, chat, &menu_palette);
        });
    response
}

/// The widest a chat row's full-message tooltip grows before it wraps.
const FULL_PREVIEW_WIDTH: f32 = 360.0;
/// Lines a full-message tooltip shows before it ends in an ellipsis.
const FULL_PREVIEW_ROWS: usize = 12;
/// Characters of a message the tooltip lays out: far more than its rows
/// hold, so a very long message costs no more than a long one.
const FULL_PREVIEW_CHARS: usize = 2_000;

/// Where a chat row's latest-message preview sits, for hover tests.
pub fn preview_id(chat: &str) -> egui::Id {
    egui::Id::new(("chat-preview", chat))
}

/// Shows the whole last message while the pointer rests on a chat row's
/// cut-short preview, as WhatsApp Web does. It keeps out of the way of an
/// open menu and of a drag.
fn full_preview_tooltip(
    app: &App,
    ui: &egui::Ui,
    chat: &str,
    area: Rect,
    prefix: &str,
    full: &str,
) {
    let hover = ui.interact(area, preview_id(chat), Sense::hover());
    if egui::Popup::is_any_open(ui.ctx()) || ui.ctx().dragged_id().is_some() {
        return;
    }
    egui::Tooltip::for_enabled(&hover)
        .width(FULL_PREVIEW_WIDTH)
        .show(|ui| {
            let full: String = full.chars().take(FULL_PREVIEW_CHARS).collect();
            let text = format!(
                "{prefix}{}",
                crate::markup::plain(&app.resolve_mention_tokens(&full), &[])
            );
            let color = ui.visuals().text_color();
            let line = widgets::line(
                ui,
                text.trim_end(),
                theme::regular(13.0),
                color,
                FULL_PREVIEW_WIDTH,
                FULL_PREVIEW_ROWS,
            );
            let (rect, _) = ui.allocate_exact_size(line.size(), Sense::hover());
            line.paint(ui, rect.min, color);
        });
}

fn context_menu(app: &mut App, ui: &mut egui::Ui, chat: &Chat, palette: &Palette) {
    if chat.unread > 0 && widgets::menu_item(ui, palette, Some(Icon::CheckCheck), "Mark as read") {
        app.actions.push(Action::MarkRead(chat.id.clone()));
    }
    if widgets::menu_item(
        ui,
        palette,
        Some(if chat.pinned { Icon::PinOff } else { Icon::Pin }),
        if chat.pinned { "Unpin" } else { "Pin to top" },
    ) {
        app.actions
            .push(Action::SetPinned(chat.id.clone(), !chat.pinned));
    }
    if widgets::menu_item(
        ui,
        palette,
        Some(Icon::Archive),
        if chat.archived {
            "Unarchive"
        } else {
            "Archive"
        },
    ) {
        app.actions
            .push(Action::SetArchived(chat.id.clone(), !chat.archived));
    }
    let now = crate::util::now();
    if chat.muted(now) {
        if widgets::menu_item(ui, palette, Some(Icon::Bell), "Unmute") {
            app.actions.push(Action::SetMuted(chat.id.clone(), None));
        }
    } else {
        for (label, until) in [
            ("Mute for 8 hours", Some(now + 8 * 3600)),
            ("Mute for a week", Some(now + 7 * 86_400)),
            ("Mute indefinitely", Some(0)),
        ] {
            if widgets::menu_item(ui, palette, Some(Icon::BellOff), label) {
                app.actions.push(Action::SetMuted(chat.id.clone(), until));
            }
        }
    }
    widgets::menu_separator(ui, palette);
    if let Some(phone) = chat.phone()
        && widgets::menu_item(ui, palette, Some(Icon::Copy), "Copy number")
    {
        app.actions.push(Action::CopyText(format!("+{phone}")));
    }
    if widgets::menu_item(ui, palette, Some(Icon::Info), "Info") {
        app.actions
            .push(Action::ShowDialog(Dialog::ChatInfo(chat.id.clone())));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::AppDirs;
    use crate::settings::Settings;

    /// Renders the header at one width and hands back what it drew. macOS
    /// chrome is previewed the way the demo does, so both rows are covered.
    fn header_at(width: f32, macos: bool) -> HeaderLayout {
        header_state(width, macos, false)
    }

    /// The header as drawn at one width, optionally in the archived row.
    fn header_state(width: f32, macos: bool, archived: bool) -> HeaderLayout {
        let root = std::env::temp_dir().join(format!(
            "vespera-header-{}-{macos}-{archived}-{width}",
            std::process::id()
        ));
        let (mut app, _events) = App::headless(AppDirs::under(&root), Settings::default());
        app.show_archived = archived;
        let ctx = egui::Context::default();
        // The row paints text, so the fonts the app uses must be bound first.
        app.attach(&ctx);
        let mut layout = None;
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(width, 480.0))),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            if macos {
                ctx.data_mut(|data| {
                    data.insert_temp(egui::Id::new("macos-preview"), true);
                });
            }
            layout = Some(header_row(&mut app, ui));
        });
        output.textures_delta.clear();
        layout.expect("the header draws every frame")
    }

    /// Two rects cross when they share more than a hair of surface. Buttons
    /// sit side by side and touch, and that is not covering.
    fn crosses(first: Rect, second: Rect) -> bool {
        let overlap = |a: f32, b: f32, c: f32, d: f32| (b.min(d) - a.max(c)).max(0.0);
        overlap(first.left(), first.right(), second.left(), second.right()) > 0.5
            && overlap(first.top(), first.bottom(), second.top(), second.bottom()) > 0.5
    }

    #[test]
    fn the_header_never_lets_one_thing_cover_another() {
        // The owner's window is about 370 wide, where the icons used to be
        // drawn on top of the picture and the title. Nothing may cross at
        // any width, on either row.
        for width in [320.0f32, 370.0, 480.0, 800.0] {
            for macos in [false, true] {
                let layout = header_at(width, macos);
                let label = format!("{width}px macos={macos}");
                let buttons = &layout.drawn_buttons;
                assert!(
                    !buttons.is_empty(),
                    "the list toggle always keeps a slot: {label}"
                );
                for (index, first) in buttons.iter().enumerate() {
                    for second in &buttons[index + 1..] {
                        assert!(
                            !crosses(*first, *second),
                            "two buttons overlap at {label}: {first:?} {second:?}"
                        );
                    }
                }
                if let Some(title) = layout.drawn_title {
                    for button in buttons {
                        assert!(
                            !crosses(title, *button),
                            "the title touches a button at {label}: {title:?} {button:?}"
                        );
                    }
                    assert!(
                        title.width() <= HEADER_TITLE_MAX + 0.5,
                        "the title keeps its measured width at {label}"
                    );
                }
                if let Some(avatar) = layout.drawn_avatar {
                    for button in buttons {
                        assert!(
                            !crosses(avatar, *button),
                            "a button covers the picture at {label}: {avatar:?} {button:?}"
                        );
                    }
                    if let Some(title) = layout.drawn_title {
                        assert!(
                            !crosses(avatar, title),
                            "the title covers the picture at {label}"
                        );
                    }
                }
                for button in buttons {
                    assert!(
                        button.right() <= width + 0.5 && button.left() >= -0.5,
                        "a button leaves the panel at {label}: {button:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_overflow_menu_appears_only_when_space_runs_out() {
        // Wide enough: every action on the row and no menu. Narrow: the
        // lowest-priority ones move into the menu, keeping the priority
        // order, and the list toggle never moves.
        let wide = header_at(800.0, false);
        assert_eq!(wide.inline.len(), HeaderAction::PRIORITY.len());
        assert!(wide.overflow.is_empty(), "no menu when there is room");
        assert!(wide.drawn_title.is_some(), "the title has its space");

        for width in [320.0f32, 370.0, 480.0] {
            let narrow = header_at(width, false);
            let label = format!("{width}px");
            assert!(
                narrow.inline.len() <= HeaderAction::PRIORITY.len(),
                "never more actions than there are: {label}"
            );
            // Inline keeps the head of the priority list, the menu the tail.
            assert_eq!(
                narrow.inline,
                HeaderAction::PRIORITY[..narrow.inline.len()],
                "the kept actions stay in priority order: {label}"
            );
            assert_eq!(
                narrow.overflow,
                HeaderAction::PRIORITY[narrow.inline.len()..]
                    .iter()
                    .rev()
                    .copied()
                    .collect::<Vec<_>>(),
                "the menu holds what no longer fits: {label}"
            );
            // One button for the menu itself, on top of the kept actions and
            // the toggle.
            let expected = narrow.inline.len()
                + usize::from(!narrow.overflow.is_empty())
                + usize::from(narrow.toggle);
            assert_eq!(
                narrow.drawn_buttons.len(),
                expected,
                "one button per kept action: {label}"
            );
        }
        // 370 is the owner's window: the title still gets room there.
        let owner = header_at(370.0, false);
        assert!(owner.drawn_title.is_some(), "the title survives at 370");
    }

    #[test]
    fn the_archived_row_keeps_its_back_button_inside_the_panel() {
        // The archived row draws a back button where the avatar would be. If
        // the plan forgets it, the row is one slot too optimistic and the
        // last action ends up outside the panel at narrow widths.
        for width in [260.0f32, 320.0, 370.0, 480.0, 800.0] {
            for macos in [false, true] {
                let layout = header_state(width, macos, true);
                let label = format!("{width}px macos={macos} archived");
                assert!(
                    !layout.drawn_buttons.is_empty(),
                    "the toggle always keeps a slot: {label}"
                );
                for button in &layout.drawn_buttons {
                    assert!(
                        button.right() <= width + 0.5,
                        "a button leaves the panel at {label}: {button:?}"
                    );
                    assert!(
                        button.left() >= -0.5,
                        "a button starts before the panel at {label}: {button:?}"
                    );
                }
                if let Some(title) = layout.drawn_title {
                    for button in &layout.drawn_buttons {
                        assert!(
                            !crosses(title, *button),
                            "the title touches a button at {label}: {title:?} {button:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn alt_navigation_scrolls_the_destination_chat_into_view() {
        let root = std::env::temp_dir().join(format!(
            "vespera-chat-list-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let (mut app, _events) = App::headless(AppDirs::under(&root), Settings::default());
        let mut first = String::new();
        let mut last = String::new();
        for index in 0..24 {
            let id = format!("49170000{index:04}@s.whatsapp.net");
            let mut chat = Chat::new(id.clone(), format!("Chat {index:02}"));
            chat.last_activity = 100 - i64::from(index);
            if index == 0 {
                first.clone_from(&id);
            }
            last.clone_from(&id);
            app.chats.push(chat);
        }
        app.open_chat = Some(first);

        let ctx = egui::Context::default();
        app.attach(&ctx);
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(360.0, 240.0))),
            events: vec![egui::Event::Key {
                key: egui::Key::ArrowUp,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::ALT,
            }],
            ..Default::default()
        };
        let mut offset = 0.0;
        let mut output = ctx.run_ui(input, |ui| {
            super::super::keys::handle(&mut app, ui.ctx());
            let scroll_id = ui.make_persistent_id(egui::IdSalt::new("chat-list"));
            list(&mut app, ui);
            offset = egui::scroll_area::State::load(ui.ctx(), scroll_id)
                .expect("chat-list scroll state")
                .offset
                .y;
        });
        output.textures_delta.clear();

        assert!(
            app.actions.contains(&Action::OpenChat(last)),
            "Alt+Up wraps to the last visible chat"
        );
        assert!(app.scroll_chat_into_view.is_none(), "reveal was consumed");
        assert!(offset > 0.0, "the list moved down to reveal the last row");
    }
}
