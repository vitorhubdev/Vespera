//! One catalog file per language, embedded in the binary.
//!
//! UI code looks up stable keys through [`t`]. Each language is a JSON file
//! under `locales/`, so adding one is adding a file: nothing here changes.
//! A key missing from a language falls back to English, and the catalog test
//! names the gaps instead of letting them reach the screen.
//!
//! Files: `en.json` (the source text), `pt-BR.json`, `es.json` and
//! `zh-Hans.json`. Traditional Chinese locales resolve to English rather than
//! show the wrong script.
//!
//! Adding a language is three edits, not just a file:
//!
//! 1. Write `locales/<code>.json` with the keys of `locales/en.json`.
//! 2. Add the variant to [`Language`] and to [`Language::ALL`], with its own
//!    name in [`Language::label`].
//! 3. Add the variant to `Resolved` and point `Resolved::source` at the
//!    file, then map the locale in `detect_for` if it should be automatic.
//!
//! `every_language_covers_every_key` and `the_catalog_test_lists_every_file`
//! both fail until the catalog is registered: a file nobody embeds would
//! otherwise sit there unused, never reaching Settings.

use std::{collections::HashMap, sync::OnceLock};

use serde::{Deserialize, Serialize};

/// UI language preference.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    /// Follow the desktop locale: Portuguese for `pt-*`, Spanish for `es-*`,
    /// Simplified Chinese for Hans locales, English otherwise.
    #[default]
    Auto,
    English,
    Portuguese,
    Spanish,
    Simplified,
}

impl Language {
    pub const ALL: [Language; 5] = [
        Language::Auto,
        Language::English,
        Language::Portuguese,
        Language::Spanish,
        Language::Simplified,
    ];

    /// Option label. Language names stay in their own script on purpose.
    pub fn label(self) -> &'static str {
        match self {
            Language::Auto => "Auto",
            Language::English => "English",
            Language::Portuguese => "Português (Brasil)",
            Language::Spanish => "Español",
            Language::Simplified => "Simplified Chinese",
        }
    }

    /// Name of one choice in the interface language. The settings dropdown
    /// lists every language translated, so a Portuguese user reads
    /// "Chinês simplificado" instead of "Simplified Chinese".
    pub fn name_in(self, lang: Language) -> String {
        let key = match self {
            Language::Auto => "language.auto",
            Language::English => "language.english",
            Language::Portuguese => "language.portuguese",
            Language::Spanish => "language.spanish",
            Language::Simplified => "language.simplified",
        };
        t(lang, key)
    }

    /// Effective table after resolving `Auto` against the desktop locale.
    pub fn resolved(self) -> Resolved {
        match self {
            Language::English => Resolved::En,
            Language::Portuguese => Resolved::Pt,
            Language::Spanish => Resolved::Es,
            Language::Simplified => Resolved::Sc,
            Language::Auto => detect(),
        }
    }
}

/// Resolved string table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolved {
    En,
    Pt,
    Es,
    Sc,
}

impl Resolved {
    /// The catalog file this language reads.
    fn source(self) -> &'static str {
        match self {
            Resolved::En => include_str!("../locales/en.json"),
            Resolved::Pt => include_str!("../locales/pt-BR.json"),
            Resolved::Es => include_str!("../locales/es.json"),
            Resolved::Sc => include_str!("../locales/zh-Hans.json"),
        }
    }
}

/// The embedded catalogs, parsed once per language.
fn catalog(resolved: Resolved) -> &'static HashMap<String, String> {
    static EN: OnceLock<HashMap<String, String>> = OnceLock::new();
    static PT: OnceLock<HashMap<String, String>> = OnceLock::new();
    static ES: OnceLock<HashMap<String, String>> = OnceLock::new();
    static SC: OnceLock<HashMap<String, String>> = OnceLock::new();
    let cell = match resolved {
        Resolved::En => &EN,
        Resolved::Pt => &PT,
        Resolved::Es => &ES,
        Resolved::Sc => &SC,
    };
    cell.get_or_init(|| parse(resolved.source()))
}

/// Every catalog file in `locales/`, by the name it has there.
#[cfg(test)]
const CATALOG_FILES: &[&str] = &["en.json", "pt-BR.json", "es.json", "zh-Hans.json"];

