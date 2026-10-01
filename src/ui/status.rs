//! Status updates from the last 24 hours.

use egui::{Color32, CornerRadius, Frame, Margin, Rect, RichText, ScrollArea, Sense, Stroke, vec2};

use crate::app::App;
use crate::model::{Action, Page};
use crate::stories::{self, Privacy, Story, StoryKind, StoryView};
use crate::theme::{self, Icon};
use crate::ui::widgets;

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let locale =
        crate::i18n::message_locale_tag(crate::i18n::message_locale(app.settings.language));
    if app.story_view.is_some() {
        show_viewer(app, ui, locale);
    } else {
        show_list(app, ui, locale);
    }
    wake_for_expiry(ui, &app.stories);
}

fn show_list(app: &mut App, ui: &mut egui::Ui, locale: &str) {
    let palette = app.palette;
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.add_space(12.0);
        if theme::icon_button(
            ui,
            Icon::ArrowLeft,
            18.0,
            palette.secondary,
            palette.text,
            stories::phrase(locale, "back"),
        )
        .clicked()
        {
            app.actions.push(Action::Open(Page::Chats));
        }
        theme::text(
            ui,
            stories::phrase(locale, "title"),
            theme::bold(20.0),
            palette.text,
        );
    });
    ui.add_space(8.0);
    let groups = stories::groups(&app.stories, crate::util::now());
    let height = (ui.available_height() - 220.0).max(80.0);
    if groups.is_empty() {
        ui.add_space(24.0);
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            widgets::rich_text(
                ui,
                stories::phrase(locale, "empty"),
                theme::regular(14.0),
                palette.secondary,
            );
        });
    } else {
        ScrollArea::vertical()
            .max_height(height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for group in &groups {
                    let name = stories::contact_label(
                        &group.stories,
                        &group.name,
                        stories::phrase(locale, "you"),
                    )
                    .to_owned();
                    let sender = group.sender.clone();
                    let unseen = group.unseen;
                    let preview = group
                        .stories
                        .last()
                        .map(|story| story.preview())
                        .unwrap_or_default();
                    let response = ui
                        .push_id(&sender, |ui| {
                            ui.horizontal(|ui| {
                                ui.add_space(12.0);
                                let picture = app.avatar(&sender);
                                widgets::avatar(
                                    ui,
                                    &palette,
                                    &name,
                                    &sender,
                                    36.0,
                                    picture.as_deref(),
                                );
                                ui.add_space(8.0);
                                ui.vertical(|ui| {
                                    let weight = if unseen > 0 {
                                        theme::bold(14.0)
                                    } else {
                                        theme::regular(14.0)
                                    };
                                    widgets::rich_text(ui, &name, weight, palette.text);
                                    widgets::rich_text(
                                        ui,
                                        &preview,
                                        theme::regular(12.5),
                                        palette.secondary,
                                    );
                                });
                            });
                        })
                        .response;
                    if response.interact(Sense::click()).clicked() {
                        let index = group
                            .stories
                            .iter()
                            .position(|story| !story.seen)
                            .unwrap_or(0);
                        app.story_view = Some(StoryView { sender, index });
                        app.actions.push(Action::StoryStep(0));
                    }
                }
            });
    }
    ui.add_space(8.0);
    composer(app, ui, locale);
}

fn composer(app: &mut App, ui: &mut egui::Ui, locale: &str) {
    let palette = app.palette;
    Frame::new()
        .fill(palette.surface)
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.label(RichText::new(stories::phrase(locale, "post")).color(palette.secondary));
            let text = &mut app.status_draft.text;
            ui.add(
                egui::TextEdit::multiline(text)
                    .desired_rows(2)
                    .hint_text(stories::phrase(locale, "text")),
            );
            ui.horizontal(|ui| {
                for background in stories::BACKGROUNDS {
                    let (rect, response) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
                    ui.painter().rect_filled(rect, 6.0, argb(background));
                    if app.status_draft.background == background {
                        ui.painter().rect_stroke(
                            rect,
                            6.0,
                            Stroke::new(2.0, palette.text),
                            egui::StrokeKind::Inside,
                        );
                    }
                    if response.clicked() {
                        app.status_draft.background = background;
                        app.status_draft.confirm = false;
                    }
                }
            });
            ui.horizontal_wrapped(|ui| {
                for privacy in Privacy::ALL {
                    let selected = app.status_draft.privacy == privacy;
                    if ui
                        .selectable_label(selected, stories::privacy_label(locale, privacy))
                        .clicked()
                    {
                        app.status_draft.privacy = privacy;
                        app.status_draft.confirm = false;
                    }
                }
                if ui.button(stories::phrase(locale, "pick")).clicked() {
                    app.actions.push(Action::PickStatusPhoto);
                }
                if app.status_draft.image.is_some()
                    && ui.button(stories::phrase(locale, "clear_photo")).clicked()
                {
                    app.status_draft.image = None;
                    app.status_draft.confirm = false;
                }
            });
            if app.status_draft.privacy != Privacy::Contacts {
                let people = people(app);
                ScrollArea::vertical().max_height(120.0).show(ui, |ui| {
                    for (id, name) in people {
                        let mut on = app.status_draft.picked.iter().any(|picked| picked == &id);
                        if ui.checkbox(&mut on, name).changed() {
                            if on {
                                app.status_draft.picked.push(id);
                            } else {
                                app.status_draft.picked.retain(|picked| picked != &id);
                            }
                            app.status_draft.confirm = false;
                        }
                    }
                });
            }
            if app.status_posting {
                ui.label(stories::phrase(locale, "publishing"));
            } else if app.status_draft.confirm {
                ui.label(stories::phrase(locale, "confirm"));
                ui.horizontal(|ui| {
                    if ui.button(stories::phrase(locale, "publish")).clicked()
                        && app.status_draft.can_publish()
                    {
                        app.actions.push(Action::PostStatus);
                    }
                    if ui.button(stories::phrase(locale, "cancel")).clicked() {
                        app.status_draft.confirm = false;
                    }
                });
            } else if ui.button(stories::phrase(locale, "publish")).clicked()
                && app.status_draft.can_publish()
            {
                app.status_draft.confirm = true;
            }
        });
}

