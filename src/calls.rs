//! Incoming-call notices. This device does not place or answer a call.
//!
//! A ring is one short tone. A muted chat and system quiet hours skip it.

use crate::model::{Content, Delivery, Message};

/// Whether the short ring should play.
pub fn should_ring(muted: bool, quiet_hours: bool) -> bool {
    !muted && !quiet_hours
}

/// Windows quiet hours (`QUNS_QUIET_TIME`). Other states still ring.
pub fn quiet_hours(state: i32) -> bool {
    state == 6
}

/// A second offer for a call that is already ringing must not alert again.
pub fn repeat_ring(already_seen: bool) -> bool {
    already_seen
}

/// Windows quiet hours. Linux and macOS have no equally small check here,
/// so those builds return false and the chat mute still applies.
pub fn system_dnd() -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Shell::SHQueryUserNotificationState;
        let mut state = 0;
        let result = unsafe { SHQueryUserNotificationState(&mut state) };
        result == 0 && quiet_hours(state)
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// A name safe to show. A string that is mostly digits stays off the screen.
pub fn caller_label(notify: Option<&str>, known: Option<&str>) -> String {
    for candidate in [known, notify].into_iter().flatten() {
        let trimmed = candidate.trim();
        if !trimmed.is_empty() && !looks_like_number(trimmed) {
            return trimmed.to_owned();
        }
    }
    "WhatsApp".to_owned()
}

fn looks_like_number(value: &str) -> bool {
    let digits = value.chars().filter(|char| char.is_ascii_digit()).count();
    digits >= 8 && digits * 2 >= value.chars().count()
}

/// Notification title and body. The body names voice or video, never a number.
pub fn notify_lines(locale: &str, name: &str, video: bool) -> (String, String) {
    let locale = crate::i18n::message_locale_tag(locale);
    let kind = match locale {
        "pt" => {
            if video {
                "vídeo"
            } else {
                "voz"
            }
        }
        "es" => {
            if video {
                "video"
            } else {
                "voz"
            }
        }
        _ => {
            if video {
                "video"
            } else {
                "voice"
            }
        }
    };
    let body = match locale {
        "pt" => format!("{name} está ligando ({kind})"),
        "es" => format!("{name} está llamando ({kind})"),
        _ => format!("{name} is calling ({kind})"),
    };
    (name.to_owned(), body)
}

/// Banner copy: answer on the phone, and decline when the protocol allows it.
pub fn banner_copy(locale: &str) -> (&'static str, &'static str) {
    match crate::i18n::message_locale_tag(locale) {
        "pt" => ("Atender no celular", "Recusar"),
        "es" => ("Contestar en el celular", "Rechazar"),
        _ => ("Answer on your phone", "Decline"),
    }
}

/// Confirmation before a linked device sends a reject.
pub fn reject_copy(locale: &str) -> (&'static str, &'static str, &'static str, &'static str) {
    match crate::i18n::message_locale_tag(locale) {
        "pt" => (
            "Recusar esta chamada?",
            "O celular deixa de tocar. A conversa não começa neste computador.",
            "Recusar",
            "Cancelar",
        ),
        "es" => (
            "¿Rechazar esta llamada?",
            "El celular deja de sonar. La conversación no empieza en este equipo.",
            "Rechazar",
            "Cancelar",
        ),
        _ => (
            "Decline this call?",
            "The phone stops ringing. The conversation does not start on this computer.",
            "Decline",
            "Cancel",
        ),
    }
}

/// A missed call filed as a chat row. The id is stable for this signaling id.
pub fn missed_message(
    chat: &str,
    call_id: &str,
    name: &str,
    video: bool,
    timestamp: i64,
) -> Message {
    Message {
        id: format!("call-{call_id}"),
        chat: chat.to_owned(),
        sender: chat.to_owned(),
        sender_name: Some(name.to_owned()),
        from_me: false,
        timestamp,
        content: Content::CallLog {
            video,
            outcome: "missed".to_owned(),
            seconds: None,
            scheduled: false,
        },
        status: Delivery::None,
        delivered_at: None,
        read_at: None,
        quoted: None,
        reactions: Vec::new(),
        edited: false,
        mentions: Vec::new(),
        forwarded: false,
        thumbnail: None,
    }
}

/// One short tone. Failures stay quiet: a missing device is not an error toast.
pub fn play_ring() {
    let _ = std::thread::Builder::new()
        .name("call-ring".into())
        .spawn(|| {
            let Ok(device) = rodio::DeviceSinkBuilder::open_default_sink() else {
                return;
            };
            let player = rodio::Player::connect_new(device.mixer());
            let channels = std::num::NonZero::new(1).expect("one channel");
            let rate = std::num::NonZero::new(48_000).expect("rate");
            player.append(rodio::buffer::SamplesBuffer::new(
                channels,
                rate,
                ring_samples(),
            ));
            player.sleep_until_end();
        });
}

fn ring_samples() -> Vec<f32> {
    const RATE: f32 = 48_000.0;
    const SECONDS: f32 = 0.35;
    let count = (RATE * SECONDS) as usize;
    (0..count)
        .map(|index| {
            let t = index as f32 / RATE;
            let fade = (1.0 - t / SECONDS).clamp(0.0, 1.0);
            (t * 440.0 * std::f32::consts::TAU).sin() * 0.2 * fade
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_muted_or_quiet_chat_does_not_ring() {
        assert!(should_ring(false, false));
        assert!(!should_ring(true, false));
        assert!(!should_ring(false, true));
        assert!(!repeat_ring(false));
        assert!(repeat_ring(true));
        assert!(quiet_hours(6));
        assert!(!quiet_hours(0));
    }

    #[test]
    fn a_phone_like_name_is_not_shown() {
        assert_eq!(caller_label(Some("5511999999999"), None), "WhatsApp");
        assert_eq!(caller_label(Some("+55 11 99999-9999"), Some("Ada")), "Ada");
        assert_eq!(caller_label(Some("Ada"), None), "Ada");
    }

    #[test]
    fn a_missed_call_is_a_chat_row() {
        let message = missed_message("ada@s.whatsapp.net", "abc", "Ada", true, 1_700_000_000);
        assert_eq!(message.id, "call-abc");
        assert!(!message.from_me);
        match message.content {
            Content::CallLog {
                video,
                outcome,
                seconds,
                ..
            } => {
                assert!(video);
                assert_eq!(outcome, "missed");
                assert!(seconds.is_none());
            }
            other => panic!("expected a call log, got {other:?}"),
        }
        let (title, body) = notify_lines("pt", "Ada", false);
        assert_eq!(title, "Ada");
        assert_eq!(body, "Ada está ligando (voz)");
        assert_eq!(banner_copy("pt").0, "Atender no celular");
    }
}
