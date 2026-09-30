//! Call history. Choosing a row opens that chat.

use egui::{Align, Color32, Layout, ScrollArea, Sense};

use crate::app::App;
use crate::model::{Action, Page};
use crate::theme::{self, Icon};
use crate::ui::widgets;

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let locale = crate::i18n::message_locale(app.settings.language);
    let heading = match crate::i18n::message_locale_tag(locale) {
        "pt" => "Chamadas",
        "es" => "Llamadas",
        _ => "Calls",
    };
    let empty = match crate::i18n::message_locale_tag(locale) {
        "pt" => "Nenhuma chamada ainda.",
        "es" => "Todavía no hay llamadas.",
        _ => "No calls yet.",
    };
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.add_space(12.0);
        if theme::icon_button(
            ui,
            Icon::ArrowLeft,
            18.0,
            palette.secondary,
            palette.text,
            "Chats",
        )
        .clicked()
        {
            app.actions.push(Action::Open(Page::Chats));
        }
        theme::text(ui, heading, theme::bold(20.0), palette.text);
    });
    ui.add_space(8.0);
    if app.call_records.is_empty() {
        ui.add_space(24.0);
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            theme::text(ui, empty, theme::regular(14.0), palette.secondary);
        });
        return;
    }
    let records = app.call_records.clone();
    ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for message in records {
                let title = app
                    .chat(&message.chat)
                    .map(|chat| chat.name.clone())
                    .filter(|name| !name.is_empty())
                    .or(message.sender_name.clone())
                    .unwrap_or_else(|| "WhatsApp".to_owned());
                let summary = message.content.summary();
                let when = crate::util::clock(message.timestamp);
                let chat = message.chat.clone();
                let response = ui
                    .push_id(&message.id, |ui| {
                        ui.horizontal(|ui| {
                            ui.add_space(12.0);
                            let picture = app.avatar(&chat);
                            widgets::avatar(ui, &palette, &title, &chat, 36.0, picture.as_deref());
                            ui.add_space(8.0);
                            ui.vertical(|ui| {
                                paint_line(ui, &title, theme::bold(14.0), palette.text, 1);
                                paint_line(
                                    ui,
                                    &summary,
                                    theme::regular(12.5),
                                    palette.secondary,
                                    1,
                                );
                            });
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                theme::text(ui, &when, theme::regular(12.0), palette.secondary);
                            });
                        });
                    })
                    .response
                    .interact(Sense::click());
                if response.clicked() {
                    app.actions.push(Action::OpenChat(chat));
                }
            }
        });
}

fn paint_line(ui: &mut egui::Ui, text: &str, font: egui::FontId, color: Color32, rows: usize) {
    let width = ui.available_width().max(1.0);
    let line = widgets::line(ui, text, font, color, width, rows);
    let (rect, _) = ui.allocate_exact_size(line.size(), Sense::hover());
    if ui.is_rect_visible(rect) {
        line.paint(ui, rect.min, color);
    }
}
