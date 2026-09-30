//! The settings page.

use egui::{CornerRadius, Frame, Margin};

use crate::app::App;
use crate::model::{Action, Dialog, Page};
use crate::settings::ThemeChoice;
use crate::theme::{self, Icon};

use super::widgets;

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    super::standalone_header(app, ui);
    if theme::macos_chrome(ui.ctx()) {
        super::banner(app, ui);
    }
    let palette = app.palette;
    egui::ScrollArea::vertical()
        .id_salt("settings")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            Frame::new()
                .inner_margin(Margin::symmetric(32, 24))
                .show(ui, |ui| {
                    ui.set_max_width(640.0);
                    ui.horizontal(|ui| {
                        if theme::icon_button(
                            ui,
                            Icon::ArrowLeft,
                            20.0,
                            palette.secondary,
                            palette.text,
                            "Back (Esc)",
                        )
                        .clicked()
                        {
                            app.actions.push(Action::Open(Page::Chats));
                        }
                        theme::text(
                            ui,
                            crate::i18n::t(app.settings.language, "settings.title"),
                            theme::bold(24.0),
                            palette.text,
                        );
                    });
                    ui.add_space(18.0);

                    section(
                        ui,
                        app,
                        &crate::i18n::t(app.settings.language, "settings.appearance"),
                    );
                    let detail = app
                        .custom_themes
                        .detail(app.settings.custom_theme.as_deref());
                    let detail = if !detail.is_empty() {
                        detail
                    } else if app.custom_themes.follows_omarchy() {
                        &crate::i18n::t(app.settings.language, "settings.theme_follow_omarchy")
                    } else {
                        &crate::i18n::t(app.settings.language, "settings.theme_follow_system")
                    };
                    widgets::setting_row(
                        ui,
                        &palette,
                        &crate::i18n::t(app.settings.language, "settings.theme"),
                        detail,
                        |ui| {
                            ui.with_layout(egui::Layout::top_down(egui::Align::Max), |ui| {
                                let selected = app
                                    .settings
                                    .custom_theme
                                    .as_deref()
                                    .map(|name| theme::custom::label(name).to_owned())
                                    .unwrap_or_else(|| {
                                        app.settings.theme.label_in(app.settings.language)
                                    });
                                let response = egui::ComboBox::from_id_salt("appearance_theme")
                                    .selected_text(" ")
                                    .width(200.0_f32.min(ui.available_width()))
                                    .height(320.0)
                                    .show_ui(ui, |ui| {
                                        for choice in ThemeChoice::ALL {
                                            if theme_option(
                                                ui,
                                                &palette,
                                                &choice.label_in(app.settings.language),
                                                app.settings.custom_theme.is_none()
                                                    && app.settings.theme == choice,
                                            ) {
                                                app.actions.push(Action::SetTheme(choice));
                                            }
                                        }
                                        if app.custom_themes.picker_themes().next().is_some() {
                                            ui.separator();
                                        }
                                        for custom in app.custom_themes.picker_themes() {
                                            if theme_option(
                                                ui,
                                                &palette,
                                                theme::custom::label(&custom.filename),
                                                app.settings.custom_theme.as_deref()
                                                    == Some(custom.filename.as_str()),
                                            ) {
                                                app.actions.push(Action::SetCustomTheme(
                                                    custom.filename.clone(),
                                                ));
                                            }
                                        }
                                    });
                                let rect = response.response.rect;
                                let text = widgets::line(
                                    ui,
                                    &selected,
                                    theme::regular(14.0),
                                    palette.text,
                                    rect.width() - 36.0,
                                    1,
                                );
                                text.paint(
                                    ui,
                                    egui::pos2(
                                        rect.left() + 8.0,
                                        rect.center().y - text.size().y / 2.0,
                                    ),
                                    palette.text,
                                );
                                response.response.widget_info(|| {
                                    let mut info = egui::WidgetInfo::labeled(
                                        egui::WidgetType::ComboBox,
                                        ui.is_enabled(),
                                        "Theme",
                                    );
                                    info.current_text_value = Some(selected.to_owned());
                                    info
                                });
                                if theme::soft_button(
                                    ui,
                                    &palette,
                                    Some(Icon::ExternalLink),
                                    &crate::i18n::t(app.settings.language, "settings.open_themes"),
                                    false,
                                )
                                .clicked()
                                {
                                    app.actions.push(Action::OpenThemesFolder);
                                }
                            });
                        },
                    );
                    widgets::setting_row(
                        ui,
                        &palette,
                        &crate::i18n::t(app.settings.language, "settings.language"),
                        &crate::i18n::t(app.settings.language, "settings.language_detail"),
                        |ui| {
                            ui.horizontal(|ui| {
                                for choice in crate::i18n::Language::ALL {
                                    if ui
                                        .selectable_label(
                                            app.settings.language == choice,
                                            choice.label(),
                                        )
                                        .clicked()
                                    {
                                        app.settings.language = choice;
                                        app.actions.push(Action::SettingsChanged);
                                    }
                                }
                            });
                        },
                    );
                    widgets::setting_row(
                        ui,
                        &palette,
                        &crate::i18n::t(app.settings.language, "settings.zoom"),
                        &crate::i18n::t(app.settings.language, "settings.zoom_hint"),
                        |ui| {
                            if theme::icon_button(
                                ui,
                                Icon::Plus,
                                16.0,
                                palette.secondary,
                                palette.text,
                                &crate::i18n::t(app.settings.language, "settings.larger"),
                            )
                            .clicked()
                            {
                                app.actions.push(Action::ZoomBy(0.1));
                            }
                            theme::text(
                                ui,
                                format!("{:.0}%", app.settings.zoom * 100.0),
                                theme::medium(13.5),
                                palette.text,
                            );
                            if theme::icon_button(
                                ui,
                                Icon::Minus,
                                16.0,
                                palette.secondary,
                                palette.text,
                                &crate::i18n::t(app.settings.language, "settings.smaller"),
                            )
                            .clicked()
                            {
                                app.actions.push(Action::ZoomBy(-0.1));
                            }
                        },
                    );

                    section(
                        ui,
                        app,
                        &crate::i18n::t(app.settings.language, "settings.chats"),
                    );
                    toggle(
                        ui,
                        app,
                        &crate::i18n::t(app.settings.language, "settings.enter_sends"),
                        &crate::i18n::t(app.settings.language, "settings.enter_sends_detail"),
                        |settings| &mut settings.enter_sends,
                    );
                    let receipts_note = if app.account_receipts_off {
                        crate::i18n::t(app.settings.language, "settings.receipts_off_detail")
                    } else {
                        crate::i18n::t(app.settings.language, "settings.receipts_on_detail")
                    };
                    toggle(
                        ui,
                        app,
                        &crate::i18n::t(app.settings.language, "settings.read_receipts"),
                        &receipts_note,
                        |settings| &mut settings.send_read_receipts,
                    );
                    toggle(
                        ui,
                        app,
                        &crate::i18n::t(app.settings.language, "settings.typing"),
                        "",
                        |settings| &mut settings.send_typing,
                    );
                    toggle(
                        ui,
                        app,
                        &crate::i18n::t(app.settings.language, "settings.auto_download"),
                        &crate::i18n::t(app.settings.language, "settings.auto_download_detail"),
                        |settings| &mut settings.auto_download,
                    );
                    toggle(
                        ui,
                        app,
                        &crate::i18n::t(app.settings.language, "settings.sender_pictures"),
                        &crate::i18n::t(app.settings.language, "settings.sender_pictures_detail"),
                        |settings| &mut settings.show_sender_pictures,
                    );
                    toggle(
                        ui,
                        app,
                        &crate::i18n::t(app.settings.language, "settings.names_contacts"),
                        &crate::i18n::t(app.settings.language, "settings.names_contacts_detail"),
                        |settings| &mut settings.names_from_contacts,
                    );
                    toggle(
                        ui,
                        app,
                        &crate::i18n::t(app.settings.language, "settings.save_contacts"),
                        &crate::i18n::t(app.settings.language, "settings.save_contacts_detail"),
                        |settings| &mut settings.save_contacts_to_phone,
                    );
                    toggle(
                        ui,
                        app,
                        &crate::i18n::t(app.settings.language, "settings.shortcut_hints"),
                        "",
                        |settings| &mut settings.show_shortcut_hints,
                    );

                    section(
                        ui,
                        app,
                        &crate::i18n::t(app.settings.language, "settings.audio"),
                    );
                    theme::paragraph(
                        ui,
                        crate::i18n::t(app.settings.language, "settings.audio_note"),
                        theme::regular(12.5),
                        palette.secondary,
                    );
                    toggle(
                        ui,
                        app,
                        &crate::i18n::t(app.settings.language, "settings.play_next"),
                        &crate::i18n::t(app.settings.language, "settings.play_next_detail"),
                        |settings| &mut settings.play_next_audio,
                    );

                    section(
                        ui,
                        app,
                        &crate::i18n::t(app.settings.language, "settings.window"),
                    );
                    toggle(
                        ui,
                        app,
                        &crate::i18n::t(app.settings.language, "settings.keep_running"),
                        &crate::i18n::t(app.settings.language, "settings.keep_running_detail"),
                        |settings| &mut settings.keep_running_in_background,
                    );
                    toggle(
                        ui,
                        app,
                        &crate::i18n::t(app.settings.language, "settings.notify"),
                        &crate::i18n::t(app.settings.language, "settings.notify_detail"),
                        |settings| &mut settings.notifications,
                    );
                    toggle(
                        ui,
                        app,
                        &crate::i18n::t(app.settings.language, "settings.auto_update"),
                        &crate::i18n::t(app.settings.language, "settings.auto_update_detail"),
                        |settings| &mut settings.download_updates_automatically,
                    );
                    toggle(
                        ui,
                        app,
                        &crate::i18n::t(app.settings.language, "settings.check_updates"),
                        &crate::i18n::t(app.settings.language, "settings.check_updates_detail"),
                        |settings| &mut settings.check_for_updates,
                    );
                    widgets::setting_row(
                        ui,
                        &palette,
                        &crate::i18n::t(app.settings.language, "settings.update_channel"),
                        &crate::i18n::t(app.settings.language, "settings.update_channel_detail"),
                        |ui| {
                            ui.horizontal(|ui| {
                                for (choice, label) in [
                                    (
                                        crate::updates::Channel::Stable,
                                        crate::i18n::t(
                                            app.settings.language,
                                            "settings.channel_stable",
                                        ),
                                    ),
                                    (
                                        crate::updates::Channel::Testing,
                                        crate::i18n::t(
                                            app.settings.language,
                                            "settings.channel_testing",
                                        ),
                                    ),
                                ] {
                                    if ui
                                        .selectable_label(
                                            app.settings.update_channel == choice,
                                            label,
                                        )
                                        .clicked()
                                    {
                                        app.settings.update_channel = choice;
                                        app.actions.push(Action::SettingsChanged);
                                    }
                                }
                            });
                        },
                    );

                    section(
                        ui,
                        app,
                        &crate::i18n::t(app.settings.language, "settings.account"),
                    );
                    let name = app.me_name.clone().unwrap_or_default();
                    let me = app.me.clone().unwrap_or_default();
                    let phone = crate::model::phone_of(&me)
                        .map(crate::util::phone)
                        .unwrap_or_else(|| me.clone());
                    let description = match &app.me_about {
                        Some(about) => format!("{phone} · {about}"),
                        None => phone,
                    };
                    ui.horizontal(|ui| {
                        let picture = app.avatar_full(&me).or_else(|| app.avatar(&me));
                        widgets::avatar(ui, &palette, &name, &me, 56.0, picture.as_deref());
                    });
                    ui.add_space(6.0);
                    let who = if name.is_empty() {
                        crate::i18n::t(app.settings.language, "settings.linked_device")
                    } else {
                        name.clone()
                    };
                    widgets::setting_row(ui, &palette, &who, &description, |ui| {
                        if theme::soft_button(
                            ui,
                            &palette,
                            Some(Icon::LogOut),
                            &crate::i18n::t(app.settings.language, "settings.unlink"),
                            false,
                        )
                        .clicked()
                        {
                            app.actions.push(Action::ShowDialog(Dialog::ConfirmUnlink));
                        }
                    });

                    section(
                        ui,
                        app,
                        &crate::i18n::t(app.settings.language, "settings.files"),
                    );
                    let archive = app.dirs.archive_db();
                    widgets::setting_row(
                        ui,
                        &palette,
                        &crate::i18n::t(app.settings.language, "settings.archive"),
                        &archive.display().to_string(),
                        |ui| {
                            if theme::soft_button(
                                ui,
                                &palette,
                                Some(Icon::ExternalLink),
                                &crate::i18n::t(app.settings.language, "settings.open_folder"),
                                false,
                            )
                            .clicked()
                            {
                                app.actions.push(Action::OpenFile(app.dirs.state.clone()));
                            }
                        },
                    );
                    let media = app.dirs.media_cache_dir();
                    widgets::setting_row(
                        ui,
                        &palette,
                        &crate::i18n::t(app.settings.language, "settings.downloads"),
                        &media.display().to_string(),
                        |ui| {
                            if theme::soft_button(
                                ui,
                                &palette,
                                Some(Icon::ExternalLink),
                                &crate::i18n::t(app.settings.language, "settings.open_folder"),
                                false,
                            )
                            .clicked()
                            {
                                let _ = std::fs::create_dir_all(&media);
                                app.actions.push(Action::OpenFile(media.clone()));
                            }
                        },
                    );
                    let log = app.dirs.log_file();
                    widgets::setting_row(
                        ui,
                        &palette,
                        &crate::i18n::t(app.settings.language, "settings.log"),
                        &log.display().to_string(),
                        |ui| {
                            if theme::soft_button(
                                ui,
                                &palette,
                                Some(Icon::FileText),
                                &crate::i18n::t(app.settings.language, "settings.open"),
                                false,
                            )
                            .clicked()
                            {
                                app.actions.push(Action::OpenFile(log.clone()));
                            }
                        },
                    );

                    section(
                        ui,
                        app,
                        &crate::i18n::t(app.settings.language, "settings.about"),
                    );
                    widgets::setting_row(
                        ui,
                        &palette,
                        &format!("Vespera {}", crate::updates::vespera_version()),
                        &crate::i18n::t(app.settings.language, "settings.about_detail"),
                        |ui| {
                            if theme::soft_button(
                                ui,
                                &palette,
                                Some(Icon::Info),
                                &crate::i18n::t(app.settings.language, "settings.about"),
                                false,
                            )
                            .clicked()
                            {
                                app.actions.push(Action::ShowDialog(Dialog::About));
                            }
                            if theme::soft_button(
                                ui,
                                &palette,
                                Some(Icon::Keyboard),
                                &crate::i18n::t(app.settings.language, "settings.shortcuts"),
                                false,
                            )
                            .clicked()
                            {
                                app.actions.push(Action::ShowDialog(Dialog::Shortcuts));
                            }
                        },
                    );
                });
        });
}

