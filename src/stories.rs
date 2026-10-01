//! Status updates (stories): grouping, expiry, audience, and cache limits.
//!
//! A status lasts 24 hours. Unseen contacts come first. Media files stay in
//! their own cache and are dropped oldest-first once the cap is passed.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// How long a status stays visible, in Unix seconds.
pub const TTL_SECS: i64 = 24 * 60 * 60;
/// Files kept for status media. Oldest leave first.
pub const CACHE_MAX_FILES: usize = 48;
/// Byte cap for the status cache, beside the file count.
pub const CACHE_MAX_BYTES: u64 = 200 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Privacy {
    Contacts,
    Allow,
    Deny,
}

impl Privacy {
    pub const ALL: [Privacy; 3] = [Privacy::Contacts, Privacy::Allow, Privacy::Deny];
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum StoryKind {
    Text {
        text: String,
        background: u32,
        font: i32,
    },
    Image {
        caption: Option<String>,
    },
    Video {
        caption: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Story {
    pub id: String,
    pub sender: String,
    pub sender_name: Option<String>,
    pub from_me: bool,
    pub timestamp: i64,
    pub kind: StoryKind,
    pub seen: bool,
    pub path: Option<PathBuf>,
}

impl Story {
    pub fn preview(&self) -> String {
        match &self.kind {
            StoryKind::Text { text, .. } => text.clone(),
            StoryKind::Image { caption } | StoryKind::Video { caption } => caption
                .clone()
                .filter(|text| !text.is_empty())
                .unwrap_or_else(|| {
                    if matches!(self.kind, StoryKind::Video { .. }) {
                        "Video".to_owned()
                    } else {
                        "Photo".to_owned()
                    }
                }),
        }
    }

    pub fn needs_file(&self) -> bool {
        matches!(self.kind, StoryKind::Image { .. } | StoryKind::Video { .. })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct StoryGroup {
    pub sender: String,
    pub name: String,
    pub stories: Vec<Story>,
    pub unseen: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoryView {
    pub sender: String,
    pub index: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StatusDraft {
    pub text: String,
    pub background: u32,
    pub font: i32,
    pub privacy: Privacy,
    pub picked: Vec<String>,
    pub image: Option<PathBuf>,
    /// The user has seen the publish question and has not sent yet.
    pub confirm: bool,
}

impl Default for StatusDraft {
    fn default() -> Self {
        Self {
            text: String::new(),
            background: 0xFF1E6E4F,
            font: 0,
            privacy: Privacy::Contacts,
            picked: Vec::new(),
            image: None,
            confirm: false,
        }
    }
}

impl StatusDraft {
    pub fn can_publish(&self) -> bool {
        self.image.is_some() || !self.text.trim().is_empty()
    }
}

/// Backgrounds offered when posting a text status. Values are 0xAARRGGBB.
pub const BACKGROUNDS: [u32; 6] = [
    0xFF1E6E4F, 0xFF111B21, 0xFF027EB5, 0xFF5E35B1, 0xFFC62828, 0xFFF9F7F3,
];

pub fn alive(timestamp: i64, now: i64) -> bool {
    now.saturating_sub(timestamp) < TTL_SECS
}

pub fn is_person(id: &str) -> bool {
    let Some((user, server)) = id.rsplit_once('@') else {
        return false;
    };
    !user.is_empty()
        && !user.eq_ignore_ascii_case("status")
        && matches!(server, "s.whatsapp.net" | "lid")
}

/// A saved address-book contact who can receive a status. The user's own
/// id and a push name with no saved name are left out.
pub fn address_book(id: &str, full_name: Option<&str>, is_me: bool) -> bool {
    !is_me && is_person(id) && full_name.is_some_and(|name| !name.trim().is_empty())
}

/// Who a status is encrypted to. Groups, channels, and the status address
/// are never included. An allow list is only the picked people. A deny list
/// is everyone except the picked people.
pub fn audience(people: &[String], privacy: Privacy, picked: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    match privacy {
        Privacy::Allow => {
            for id in picked {
                if is_person(id) && !out.iter().any(|known| known == id) {
                    out.push(id.clone());
                }
            }
        }
        Privacy::Contacts | Privacy::Deny => {
            for id in people {
                if !is_person(id) || out.iter().any(|known| known == id) {
                    continue;
                }
                if privacy == Privacy::Deny && picked.iter().any(|skip| skip == id) {
                    continue;
                }
                out.push(id.clone());
            }
        }
    }
    out
}

pub fn groups(stories: &[Story], now: i64) -> Vec<StoryGroup> {
    let mut by_sender: BTreeMap<&str, Vec<&Story>> = BTreeMap::new();
    for story in stories {
        if alive(story.timestamp, now) {
            by_sender
                .entry(story.sender.as_str())
                .or_default()
                .push(story);
        }
    }
    let mut groups = Vec::new();
    for (sender, mut rows) in by_sender {
        rows.sort_by_key(|story| (story.timestamp, story.id.as_str()));
        let unseen = rows
            .iter()
            .filter(|story| !story.seen && !story.from_me)
            .count();
        let name = rows
            .iter()
            .rev()
            .find_map(|story| story.sender_name.clone())
            .unwrap_or_else(|| sender.to_owned());
        groups.push(StoryGroup {
            sender: sender.to_owned(),
            name,
            stories: rows.into_iter().cloned().collect(),
            unseen,
        });
    }
    groups.sort_by(|left, right| {
        right
            .unseen
            .min(1)
            .cmp(&left.unseen.min(1))
            .then_with(|| {
                let left_at = left
                    .stories
                    .last()
                    .map(|story| story.timestamp)
                    .unwrap_or(0);
                let right_at = right
                    .stories
                    .last()
                    .map(|story| story.timestamp)
                    .unwrap_or(0);
                right_at.cmp(&left_at)
            })
            .then_with(|| left.sender.cmp(&right.sender))
    });
    groups
}

/// The name shown for a contact's status. The user's own updates use the
/// localized word, never the account id.
pub fn contact_label<'a>(stories: &'a [Story], fallback: &'a str, you: &'a str) -> &'a str {
    if !stories.is_empty() && stories.iter().all(|story| story.from_me) {
        you
    } else {
        fallback
    }
}

/// Moves inside the open contact, then to the next or previous contact.
/// Forward past the last status closes the viewer.
pub fn step(groups: &[StoryGroup], view: &StoryView, delta: i32) -> Option<StoryView> {
    let index = groups
        .iter()
        .position(|group| group.sender == view.sender)?;
    let group = &groups[index];
    if delta >= 0 {
        let next = view.index.saturating_add(delta as usize);
        if next < group.stories.len() {
            return Some(StoryView {
                sender: view.sender.clone(),
                index: next,
            });
        }
        let following = &groups.get(index + 1)?;
        return Some(StoryView {
            sender: following.sender.clone(),
            index: 0,
        });
    }
    let back = delta.unsigned_abs() as usize;
    if view.index >= back {
        return Some(StoryView {
            sender: view.sender.clone(),
            index: view.index - back,
        });
    }
    let previous = groups.get(index.checked_sub(1)?)?;
    Some(StoryView {
        sender: previous.sender.clone(),
        index: previous.stories.len().saturating_sub(1),
    })
}

pub fn current<'a>(groups: &'a [StoryGroup], view: &StoryView) -> Option<&'a Story> {
    groups
        .iter()
        .find(|group| group.sender == view.sender)?
        .stories
        .get(view.index)
}

/// A read receipt goes out only when the user left confirmation on, and
/// never for the user's own status.
pub fn send_seen_receipt(setting_on: bool, from_me: bool) -> bool {
    setting_on && !from_me
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CachedFile {
    pub path: PathBuf,
    pub modified: i64,
    pub bytes: u64,
}

/// Paths to delete so the cache stays inside both caps. Newest files stay.
pub fn trim_cache(files: &[CachedFile], max_files: usize, max_bytes: u64) -> Vec<PathBuf> {
    let mut ordered = files.to_vec();
    ordered.sort_by(|left, right| {
        right
            .modified
            .cmp(&left.modified)
            .then_with(|| left.path.cmp(&right.path))
    });
    let mut kept = 0usize;
    let mut bytes = 0u64;
    let mut drop = Vec::new();
    for file in ordered {
        let next = bytes.saturating_add(file.bytes);
        if kept < max_files && next <= max_bytes {
            kept += 1;
            bytes = next;
        } else {
            drop.push(file.path);
        }
    }
    drop
}

/// Dark ink on a light background, light ink otherwise.
pub fn dark_ink(argb: u32) -> bool {
    let red = (argb >> 16) & 0xff;
    let green = (argb >> 8) & 0xff;
    let blue = argb & 0xff;
    red * 3 + green * 6 + blue > 1_200
}

pub fn phrase<'a>(locale: &str, key: &'a str) -> &'a str {
    let row: &[(&str, &str, &str, &str)] = &[
        ("title", "Status", "Estados", "Estados"),
        (
            "empty",
            "No status from the last 24 hours.",
            "Nenhum estado nas últimas 24 horas.",
            "Ningún estado en las últimas 24 horas.",
        ),
        ("post", "New status", "Novo estado", "Nuevo estado"),
        ("publish", "Publish", "Publicar", "Publicar"),
        ("cancel", "Cancel", "Cancelar", "Cancelar"),
        (
            "confirm",
            "Publish this status now?",
            "Publicar este estado agora?",
            "¿Publicar este estado ahora?",
        ),
        ("contacts", "My contacts", "Meus contatos", "Mis contactos"),
        (
            "allow",
            "Only selected",
            "Só os selecionados",
            "Solo los seleccionados",
        ),
        (
            "deny",
            "Contacts except",
            "Contatos, exceto",
            "Contactos, excepto",
        ),
        ("reply", "Reply", "Responder", "Responder"),
        ("photo", "Photo", "Foto", "Foto"),
        ("text", "Text", "Texto", "Texto"),
        ("downloading", "Downloading…", "Baixando…", "Descargando…"),
        (
            "retry",
            "Could not download. Click to retry.",
            "Não foi possível baixar. Clique para tentar de novo.",
            "No se pudo descargar. Clic para reintentar.",
        ),
        (
            "no_contacts",
            "No contacts to send this status to.",
            "Não há contatos para enviar este estado.",
            "No hay contactos para enviar este estado.",
        ),
        (
            "published",
            "Status published.",
            "Estado publicado.",
            "Estado publicado.",
        ),
        ("publishing", "Publishing…", "Publicando…", "Publicando…"),
        ("back", "Chats", "Conversas", "Chats"),
        (
            "open_file",
            "Open in the default app",
            "Abrir no aplicativo padrão",
            "Abrir en la aplicación predeterminada",
        ),
        ("you", "You", "Você", "Tú"),
        (
            "privacy",
            "Who can see it",
            "Quem pode ver",
            "Quién puede verlo",
        ),
        ("pick", "Choose photo", "Escolher foto", "Elegir foto"),
        ("clear_photo", "Remove photo", "Tirar foto", "Quitar foto"),
    ];
    let index = match locale {
        "pt" => 2,
        "es" => 3,
        _ => 1,
    };
    row.iter()
        .find(|(known, _, _, _)| *known == key)
        .map(|labels| match index {
            2 => labels.2,
            3 => labels.3,
            _ => labels.1,
        })
        .unwrap_or(key)
}

pub fn privacy_label(locale: &str, privacy: Privacy) -> &str {
    match privacy {
        Privacy::Contacts => phrase(locale, "contacts"),
        Privacy::Allow => phrase(locale, "allow"),
        Privacy::Deny => phrase(locale, "deny"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn story(id: &str, sender: &str, at: i64, seen: bool) -> Story {
        Story {
            id: id.into(),
            sender: sender.into(),
            sender_name: Some(format!("N{sender}")),
            from_me: false,
            timestamp: at,
            kind: StoryKind::Text {
                text: id.into(),
                background: 0xFF111B21,
                font: 0,
            },
            seen,
            path: None,
        }
    }

    #[test]
    fn own_status_uses_the_you_label() {
        let mut mine = story("me", "1@s.whatsapp.net", 10, true);
        mine.from_me = true;
        mine.sender_name = None;
        assert_eq!(
            contact_label(std::slice::from_ref(&mine), "1@s.whatsapp.net", "Você"),
            "Você"
        );
        let theirs = story("them", "2@s.whatsapp.net", 10, false);
        assert_eq!(
            contact_label(std::slice::from_ref(&theirs), "Ada", "Você"),
            "Ada"
        );
    }

    #[test]
    fn unseen_contacts_come_first_and_expired_status_is_gone() {
        let now = 1_000_000;
        let rows = vec![
            story("old", "a@s.whatsapp.net", now - TTL_SECS - 1, false),
            story("seen", "b@s.whatsapp.net", now - 10, true),
            story("fresh", "c@s.whatsapp.net", now - 50, false),
            story("later", "b@s.whatsapp.net", now - 5, true),
        ];
        let groups = groups(&rows, now);
        assert!(
            groups
                .iter()
                .all(|group| group.sender != "a@s.whatsapp.net")
        );
        assert_eq!(groups[0].sender, "c@s.whatsapp.net");
        assert_eq!(groups[0].unseen, 1);
        assert_eq!(groups[1].stories.len(), 2);
        assert_eq!(groups[1].stories[0].id, "seen");
    }

    #[test]
    fn address_book_keeps_saved_contacts_only() {
        assert!(address_book("1@s.whatsapp.net", Some("Ada"), false));
        assert!(!address_book("1@s.whatsapp.net", Some("Ada"), true));
        assert!(!address_book("1@s.whatsapp.net", None, false));
        assert!(!address_book("1@s.whatsapp.net", Some("  "), false));
        assert!(!address_book("3@g.us", Some("Group"), false));
    }

    #[test]
    fn audience_keeps_people_and_honours_allow_and_deny() {
        let people = vec![
            "1@s.whatsapp.net".into(),
            "2@lid".into(),
            "3@g.us".into(),
            "status@broadcast".into(),
            "4@newsletter".into(),
        ];
        assert_eq!(
            audience(&people, Privacy::Contacts, &[]),
            ["1@s.whatsapp.net", "2@lid"]
        );
        assert_eq!(
            audience(&people, Privacy::Allow, &["2@lid".into(), "3@g.us".into()]),
            ["2@lid"]
        );
        assert_eq!(
            audience(&people, Privacy::Deny, &["1@s.whatsapp.net".into()]),
            ["2@lid"]
        );
    }

    #[test]
    fn cache_trim_drops_the_oldest_past_either_cap() {
        let files = vec![
            CachedFile {
                path: PathBuf::from("a"),
                modified: 1,
                bytes: 40,
            },
            CachedFile {
                path: PathBuf::from("b"),
                modified: 3,
                bytes: 40,
            },
            CachedFile {
                path: PathBuf::from("c"),
                modified: 2,
                bytes: 40,
            },
        ];
        let dropped = trim_cache(&files, 2, 1_000);
        assert_eq!(dropped, [PathBuf::from("a")]);
        let dropped = trim_cache(&files, 10, 70);
        assert_eq!(dropped.len(), 2);
        assert!(!dropped.iter().any(|path| path == &PathBuf::from("b")));
    }

    #[test]
    fn seen_receipt_follows_the_setting_and_skips_own_status() {
        assert!(!send_seen_receipt(false, false));
        assert!(!send_seen_receipt(true, true));
        assert!(send_seen_receipt(true, false));
    }

    #[test]
    fn stepping_advances_and_closes_after_the_last() {
        let now = 100;
        let rows = vec![
            story("a1", "a@s.whatsapp.net", 10, false),
            story("a2", "a@s.whatsapp.net", 20, false),
            story("b1", "b@s.whatsapp.net", 5, true),
        ];
        let groups = groups(&rows, now);
        let first = StoryView {
            sender: groups[0].sender.clone(),
            index: 0,
        };
        let second = step(&groups, &first, 1).unwrap();
        assert_eq!(second.index, 1);
        let next_person = step(&groups, &second, 1).unwrap();
        assert_ne!(next_person.sender, second.sender);
        assert!(step(&groups, &next_person, 1).is_none());
    }

    #[test]
    fn light_background_uses_dark_ink() {
        assert!(dark_ink(0xFFF9F7F3));
        assert!(!dark_ink(0xFF111B21));
    }

    #[test]
    fn portuguese_and_spanish_name_the_status_screen() {
        assert_eq!(phrase("pt", "title"), "Estados");
        assert_eq!(phrase("es", "title"), "Estados");
        assert_eq!(phrase("en", "confirm"), "Publish this status now?");
        assert_eq!(phrase("pt", "confirm"), "Publicar este estado agora?");
    }
}
