//! Group administration helpers that do not touch the protocol.
//!
//! Phone numbers stay out of errors. The server reason is a status word.

/// Turns a list of typed numbers into canonical chat ids.
///
/// Pieces shorter than eight digits are skipped. Separators are commas,
/// semicolons, and new lines. Spaces stay inside one number.
pub fn phones(text: &str) -> Vec<String> {
    text.split([',', ';', '\n'])
        .filter_map(|part| {
            let digits: String = part.chars().filter(char::is_ascii_digit).collect();
            (digits.len() >= 8).then(|| format!("{digits}@s.whatsapp.net"))
        })
        .collect()
}

/// The sentence shown when WhatsApp refuses a participant change.
///
/// Only the server status words are included.
pub fn participant_reason(errors: &[String]) -> String {
    let reason = errors
        .iter()
        .map(|error| error.trim())
        .filter(|error| !error.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    if reason.is_empty() {
        "WhatsApp refused that change".to_owned()
    } else {
        format!("WhatsApp refused that change: {reason}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phones_keep_long_numbers_and_drop_the_rest() {
        let found = phones(" +1 555-0100, short, 5511999990000; ");
        assert_eq!(
            found,
            vec![
                "15550100@s.whatsapp.net".to_owned(),
                "5511999990000@s.whatsapp.net".to_owned()
            ]
        );
    }

    #[test]
    fn a_refusal_uses_the_server_word_only() {
        assert_eq!(
            participant_reason(&["403".into(), "  ".into()]),
            "WhatsApp refused that change: 403"
        );
        assert_eq!(participant_reason(&[]), "WhatsApp refused that change");
    }
}