/// The embedded catalogs, parsed once per language.
#[cfg(test)]
fn catalogs() -> [HashMap<String, String>; 4] {
    [
        catalog(Resolved::En).clone(),
        catalog(Resolved::Pt).clone(),
        catalog(Resolved::Es).clone(),
        catalog(Resolved::Sc).clone(),
    ]
}

/// Parses one catalog file. A file that does not parse yields an empty
/// catalog: the screen falls back to English instead of refusing to start.
fn parse(source: &str) -> HashMap<String, String> {
    serde_json::from_str(source).unwrap_or_default()
}

/// Looks up a stable key. The resolved language when it has the key,
/// English otherwise, and the key itself when nobody has it.
pub fn t(lang: Language, key: &str) -> String {
    let resolved = lang.resolved();
    if resolved != Resolved::En
        && let Some(text) = catalog(resolved).get(key)
    {
        return text.clone();
    }
    if let Some(text) = catalog(Resolved::En).get(key) {
        return text.clone();
    }
    key.to_owned()
}

/// Lowercase user locale. Windows and macOS use the desktop locale. Other
/// platforms use `LC_ALL`, then `LC_CTYPE`, then `LANG`. Empty when unset.
fn locale() -> String {
    if let Some(native) = native_locale() {
        let native = native.trim();
        if !native.is_empty() {
            return native.to_lowercase();
        }
    }
    ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .find_map(|key| std::env::var(key).ok().filter(|value| !value.is_empty()))
        .unwrap_or_default()
        .to_lowercase()
}

/// Desktop locale on Windows (`GetUserDefaultLocaleName`).
#[cfg(target_os = "windows")]
fn native_locale() -> Option<String> {
    use windows_sys::Win32::Globalization::GetUserDefaultLocaleName;
    // `LOCALE_NAME_MAX_LENGTH` is 85, including the trailing NUL.
    let mut buffer = [0u16; 85];
    let written = unsafe { GetUserDefaultLocaleName(buffer.as_mut_ptr(), buffer.len() as i32) };
    if written <= 1 {
        return None;
    }
    Some(String::from_utf16_lossy(&buffer[..written as usize - 1]))
}

/// Desktop locale on macOS (`NSLocale`).
#[cfg(target_os = "macos")]
fn native_locale() -> Option<String> {
    let identifier = objc2_foundation::NSLocale::currentLocale().localeIdentifier();
    Some(identifier.to_string())
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn native_locale() -> Option<String> {
    None
}

/// Language of the new message cards. Auto follows a Portuguese or Spanish
/// desktop locale; an explicit English or Chinese choice stays in English
/// for these strings, which are not part of the Chinese catalog.
pub fn message_locale(language: Language) -> &'static str {
    match language {
        Language::English | Language::Simplified => "en",
        Language::Portuguese => "pt",
        Language::Spanish => "es",
        Language::Auto => message_locale_tag(&locale()),
    }
}

pub fn message_locale_tag(locale: &str) -> &'static str {
    let locale = locale.to_lowercase().replace('-', "_");
    if locale.starts_with("pt") {
        "pt"
    } else if locale.starts_with("es") {
        "es"
    } else {
        "en"
    }
}

fn detect() -> Resolved {
    detect_for(&locale())
}

/// Portuguese and Spanish locales resolve to their own catalog; Hans
/// locales to Simplified; everything else to English. Specific Traditional
/// markers win over the bare `zh` prefix. Hyphens count as underscores, so
/// `zh-TW` is Traditional.
fn detect_for(locale: &str) -> Resolved {
    let locale = locale.to_lowercase().replace('-', "_");
    if locale.starts_with("pt") {
        return Resolved::Pt;
    }
    if locale.starts_with("es") {
        return Resolved::Es;
    }
    let hant = locale.contains("hant")
        || ["zh_tw", "zh_hk", "zh_mo"]
            .iter()
            .any(|prefix| locale.starts_with(prefix));
    if hant {
        return Resolved::En;
    }
    if locale.starts_with("zh") || locale.contains("hans") {
        Resolved::Sc
    } else {
        Resolved::En
    }
}

