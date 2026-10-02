//! Status updates from the last 24 hours.

use egui::{
    Color32, CornerRadius, Frame, Margin, Rect, RichText, ScrollArea, Sense, Stroke, Vec2, vec2,
};

use crate::app::App;
use crate::i18n::Language;
use crate::model::{Action, Dialog, Page};
use crate::stories::{self, Privacy, Story, StoryGroup, StoryKind, StoryView};
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

fn t(language: Language, key: &str) -> String {
    crate::i18n::t(language, key)
}

/// Relative age of the newest item: minutes and hours in the app language,
/// older than a day falls back to the dated stamp.
fn ago(language: Language, locale: &str, when: i64, now: i64) -> String {
    let elapsed = now.saturating_sub(when);
    if elapsed < 60 {
        t(language, "status.just_now")
    } else if elapsed < 3_600 {
        t(language, "status.minutes").replace("{n}", &(elapsed / 60).to_string())
    } else if elapsed < 86_400 {
        t(language, "status.hours").replace("{n}", &(elapsed / 3_600).to_string())
    } else {
        crate::util::chat_stamp_in(when, locale)
    }
}

/// List preview in the app language: captions and text as-is, bare media
/// as Photo/Video instead of English literals or a raw URL.
fn preview_in(language: Language, story: &Story) -> String {
    match &story.kind {
        StoryKind::Text { text, .. } => text.clone(),
        StoryKind::Image { caption } | StoryKind::Video { caption } => caption
            .clone()
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| {
                if matches!(story.kind, StoryKind::Video { .. }) {
                    t(language, "status.video")
                } else {
                    t(language, "status.photo")
                }
            }),
    }
}

/// Splits live groups into mine, recent (unseen), and viewed, newest first
/// inside each section.
fn split<'a>(
    groups: &'a [StoryGroup],
    me: Option<&str>,
) -> (
    Option<&'a StoryGroup>,
    Vec<&'a StoryGroup>,
    Vec<&'a StoryGroup>,
) {
    let mut mine = None;
    let mut recent = Vec::new();
    let mut viewed = Vec::new();
    for group in groups {
        let own = group.stories.first().is_some_and(|story| story.from_me)
            || me.is_some_and(|me| group.sender == me);
        if own {
            mine = Some(group);
        } else if group.unseen > 0 {
            recent.push(group);
        } else {
            viewed.push(group);
        }
    }
    (mine, recent, viewed)
}

/// Segmented seen/unseen ring around an avatar, one arc per status.
fn status_ring(
    ui: &egui::Ui,
    palette: crate::theme::Palette,
    rect: Rect,
    total: usize,
    unseen: usize,
) {
    if total == 0 || !ui.is_rect_visible(rect) {
        return;
    }
    let shown = total.min(12);
    let unseen = unseen.min(shown);
    let center = rect.center();
    let radius = rect.width() / 2.0;
    let gap = 0.12;
    let sweep = std::f32::consts::TAU / shown as f32 - gap;
    for index in 0..shown {
        let start = -std::f32::consts::FRAC_PI_2 + index as f32 * (sweep + gap);
        let color = if index < unseen {
            palette.accent
        } else {
            palette.secondary.gamma_multiply(0.45)
        };
        let steps = 24usize.max((sweep * radius / 3.0) as usize);
        let points: Vec<egui::Pos2> = (0..=steps)
            .map(|step| {
                let angle = start + sweep * step as f32 / steps as f32;
                center + Vec2::new(angle.cos() * radius, angle.sin() * radius)
            })
            .collect();
        ui.painter().line(points, Stroke::new(2.5, color));
    }
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
    let language = app.settings.language;
    let now = crate::util::now();
    let groups = stories::groups(&app.stories, now);
    let me = app.me.clone();
    let (mine, recent, viewed) = split(&groups, me.as_deref());
    ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            my_status_row(app, ui, language, locale, now, mine);
            if mine.is_none() && recent.is_empty() && viewed.is_empty() {
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
                return;
            }
            if !recent.is_empty() {
                section_title(ui, &t(language, "status.recent"), palette);
                for group in recent {
                    status_row(app, ui, language, locale, now, &groups, group);
                }
            }
            if !viewed.is_empty() {
                section_title(ui, &t(language, "status.viewed"), palette);
                for group in viewed {
                    status_row(app, ui, language, locale, now, &groups, group);
                }
            }
        });
}

