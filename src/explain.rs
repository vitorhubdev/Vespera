//! User-facing names for protocol messages the bubble cannot draw as media.
//!
//! The archive stores the protobuf field name and a stable reason key.
//! Portuguese, Spanish, and English are chosen when the bubble is drawn.

/// Title, one-sentence reason, and whether to tell the reader to use the phone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub title: String,
    pub reason: String,
    pub open_on_phone: bool,
}

/// `field` is the protobuf name (`send_payment_message`) or a legacy label
/// stored before field names were kept (`payment`).
pub fn notice(locale: &str, field: &str, reason_key: &str) -> Notice {
    let locale = crate::i18n::message_locale_tag(locale);
    let field = canonical_field(field);
    let title = label(locale, field).unwrap_or_else(|| field.to_owned());
    let open_on_phone = matches!(reason_key, "official_app" | "phone");
    Notice {
        title,
        reason: reason_sentence(locale, reason_key),
        open_on_phone,
    }
}

/// Composer replacement when this linked device has no proven way to
/// prompt Meta AI. History in the chat stays visible.
pub fn meta_ai_notice(locale: &str) -> &'static str {
    match crate::i18n::message_locale_tag(locale) {
        "pt" => "Para conversar com a Meta AI, use o celular",
        "es" => "Para conversar con Meta AI, usa el celular",
        _ => "To talk to Meta AI, use your phone",
    }
}

pub fn open_on_phone(locale: &str) -> &'static str {
    match crate::i18n::message_locale_tag(locale) {
        "pt" => "Abrir no celular",
        "es" => "Abrir en el celular",
        _ => "Open it on your phone",
    }
}

pub fn view_once(locale: &str, what: &str, can_open: bool) -> Notice {
    let locale = crate::i18n::message_locale_tag(locale);
    let kind = view_once_kind(locale, what);
    let (title, reason, open_on_phone) = match (locale, can_open) {
        ("pt", true) => (
            format!("Visualização única ({kind})"),
            "Abre uma vez neste computador e não fica guardada.".to_owned(),
            false,
        ),
        ("es", true) => (
            format!("Visualización única ({kind})"),
            "Se abre una vez en este equipo y no se guarda.".to_owned(),
            false,
        ),
        (_, true) => (
            format!("View once ({kind})"),
            "Opens once on this computer and is not kept.".to_owned(),
            false,
        ),
        ("pt", false) => (
            format!("Visualização única ({kind})"),
            "Por privacidade, o WhatsApp só entrega no celular.".to_owned(),
            true,
        ),
        ("es", false) => (
            format!("Visualización única ({kind})"),
            "Por privacidad, WhatsApp solo lo entrega en el celular.".to_owned(),
            true,
        ),
        _ => (
            format!("View once ({kind})"),
            "For privacy, WhatsApp only delivers this to the phone.".to_owned(),
            true,
        ),
    };
    Notice {
        title,
        reason,
        open_on_phone,
    }
}

pub fn view_once_button(locale: &str) -> &'static str {
    match crate::i18n::message_locale_tag(locale) {
        "pt" => "Ver uma vez",
        "es" => "Ver una vez",
        _ => "View once",
    }
}

pub fn call_title(locale: &str, video: bool, outcome: &str) -> String {
    let locale = crate::i18n::message_locale_tag(locale);
    let (kind, result) = match locale {
        "pt" => (
            if video { "vídeo" } else { "voz" },
            match outcome {
                "answered" => "atendida",
                "rejected" => "recusada",
                "failed" => "falhou",
                "ongoing" => "em andamento",
                _ => "perdida",
            },
        ),
        "es" => (
            if video { "video" } else { "voz" },
            match outcome {
                "answered" => "atendida",
                "rejected" => "rechazada",
                "failed" => "fallida",
                "ongoing" => "en curso",
                _ => "perdida",
            },
        ),
        _ => (
            if video { "video" } else { "voice" },
            match outcome {
                "answered" => "answered",
                "rejected" => "declined",
                "failed" => "failed",
                "ongoing" => "ongoing",
                _ => "missed",
            },
        ),
    };
    match locale {
        "pt" => format!("Chamada de {kind} {result}"),
        "es" => format!("Llamada de {kind} {result}"),
        _ => {
            let result = match outcome {
                "answered" => "Answered",
                "rejected" => "Declined",
                "failed" => "Failed",
                "ongoing" => "Ongoing",
                _ => "Missed",
            };
            format!("{result} {kind} call")
        }
    }
}