fn show_viewer(app: &mut App, ui: &mut egui::Ui, locale: &str) {
    let palette = app.palette;
    let groups = stories::groups(&app.stories, crate::util::now());
    let Some(view) = app.story_view.clone() else {
        return;
    };
    let Some(story) = stories::current(&groups, &view).cloned() else {
        app.story_view = None;
        return;
    };
    let count = groups
        .iter()
        .find(|group| group.sender == view.sender)
        .map(|group| group.stories.len())
        .unwrap_or(1);
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        for index in 0..count {
            let color = if index <= view.index {
                palette.accent
            } else {
                palette.secondary
            };
            let (rect, _) = ui.allocate_exact_size(vec2(28.0, 3.0), Sense::hover());
            ui.painter().rect_filled(rect, 2.0, color);
        }
    });
    let name = groups
        .iter()
        .find(|group| group.sender == story.sender)
        .map(|group| {
            stories::contact_label(&group.stories, &group.name, stories::phrase(locale, "you"))
                .to_owned()
        })
        .unwrap_or_else(|| story.sender.clone());
    ui.horizontal(|ui| {
        ui.add_space(12.0);
        widgets::rich_text(ui, &name, theme::bold(16.0), palette.text);
        if theme::icon_button(
            ui,
            Icon::X,
            16.0,
            palette.secondary,
            palette.text,
            stories::phrase(locale, "back"),
        )
        .clicked()
        {
            app.actions.push(Action::CloseStory);
        }
    });
    let bounds = ui.available_rect_before_wrap().shrink2(vec2(12.0, 8.0));
    let bottom = (bounds.max.y - 40.0).max(bounds.min.y + 1.0);
    let stage = Rect::from_min_max(bounds.min, egui::pos2(bounds.max.x, bottom));
    let response = ui.allocate_rect(stage, Sense::click());
    let failed = app.status_failed.contains(&story.id);
    let advancing = response.clicked();
    let open_external = paint_story(app, ui, &story, stage, locale, failed, advancing);
    if story.needs_file()
        && story.path.is_none()
        && !failed
        && !app.status_fetching.contains(&story.id)
    {
        app.actions.push(Action::DownloadStory(story.id.clone()));
    }
    if advancing && !open_external && failed {
        app.actions.push(Action::DownloadStory(story.id.clone()));
    } else if advancing && !open_external {
        let left = response.rect.left() + response.rect.width() * 0.33;
        let delta = if response
            .interact_pointer_pos()
            .is_some_and(|pos| pos.x < left)
        {
            -1
        } else {
            1
        };
        app.actions.push(Action::StoryStep(delta));
    }
    ui.horizontal(|ui| {
        ui.add_space(12.0);
        if ui.button(stories::phrase(locale, "reply")).clicked() {
            app.actions.push(Action::ReplyToStatus {
                sender: story.sender.clone(),
                id: story.id.clone(),
            });
        }
    });
}