fn section_title(ui: &mut egui::Ui, title: &str, palette: crate::theme::Palette) {
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.add_space(12.0);
        widgets::rich_text(ui, title, theme::semibold(13.0), palette.secondary);
    });
    ui.add_space(2.0);
}

/// Opens the viewer on a group, starting at the first unseen story.
fn open_group(app: &mut App, groups: &[StoryGroup], group: &StoryGroup) {
    let index = group
        .stories
        .iter()
        .position(|story| !story.seen)
        .unwrap_or(0);
    let order = groups.iter().map(|group| group.sender.clone()).collect();
    app.story_view = Some(StoryView {
        sender: group.sender.clone(),
        index,
        order,
    });
    app.actions.push(Action::StoryStep(0));
}

/// "My status" row with the add badge. The row opens the viewer, the badge
/// opens the creation dialog.
fn my_status_row(
    app: &mut App,
    ui: &mut egui::Ui,
    language: Language,
    locale: &str,
    now: i64,
    mine: Option<&StoryGroup>,
) {
    let palette = app.palette;
    let (count, when) = mine
        .map(|group| {
            (
                group.stories.len(),
                group.stories.last().map(|story| story.timestamp),
            )
        })
        .unwrap_or((0, None));
    let name = t(language, "status.my_status");
    let detail = when
        .map(|at| ago(language, locale, at, now))
        .unwrap_or_else(|| t(language, "status.add"));
    let picture = {
        let me = app.me.clone().unwrap_or_default();
        app.avatar(&me)
    };
    let me_id = app.me.clone().unwrap_or_default();
    let mut open_viewer = false;
    let mut open_composer = false;
    ui.horizontal(|ui| {
        ui.add_space(12.0);
        let size = 44.0;
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
        if ui.is_rect_visible(rect) {
            let ring = rect.shrink(3.0);
            status_ring(ui, palette, ring, count.max(1), 0);
            widgets::paint_avatar(
                ui,
                &palette,
                ring.shrink(3.0),
                &name,
                &me_id,
                picture.as_deref(),
            );
            let badge =
                Rect::from_center_size(rect.right_bottom() - vec2(4.0, 4.0), Vec2::splat(18.0));
            ui.painter()
                .circle_filled(badge.center(), 9.0, palette.accent);
            ui.painter().text(
                badge.center(),
                egui::Align2::CENTER_CENTER,
                "+",
                theme::bold(14.0),
                palette.on_accent,
            );
            let pointer = ui.input(|input| input.pointer.interact_pos());
            if let Some(pos) = pointer
                && badge.contains(pos)
                && ui.input(|input| input.pointer.primary_clicked())
            {
                open_composer = true;
            } else if ui.input(|input| {
                input.pointer.primary_clicked()
                    && input
                        .pointer
                        .interact_pos()
                        .is_some_and(|pos| rect.contains(pos))
            }) {
                open_viewer = count > 0;
                open_composer = count == 0;
            }
        }
        ui.add_space(8.0);
        ui.vertical(|ui| {
            widgets::rich_text(ui, &name, theme::semibold(14.5), palette.text);
            widgets::rich_text(ui, &detail, theme::regular(12.5), palette.secondary);
        });
    });
    if open_composer {
        app.actions.push(Action::ShowDialog(Dialog::StatusComposer));
    } else if open_viewer && let Some(group) = mine {
        let groups = stories::groups(&app.stories, crate::util::now());
        let sender = group.sender.clone();
        let current = groups.iter().find(|group| group.sender == sender);
        if let Some(current) = current {
            open_group(app, &groups, current);
        }
    }
}