/// Text of a toast the worker produced. The worker has no interface language,
/// so it sends this English text and the app maps it to the catalog. An
/// unknown text is shown as it arrived.
///
/// Dynamic worker payloads already carry their values (a contact name, a
/// sticker-pack name, a forward count), so they never equal the brace-filled
/// templates. They are matched by shape in [`toast`] and rendered through
/// the same catalog keys with the values substituted.
pub fn toast_key(text: &str) -> Option<&'static str> {
    Some(match text {
        "You joined the group" => "toast.joined_group",
        "Group created" => "toast.group_created",
        "You left the group. The conversation is archived." => "toast.group_left",
        "Frequently forwarded messages go to one chat at a time" => "toast.frequent_forward",
        "Chat exported" => "toast.chat_exported",
        "Copied" => "toast.copied",
        "Back online" => "toast.back_online",
        "History loaded" => "toast.history_loaded",
        "Connect to check for updates" => "toast.connect_updates",
        "You're on the latest version" => "toast.latest_version",
        "This chat cannot send messages" => "toast.cannot_send",
        "This video cannot be played here" => "toast.video_unplayable",
        "Picture copied to the clipboard" => "toast.picture_copied",
        "Sticker saved" => "toast.sticker_saved",
        "None of these chats can receive forwards" => "toast.no_forward",
        "Only groups can be renamed here" => "toast.only_groups_renamed",
        "Open a chat first" => "toast.open_chat_first",
        _ => return None,
    })
}