fn paint_story(
    app: &mut App,
    ui: &mut egui::Ui,
    story: &Story,
    rect: Rect,
    locale: &str,
    failed: bool,
    advancing: bool,
) -> bool {
    let mut open_external = false;
    match &story.kind {
        StoryKind::Text {
            text, background, ..
        } => {
            ui.painter()
                .rect_filled(rect, CornerRadius::same(12), argb(*background));
            let ink = if stories::dark_ink(*background) {
                Color32::BLACK
            } else {
                Color32::WHITE
            };
            let line = widgets::line(ui, text, theme::bold(28.0), ink, rect.width() - 48.0, 8);
            let pos = rect.center() - line.size() * 0.5;
            line.paint(ui, pos, ink);
        }
        StoryKind::Image { caption } | StoryKind::Video { caption } => {
            ui.painter()
                .rect_filled(rect, CornerRadius::same(12), Color32::from_black_alpha(180));
            if failed {
                let line = widgets::line(
                    ui,
                    stories::phrase(locale, "retry"),
                    theme::regular(16.0),
                    Color32::WHITE,
                    rect.width() - 32.0,
                    3,
                );
                line.paint(ui, rect.center() - line.size() * 0.5, Color32::WHITE);
            } else if let Some(path) = &story.path {
                if matches!(story.kind, StoryKind::Image { .. }) {
                    widgets::picture(ui, path, rect);
                } else {
                    if !advancing && !app.video.is_active(path) && app.video.refusal(path).is_none()
                    {
                        app.actions.push(Action::StatusVideo(path.clone()));
                    }
                    open_external = paint_status_video(app, ui, path, rect, locale);
                }
            }
            let label = if failed {
                String::new()
            } else {
                caption
                    .clone()
                    .filter(|text| !text.is_empty())
                    .unwrap_or_else(|| {
                        if story.path.is_none() {
                            stories::phrase(locale, "downloading").to_owned()
                        } else {
                            String::new()
                        }
                    })
            };
            if !label.is_empty() {
                let line = widgets::line(
                    ui,
                    &label,
                    theme::regular(16.0),
                    Color32::WHITE,
                    rect.width() - 32.0,
                    3,
                );
                line.paint(
                    ui,
                    rect.left_bottom() + vec2(16.0, -line.size().y - 16.0),
                    Color32::WHITE,
                );
            }
        }
    }
    open_external
}

fn paint_status_video(
    app: &mut App,
    ui: &mut egui::Ui,
    path: &std::path::Path,
    rect: Rect,
    locale: &str,
) -> bool {
    app.video
        .set_output(app.settings.video_volume, app.settings.video_muted);
    match app.video.poll(ui.ctx(), path) {
        crate::video::State::Showing { texture, size, .. } => {
            let natural = if size.x > 0.0 && size.y > 0.0 {
                size
            } else {
                vec2(4.0, 3.0)
            };
            let scale = (rect.width() / natural.x)
                .min(rect.height() / natural.y)
                .max(0.0);
            let placed = Rect::from_center_size(rect.center(), natural * scale);
            if ui.is_rect_visible(placed) {
                ui.painter().image(
                    texture.id(),
                    placed,
                    Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            false
        }
        crate::video::State::Unsupported(_) => ui
            .put(
                Rect::from_center_size(rect.center(), vec2(220.0, 32.0)),
                egui::Button::new(stories::phrase(locale, "open_file")),
            )
            .clicked()
            .then(|| app.actions.push(Action::OpenFile(path.to_path_buf())))
            .is_some(),
        crate::video::State::Loading => false,
    }
}

fn wake_for_expiry(ui: &egui::Ui, stories: &[Story]) {
    let now = crate::util::now();
    let Some(next) = stories
        .iter()
        .filter(|story| stories::alive(story.timestamp, now))
        .map(|story| story.timestamp.saturating_add(stories::TTL_SECS))
        .min()
    else {
        return;
    };
    let wait = next.saturating_sub(now).clamp(1, 86_400);
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_secs(wait as u64));
}

fn people(app: &App) -> Vec<(String, String)> {
    let me = app.me.as_deref();
    let mut rows = Vec::new();
    for contact in app.contacts.values() {
        if !stories::address_book(
            &contact.id,
            contact.full_name.as_deref(),
            me == Some(contact.id.as_str()),
        ) {
            continue;
        }
        let Some(name) = contact.full_name.clone() else {
            continue;
        };
        rows.push((contact.id.clone(), name));
    }
    rows.sort_by(|left, right| left.0.cmp(&right.0));
    rows.dedup_by(|left, right| left.0 == right.0);
    rows.sort_by(|left, right| left.1.cmp(&right.1));
    rows
}

fn argb(value: u32) -> Color32 {
    Color32::from_rgba_unmultiplied(
        ((value >> 16) & 0xff) as u8,
        ((value >> 8) & 0xff) as u8,
        (value & 0xff) as u8,
        ((value >> 24) & 0xff) as u8,
    )
}

pub fn reply_strip(app: &mut App, ui: &mut egui::Ui, story: &Story) {
    let palette = app.palette;
    let summary = story.preview();
    Frame::new()
        .fill(palette.surface)
        .corner_radius(CornerRadius::same(theme::RADIUS))
        .inner_margin(Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (bar, _) = ui.allocate_exact_size(vec2(3.0, 34.0), Sense::hover());
                ui.painter().rect_filled(bar, 2.0, palette.accent);
                ui.vertical(|ui| {
                    widgets::rich_text(
                        ui,
                        stories::phrase(
                            crate::i18n::message_locale_tag(crate::i18n::message_locale(
                                app.settings.language,
                            )),
                            "title",
                        ),
                        theme::semibold(12.5),
                        palette.accent,
                    );
                    widgets::rich_text(ui, &summary, theme::regular(12.5), palette.secondary);
                });
                if theme::icon_button(ui, Icon::X, 16.0, palette.secondary, palette.text, "Cancel")
                    .clicked()
                {
                    app.actions.push(Action::CancelReply);
                }
            });
        });
}