/// One contact row: segmented ring, name, relative time, and a thumbnail of
/// the newest item.
fn status_row(
    app: &mut App,
    ui: &mut egui::Ui,
    language: Language,
    locale: &str,
    now: i64,
    groups: &[StoryGroup],
    group: &StoryGroup,
) {
    let palette = app.palette;
    let name = stories::contact_label(&group.stories, &group.name, stories::phrase(locale, "you"))
        .to_owned();
    let last = group.stories.last();
    let preview = last
        .map(|story| preview_in(language, story))
        .unwrap_or_default();
    let when = last
        .map(|story| ago(language, locale, story.timestamp, now))
        .unwrap_or_default();
    let line = format!("{preview} · {when}");
    let sender = group.sender.clone();
    let total = group.stories.len();
    let unseen = group.unseen;
    let picture = app.avatar(&sender);
    let thumb = last
        .filter(|story| matches!(story.kind, StoryKind::Image { .. }))
        .and_then(|story| story.path.clone());
    let tint = last
        .map(|story| match &story.kind {
            StoryKind::Text { background, .. } => argb(*background),
            _ => Color32::from_black_alpha(180),
        })
        .unwrap_or(Color32::from_black_alpha(180));
    let response = ui
        .push_id(&sender, |ui| {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                let size = 44.0;
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
                if ui.is_rect_visible(rect) {
                    let ring = rect.shrink(2.0);
                    status_ring(ui, palette, ring, total, unseen);
                    widgets::paint_avatar(
                        ui,
                        &palette,
                        ring.shrink(3.0),
                        &name,
                        &sender,
                        picture.as_deref(),
                    );
                }
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
                        line.trim_start_matches(" · "),
                        theme::regular(12.5),
                        palette.secondary,
                    );
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(12.0);
                    let (tile, _) = ui.allocate_exact_size(vec2(44.0, 44.0), Sense::hover());
                    if ui.is_rect_visible(tile) {
                        let radius = CornerRadius::same(8);
                        match thumb {
                            Some(path) if widgets::picture(ui, &path, tile) => {}
                            _ => {
                                ui.painter().rect_filled(tile, radius, tint);
                            }
                        }
                    }
                });
            });
        })
        .response;
    if response.interact(Sense::click()).clicked() {
        let current = groups.iter().find(|group| group.sender == sender);
        if let Some(current) = current {
            open_group(app, groups, current);
        }
    }
}