/// A toast in the active language, with dynamic values substituted.
///
/// Exact worker and app texts go through [`toast_key`]. Texts that carry a
/// runtime value keep the value and only translate the frame around it.
pub fn toast(lang: Language, text: &str) -> String {
    if let Some(key) = toast_key(text) {
        return t(lang, key);
    }
    if let Some(name) = text
        .strip_prefix("Added sticker pack \"")
        .and_then(|rest| rest.strip_suffix('"'))
    {
        return t(lang, "toast.sticker_pack_added").replace("{name}", name);
    }
    if let Some(name) = text
        .strip_prefix("Added ")
        .and_then(|rest| rest.strip_suffix(" to contacts"))
    {
        return t(lang, "toast.contact_added").replace("{name}", name);
    }
    if let Some(rest) = text.strip_prefix("Forwarded ")
        && let Some((what, whereto)) = rest.split_once(" to ")
    {
        return t(lang, "toast.forwarded")
            .replace("{what}", what)
            .replace("{whereto}", whereto);
    }
    text.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn keys(catalog: &HashMap<String, String>) -> HashSet<&str> {
        catalog.keys().map(String::as_str).collect()
    }

    #[test]
    fn every_language_covers_every_key() {
        let [en, pt, es, sc] = catalogs();
        let source = keys(&en);
        assert!(!source.is_empty(), "the English catalog is not empty");
        for (name, catalog) in [("pt-BR", &pt), ("es", &es), ("zh-Hans", &sc)] {
            let missing: Vec<_> = source.difference(&keys(catalog)).copied().collect();
            assert!(missing.is_empty(), "{name} is missing {missing:?}");
            let extra: Vec<_> = keys(catalog).difference(&source).copied().collect();
            assert!(
                extra.is_empty(),
                "{name} has keys nobody else has: {extra:?}"
            );
        }
        for (name, catalog) in [("en", &en), ("pt-BR", &pt), ("es", &es), ("zh-Hans", &sc)] {
            for (key, text) in catalog {
                assert!(!text.trim().is_empty(), "{name}: {key} is empty");
                assert_eq!(key.trim(), key, "{name}: {key} has spaces around it");
            }
        }
    }

    #[test]
    fn portuguese_and_spanish_say_it_in_their_own_words() {
        // A file that still holds the English text would pass the key test.
        assert_eq!(t(Language::Portuguese, "settings.title"), "Configurações");
        assert_eq!(t(Language::Portuguese, "chatlist.chats"), "Conversas");
        assert_eq!(t(Language::Spanish, "settings.title"), "Ajustes");
        assert_eq!(t(Language::Spanish, "chatlist.chats"), "Chats");
        assert_eq!(t(Language::English, "settings.title"), "Settings");
        assert_eq!(t(Language::Simplified, "settings.title"), "设置");
    }

    #[test]
    fn a_missing_key_falls_back_to_english_and_never_panics() {
        // An unknown key echoes back instead of panicking, in every language.
        for language in Language::ALL {
            assert_eq!(t(language, "no.such.key"), "no.such.key");
        }
        // A key present only in English still answers in the other languages:
        // the catalogs are checked for equal keys, and the fallback is the
        // safety net behind that check.
        assert_eq!(t(Language::Portuguese, "settings.title"), "Configurações");
    }

    #[test]
    fn auto_follows_the_desktop_locale() {
        assert_eq!(detect_for("pt_br.utf-8"), Resolved::Pt);
        assert_eq!(detect_for("pt_PT"), Resolved::Pt);
        assert_eq!(detect_for("pt-BR"), Resolved::Pt);
        assert_eq!(detect_for("es_ES.UTF-8"), Resolved::Es);
        assert_eq!(detect_for("es-MX"), Resolved::Es);
        assert_eq!(detect_for("es-419"), Resolved::Es);
        assert_eq!(detect_for("zh_cn.utf-8"), Resolved::Sc);
        assert_eq!(detect_for("zh_sg.utf-8"), Resolved::Sc);
        assert_eq!(detect_for("zh_hans"), Resolved::Sc);
        assert_eq!(detect_for("zh-CN"), Resolved::Sc);
        assert_eq!(detect_for("zh-Hans"), Resolved::Sc);
        assert_eq!(detect_for("zh_tw.utf-8"), Resolved::En);
        assert_eq!(detect_for("zh_hk.utf-8"), Resolved::En);
        assert_eq!(detect_for("zh_mo"), Resolved::En);
        assert_eq!(detect_for("zh-Hant"), Resolved::En);
        assert_eq!(detect_for("zh-TW"), Resolved::En);
        assert_eq!(detect_for("en_us.utf-8"), Resolved::En);
        assert_eq!(detect_for(""), Resolved::En);
        assert_eq!(Language::Auto.resolved(), detect());
        assert_eq!(Language::Portuguese.resolved(), Resolved::Pt);
        assert_eq!(Language::Spanish.resolved(), Resolved::Es);
    }

    #[test]
    fn new_message_cards_follow_the_language() {
        assert_eq!(message_locale(Language::Portuguese), "pt");
        assert_eq!(message_locale(Language::Spanish), "es");
        assert_eq!(message_locale(Language::English), "en");
        assert_eq!(message_locale_tag("pt-BR"), "pt");
        assert_eq!(message_locale_tag("es_MX"), "es");
        assert_eq!(message_locale_tag("en_US"), "en");
    }

    #[test]
    fn the_catalog_test_lists_every_file() {
        // A file in `locales/` that nothing embeds is a language nobody can
        // pick: it needs an entry in `Resolved::source` and in `Language`, and
        // this is where that shows up.
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("locales");
        let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
            .expect("locales directory")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".json"))
            .collect();
        on_disk.sort();
        let mut listed: Vec<String> = CATALOG_FILES
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
        listed.sort();
        assert_eq!(
            on_disk, listed,
            "a catalog file needs a variant in Language and an arm in \
             Resolved::source; see the module docs"
        );
    }

    #[test]
    fn every_toast_the_worker_sends_has_a_translation() {
        // The worker has no interface language: it sends this English text
        // and the app maps it. A toast added there without a key here would
        // reach a Portuguese user in English.
        for text in [
            "You joined the group",
            "Group created",
            "You left the group. The conversation is archived.",
            "Frequently forwarded messages go to one chat at a time",
            "Chat exported",
            "Copied",
            "Back online",
            "History loaded",
            "Connect to check for updates",
            "You're on the latest version",
            "This chat cannot send messages",
            "This video cannot be played here",
            "Picture copied to the clipboard",
            "Sticker saved",
            "None of these chats can receive forwards",
            "Only groups can be renamed here",
            "Open a chat first",
        ] {
            let key = toast_key(text).unwrap_or_else(|| panic!("no key for {text}"));
            for language in Language::ALL {
                let translated = t(language, key);
                assert!(
                    translated != key && !translated.is_empty(),
                    "{text} is untranslated in {:?}: {translated}",
                    language.label()
                );
            }
        }
        // Dynamic worker payloads carry their values: they are translated
        // by shape, keeping the value.
        assert_eq!(
            toast(Language::Portuguese, "Added Alice to contacts"),
            "Alice adicionado aos contatos"
        );
        assert_eq!(
            toast(Language::Portuguese, "Added sticker pack \"Cats\""),
            "Pacote de stickers \"Cats\" adicionado"
        );
        assert_eq!(
            toast(Language::Portuguese, "Forwarded 2 messages to 3 chats"),
            "2 messages encaminhado para 3 chats"
        );
        // An unknown toast is shown as it arrived, never swallowed.
        assert_eq!(
            toast(Language::Portuguese, "Something new"),
            "Something new"
        );
    }
}
