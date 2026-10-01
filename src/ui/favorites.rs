//! Starred messages, for every chat or the one that is open.

use egui::{ScrollArea, Sense};

use crate::app::App;
use crate::model::{Action, Page};
use crate::theme::{self, Icon};

use super::widgets;

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let locale =
        crate::i18n::message_locale_tag(crate::i18n::message_locale(app.settings.language));
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
            phrase(locale, "back"),
        )
        .clicked()
        {
            app.actions.push(Action::Open(Page::Chats));
        }
        theme::text(ui, phrase(locale, "title"), theme::bold(18.0), palette.text);
    });
    ui.add_space(8.0);
    let mut changed = app.favorites_sent.is_empty();
    ui.horizontal(|ui| {
        ui.add_space(12.0);
        changed |= ui
            .add(
                egui::TextEdit::singleline(&mut app.favorites_query)
                    .hint_text(phrase(locale, "search"))
                    .desired_width(220.0),
            )
            .changed();
        if ui
            .selectable_label(app.favorites_chat_only, phrase(locale, "chat"))
            .clicked()
        {
            app.favorites_chat_only = true;
            changed = true;
        }
        if ui
            .selectable_label(!app.favorites_chat_only, phrase(locale, "all"))
            .clicked()
        {
            app.favorites_chat_only = false;
            changed = true;
        }
    });
    if changed {
        request(app);
    }
    let hits = app.favorites.clone();
    if hits.is_empty() {
        ui.add_space(24.0);
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            theme::text(
                ui,
                phrase(locale, "empty"),
                theme::regular(14.0),
                palette.secondary,
            );
        });
        return;
    }
    ScrollArea::vertical().show(ui, |ui| {
        for hit in hits {
            let name = app
                .chat(&hit.chat)
                .map(|chat| chat.name.clone())
                .unwrap_or_else(|| hit.chat.clone());
            let label = format!(
                "{name}  {}  {}",
                crate::util::clock(hit.timestamp),
                hit.preview
            );
            let line = widgets::line(
                ui,
                &label,
                theme::regular(14.0),
                palette.text,
                ui.available_width(),
                1,
            );
            let (rect, response) = ui.allocate_exact_size(line.size(), Sense::click());
            line.paint(ui, rect.min, palette.text);
            if response
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
            {
                app.actions.push(Action::OpenMessage {
                    chat: hit.chat,
                    message: hit.id,
                });
            }
        }
    });
}

fn request(app: &mut App) {
    let chat = app
        .favorites_chat_only
        .then(|| app.open_chat.clone())
        .flatten();
    let key = format!("{}|{}", chat.as_deref().unwrap_or(""), app.favorites_query);
    if app.favorites_sent == key {
        return;
    }
    app.favorites_sent = key;
    app.actions.push(Action::LoadFavorites {
        chat,
        query: app.favorites_query.clone(),
    });
}

pub fn phrase(locale: &str, key: &str) -> &'static str {
    match (locale, key) {
        ("pt", "title") => "Favoritas",
        ("es", "title") => "Favoritas",
        (_, "title") => "Favorites",
        ("pt", "empty") => "Nenhuma mensagem favorita",
        ("es", "empty") => "Ningún mensaje favorito",
        (_, "empty") => "No favorite messages",
        ("pt", "search") => "Buscar",
        ("es", "search") => "Buscar",
        (_, "search") => "Search",
        ("pt", "chat") => "Nesta conversa",
        ("es", "chat") => "En este chat",
        (_, "chat") => "This chat",
        ("pt", "all") => "Todas",
        ("es", "all") => "Todas",
        (_, "all") => "All",
        ("pt", "back") => "Voltar",
        ("es", "back") => "Volver",
        (_, "back") => "Back",
        ("pt", "pin") => "Fixar mensagem",
        ("es", "pin") => "Fijar mensaje",
        (_, "pin") => "Pin message",
        ("pt", "day") => "24 horas",
        ("es", "day") => "24 horas",
        (_, "day") => "24 hours",
        ("pt", "week") => "7 dias",
        ("es", "week") => "7 días",
        (_, "week") => "7 days",
        ("pt", "month") => "30 dias",
        ("es", "month") => "30 días",
        (_, "month") => "30 days",
        ("pt", "pinned") => "Mensagem fixada",
        ("es", "pinned") => "Mensaje fijado",
        (_, "pinned") => "Pinned message",
        _ => "",
    }
}