/// Status creation dialog body: colored text or photo/video with a caption,
/// privacy list, and a prominent Publish.
pub fn composer_dialog(app: &mut App, ui: &mut egui::Ui) {
    let locale =
        crate::i18n::message_locale_tag(crate::i18n::message_locale(app.settings.language));
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
    let mut advancing = response.clicked();
    // Timed advance, paused while held. The timer only runs once the item
    // is displayable: a pending download or a retry prompt must not burn
    // the viewing window. A hold suppresses its own release click, while a
    // quick tap still steps.
    let displayable = !story.needs_file() || story.path.is_some();
    let failed_here = app.status_failed.contains(&story.id);
    let duration =
        std::time::Duration::from_secs(if matches!(story.kind, StoryKind::Video { .. }) {
            15
        } else {
            5
        });
    let down_here = response.contains_pointer() && ui.input(|input| input.pointer.primary_down());
    let now = std::time::Instant::now();
    if advancing {
        let hold = app
            .story_press_at
            .is_some_and(|at| now.duration_since(at) > std::time::Duration::from_millis(400));
        app.story_press_at = None;
        if hold {
            // A hold replays the full duration instead of stepping.
            advancing = false;
            app.story_started_at = None;
        }
    } else if down_here {
        if app.story_press_at.is_none() {
            app.story_press_at = Some(now);
        }
        app.story_started_at = None;
    } else {
        app.story_press_at = None;
        if displayable && !failed_here {
            match app.story_started_at {
                None => {
                    app.story_started_at = Some(now);
                    ui.ctx().request_repaint_after(duration);
                }
                Some(started) => match duration.checked_sub(now.duration_since(started)) {
                    Some(left) => ui.ctx().request_repaint_after(left),
                    None => app.actions.push(Action::StoryStep(1)),
                },
            }
        } else {
            app.story_started_at = None;
        }
    }
    let failed = app.status_failed.contains(&story.id);
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
        ui.add_space(8.0);
        ui.spacing_mut().item_spacing.x = 2.0;
        for emoji in ["❤️", "😂", "😮", "😢", "🙏", "👏"] {
            // Painted through the emoji pipeline (like message quick
            // reactions): a plain button would stay monochrome.
            let line = widgets::line(ui, emoji, theme::regular(20.0), palette.text, 40.0, 1);
            let (rect, response) = ui.allocate_exact_size(vec2(34.0, 34.0), Sense::click());
            if response.hovered() {
                ui.painter()
                    .circle_filled(rect.center(), 17.0, palette.surface_hover);
            }
            line.paint(ui, rect.center() - line.size() / 2.0, palette.text);
            if response
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
            {
                app.actions.push(Action::ReactToStory {
                    sender: story.sender.clone(),
                    id: story.id.clone(),
                    emoji: emoji.to_owned(),
                });
            }
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
                if theme::icon_button(
                    ui,
                    Icon::X,
                    16.0,
                    palette.secondary,
                    palette.text,
                    &crate::i18n::t(app.settings.language, "dialog.cancel"),
                )
                .clicked()
                {
                    app.actions.push(Action::CancelReply);
                }
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stories::{Privacy, StoryKind};

    fn story(id: &str, sender: &str, from_me: bool, seen: bool, at: i64) -> Story {
        Story {
            id: id.into(),
            sender: sender.into(),
            sender_name: None,
            from_me,
            timestamp: at,
            kind: StoryKind::Text {
                text: "hello".into(),
                background: 0xff000000,
                font: 0,
            },
            seen,
            path: None,
        }
    }

    #[test]
    fn ago_counts_minutes_hours_then_dates() {
        let now = 10_000;
        assert_eq!(ago(Language::English, "en", now - 10, now), "just now");
        assert_eq!(ago(Language::English, "en", now - 120, now), "2 min ago");
        assert_eq!(ago(Language::Portuguese, "pt", now - 120, now), "há 2 min");
        assert_eq!(ago(Language::Portuguese, "pt", now - 7_200, now), "há 2 h");
        assert_eq!(ago(Language::Spanish, "es", now - 7_200, now), "hace 2 h");
        assert_eq!(
            ago(Language::English, "en", now - 90_000, now),
            crate::util::chat_stamp_in(now - 90_000, "en")
        );
    }

    #[test]
    fn previews_name_bare_media_in_the_app_language() {
        let photo = Story {
            kind: StoryKind::Image { caption: None },
            ..story("p", "a", false, false, 0)
        };
        assert_eq!(preview_in(Language::English, &photo), "Photo");
        assert_eq!(preview_in(Language::Portuguese, &photo), "Foto");
        let captioned = Story {
            kind: StoryKind::Image {
                caption: Some("beach".into()),
            },
            ..story("c", "a", false, false, 0)
        };
        assert_eq!(preview_in(Language::Portuguese, &captioned), "beach");
        let clip = Story {
            kind: StoryKind::Video { caption: None },
            ..story("v", "a", false, false, 0)
        };
        assert_eq!(preview_in(Language::Portuguese, &clip), "Vídeo");
    }

    #[test]
    fn split_separates_mine_recent_and_viewed() {
        let rows = vec![
            story("m1", "me", true, true, 300),
            story("a1", "a", false, false, 200),
            story("a2", "a", false, true, 100),
            story("b1", "b", false, true, 400),
        ];
        let groups = stories::groups(&rows, 1_000);
        let (mine, recent, viewed) = split(&groups, Some("me"));
        assert_eq!(mine.map(|group| group.sender.as_str()), Some("me"));
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].sender, "a");
        assert_eq!(viewed.len(), 1);
        assert_eq!(viewed[0].sender, "b");
    }

    #[test]
    fn privacy_list_keeps_all_three_choices() {
        assert_eq!(Privacy::ALL.len(), 3);
    }
}