pub fn join_copy(locale: &str, name: &str) -> (&'static str, String, &'static str, &'static str) {
    match crate::i18n::message_locale_tag(locale) {
        "pt" => (
            "Entrar no grupo?",
            format!("Entrar em {name}? Você passa a receber as mensagens deste grupo."),
            "Entrar",
            "Cancelar",
        ),
        "es" => (
            "¿Entrar al grupo?",
            format!("¿Entrar en {name}? Vas a recibir los mensajes de este grupo."),
            "Entrar",
            "Cancelar",
        ),
        _ => (
            "Join this group?",
            format!("Join {name}? You will start receiving its messages."),
            "Join",
            "Cancel",
        ),
    }
}

pub fn join_button(locale: &str) -> &'static str {
    match crate::i18n::message_locale_tag(locale) {
        "pt" | "es" => "Entrar",
        _ => "Join",
    }
}

fn canonical_field(field: &str) -> &str {
    match field {
        "payment" => "send_payment_message",
        "product" | "catalog" => "product_message",
        "group invite" => "group_invite_message",
        "event" => "event_message",
        "call" => "call_log_messsage",
        "animated sticker" => "lottie_sticker_message",
        "sticker pack" => "sticker_pack_message",
        "interactive message" => "interactive_message",
        "message" => "message",
        other => other,
    }
}

fn label(locale: &str, field: &str) -> Option<String> {
    let row = LABELS.iter().find(|(name, _, _, _)| *name == field)?;
    let text = match locale {
        "pt" => row.2,
        "es" => row.3,
        _ => row.1,
    };
    Some((*text).to_owned())
}

fn reason_sentence(locale: &str, key: &str) -> String {
    let row = match key {
        "official_app" => (
            "WhatsApp only shows this in the official app.",
            "O WhatsApp só mostra isso no app oficial.",
            "WhatsApp solo muestra esto en la app oficial.",
        ),
        "phone" => (
            "WhatsApp only opens this on the phone.",
            "O WhatsApp só abre isto no celular.",
            "WhatsApp solo abre esto en el celular.",
        ),
        "unknown" => (
            "This message type does not open here yet.",
            "Este tipo de mensagem ainda não abre aqui.",
            "Este tipo de mensaje todavía no se abre aquí.",
        ),
        _ => (
            "This message type does not open here yet.",
            "Este tipo de mensagem ainda não abre aqui.",
            "Este tipo de mensaje todavía no se abre aquí.",
        ),
    };
    match locale {
        "pt" => row.1.to_owned(),
        "es" => row.2.to_owned(),
        _ => row.0.to_owned(),
    }
}

fn view_once_kind(locale: &str, what: &str) -> &'static str {
    match (locale, what) {
        ("pt", "photo") => "foto",
        ("pt", "video") => "vídeo",
        ("pt", "voice message") => "áudio",
        ("es", "photo") => "foto",
        ("es", "video") => "video",
        ("es", "voice message") => "audio",
        (_, "photo") => "photo",
        (_, "video") => "video",
        (_, "voice message") => "voice message",
        ("pt", _) => "mensagem",
        ("es", _) => "mensaje",
        _ => "message",
    }
}