fn section(ui: &mut egui::Ui, app: &App, label: &str) {
    let palette = app.palette;
    ui.add_space(10.0);
    Frame::new()
        .fill(palette.panel)
        .corner_radius(CornerRadius::same(theme::RADIUS))
        .inner_margin(Margin::symmetric(14, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            theme::text(ui, label, theme::semibold(12.5), palette.accent);
        });
    ui.add_space(8.0);
}

fn toggle(
    ui: &mut egui::Ui,
    app: &mut App,
    label: &str,
    description: &str,
    field: impl Fn(&mut crate::settings::Settings) -> &mut bool,
) {
    let palette = app.palette;
    let mut value = *field(&mut app.settings);
    let mut changed = false;
    widgets::setting_row(ui, &palette, label, description, |ui| {
        changed = widgets::switch(ui, &palette, &mut value).changed();
    });
    if changed {
        *field(&mut app.settings) = value;
        app.actions.push(Action::SettingsChanged);
    }
}

/// Theme filenames can contain emoji, so paint them through the shared line renderer.
fn theme_option(ui: &mut egui::Ui, palette: &theme::Palette, text: &str, selected: bool) -> bool {
    let response = ui.add(
        egui::Button::selectable(selected, " ").min_size(egui::vec2(ui.available_width(), 28.0)),
    );
    let rect = response.rect;
    let line = widgets::line(
        ui,
        text,
        theme::regular(14.0),
        palette.text,
        rect.width() - 16.0,
        1,
    );
    if ui.is_rect_visible(rect) {
        line.paint(
            ui,
            egui::pos2(rect.left() + 8.0, rect.center().y - line.size().y / 2.0),
            palette.text,
        );
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::SelectableLabel,
            ui.is_enabled(),
            selected,
            text,
        )
    });
    response.clicked()
}