/// `(field, en, pt, es)`. Unlisted fields keep the protobuf name.
const LABELS: &[(&str, &str, &str, &str)] = &[
    ("message", "Message", "Mensagem", "Mensaje"),
    ("send_payment_message", "Payment", "Pagamento", "Pago"),
    (
        "request_payment_message",
        "Payment request",
        "Cobrança",
        "Cobro",
    ),
    (
        "payment_invite_message",
        "Payment invite",
        "Convite de pagamento",
        "Invitación de pago",
    ),
    ("invoice_message", "Invoice", "Fatura", "Factura"),
    ("product_message", "Product", "Produto", "Producto"),
    ("order_message", "Order", "Pedido", "Pedido"),
    (
        "decline_payment_request_message",
        "Declined payment",
        "Pagamento recusado",
        "Pago rechazado",
    ),
    (
        "cancel_payment_request_message",
        "Cancelled payment",
        "Pagamento cancelado",
        "Pago cancelado",
    ),
    (
        "payment_reminder_message",
        "Payment reminder",
        "Lembrete de pagamento",
        "Recordatorio de pago",
    ),
    (
        "split_payment_message",
        "Split payment",
        "Divisão de pagamento",
        "Pago dividido",
    ),
    (
        "split_payment_update_message",
        "Split payment update",
        "Atualização de divisão",
        "Actualización de pago dividido",
    ),
    (
        "newsletter_admin_invite_message",
        "Channel invite",
        "Convite de canal",
        "Invitación de canal",
    ),
    (
        "newsletter_follower_invite_message_v2",
        "Channel invite",
        "Convite de canal",
        "Invitación de canal",
    ),
    (
        "edited_message",
        "Edited message",
        "Mensagem editada",
        "Mensaje editado",
    ),
    (
        "ptv_message",
        "Round video",
        "Vídeo redondo",
        "Video redondo",
    ),
    (
        "group_invite_message",
        "Group invite",
        "Convite de grupo",
        "Invitación de grupo",
    ),
    ("event_message", "Event", "Evento", "Evento"),
    (
        "call_log_messsage",
        "Call",
        "Registro de chamada",
        "Registro de llamada",
    ),
    (
        "lottie_sticker_message",
        "Animated sticker",
        "Figurinha animada",
        "Sticker animado",
    ),
    (
        "sticker_pack_message",
        "Sticker pack",
        "Pacote de figurinhas",
        "Paquete de stickers",
    ),
    (
        "interactive_message",
        "Interactive message",
        "Mensagem interativa",
        "Mensaje interactivo",
    ),
    (
        "scheduled_call_creation_message",
        "Scheduled call",
        "Chamada agendada",
        "Llamada programada",
    ),
    ("music_message", "Music", "Música", "Música"),
    (
        "highly_structured_message",
        "Template",
        "Modelo",
        "Plantilla",
    ),
    (
        "request_phone_number_message",
        "Phone request",
        "Pedido de telefone",
        "Pedido de teléfono",
    ),
    ("comment_message", "Comment", "Comentário", "Comentario"),
    ("bcall_message", "Call", "Chamada", "Llamada"),
    (
        "unreadable",
        "Unreadable message",
        "Mensagem ilegível",
        "Mensaje ilegible",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_ai_notice_names_the_phone() {
        assert_eq!(
            meta_ai_notice("pt-BR"),
            "Para conversar com a Meta AI, use o celular"
        );
        assert_eq!(
            meta_ai_notice("es-MX"),
            "Para conversar con Meta AI, usa el celular"
        );
        assert!(meta_ai_notice("en").contains("phone"));
    }

    #[test]
    fn payment_product_and_channel_name_the_reason() {
        let payment = notice("pt-BR", "send_payment_message", "official_app");
        assert_eq!(payment.title, "Pagamento");
        assert_eq!(payment.reason, "O WhatsApp só mostra isso no app oficial.");
        assert!(payment.open_on_phone);
        assert_eq!(open_on_phone("pt-BR"), "Abrir no celular");

        let product = notice("pt", "product_message", "official_app");
        assert_eq!(product.title, "Produto");

        let order = notice("es-MX", "order_message", "official_app");
        assert_eq!(order.title, "Pedido");
        assert!(order.reason.contains("app oficial"));

        let channel = notice("pt_PT", "newsletter_admin_invite_message", "phone");
        assert_eq!(channel.title, "Convite de canal");
        assert!(channel.open_on_phone);

        let edited = notice("pt", "edited_message", "unknown");
        assert_eq!(edited.title, "Mensagem editada");
        assert!(!edited.open_on_phone);
    }

    #[test]
    fn an_unknown_field_keeps_its_protobuf_name() {
        let notice = notice("pt-BR", "some_new_message", "unknown");
        assert_eq!(notice.title, "some_new_message");
        assert_ne!(notice.title, "message");
        assert!(!notice.reason.is_empty());
    }

    #[test]
    fn view_once_says_the_phone_keeps_it() {
        let photo = view_once("pt-BR", "photo", false);
        assert_eq!(photo.title, "Visualização única (foto)");
        assert!(photo.reason.contains("privacidade"));
        assert!(photo.open_on_phone);
        let voice = view_once("es", "voice message", false);
        assert!(voice.title.contains("audio"));
        let once = view_once("pt-BR", "photo", true);
        assert!(!once.open_on_phone);
        assert!(once.reason.contains("não fica guardada"));
        assert_eq!(view_once_button("pt-BR"), "Ver uma vez");
    }

    #[test]
    fn a_missed_voice_call_is_named() {
        assert_eq!(
            call_title("pt-BR", false, "missed"),
            "Chamada de voz perdida"
        );
        assert_eq!(
            call_title("es", true, "answered"),
            "Llamada de video atendida"
        );
    }
}
