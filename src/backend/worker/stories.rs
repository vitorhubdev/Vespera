//! Status updates: store what arrives, mark one as seen, download its file, post one.

use std::path::PathBuf;
use std::sync::Arc;

use whatsapp_rust::download::MediaType;
use whatsapp_rust::features::{StatusPrivacySetting, StatusSendOptions};
use whatsapp_rust::prelude::{Jid, MessageExt, wa};
use whatsapp_rust::upload::UploadOptions;
use whatsapp_rust::wacore::download::Downloadable;
use whatsapp_rust::waproto::buffa::Message as _;

use super::{Command, Event, Worker, fetch_to_temp, media_path, publish_download};
use crate::model::{ChatKind, Content, Delivery, Media, MediaState, MentionRef, Message};
use crate::stories::{
    CachedFile, Privacy, Story, StoryKind, audience, send_seen_receipt, trim_cache,
};

pub(super) fn ingest(
    worker: &mut Worker,
    message: &Arc<wa::Message>,
    info: &whatsapp_rust::types::message::MessageInfo,
) {
    let base = message.get_base_message();
    if let Some(protocol) = base.protocol_message.as_option() {
        use wa::message::protocol_message::Type;
        if protocol.r#type == Some(Type::REVOKE)
            && let Some(target) = protocol.key.as_option().and_then(|key| key.id.clone())
        {
            forget(worker, &target);
        }
        return;
    }
    let Some(kind) = kind_of(base) else {
        return;
    };
    let from_me = info.source.is_from_me;
    let sender = if from_me {
        worker.me()
    } else {
        worker.canonical(&info.source.sender)
    };
    let sender_name = (!info.push_name.is_empty() && !from_me).then(|| info.push_name.to_string());
    let story = Story {
        id: info.id.to_string(),
        sender,
        sender_name,
        from_me,
        timestamp: info.timestamp.timestamp(),
        kind,
        seen: from_me,
        path: None,
    };
    if let Err(error) = worker
        .archive
        .upsert_story(&story, Some(&message.encode_to_vec()))
    {
        log::debug!("status not stored: {error}");
        return;
    }
    worker.emit(Event::Story(story));
}

pub(super) fn load(worker: &mut Worker) {
    match worker.archive.stories(crate::util::now()) {
        Ok(stories) => worker.emit(Event::Stories(stories)),
        Err(error) => log::debug!("status list failed: {error}"),
    }
}

pub(super) fn mark_seen(worker: &mut Worker, id: String, sender: String, receipts: bool) {
    let Some(story) = worker.archive.story(&id).ok().flatten() else {
        return;
    };
    if let Err(error) = worker.archive.set_story_seen(&id) {
        log::debug!("status seen flag not stored: {error}");
    }
    if !send_seen_receipt(receipts, story.from_me) {
        return;
    }
    let (Some(client), Some(author)) = (worker.client.clone(), sender.parse::<Jid>().ok()) else {
        return;
    };
    let story_id = id;
    tokio::spawn(async move {
        let chat = Jid::status_broadcast();
        if let Err(error) = client
            .mark_as_read(&chat, Some(&author), &[story_id.as_str()])
            .await
        {
            log::debug!("status seen receipt not sent: {error}");
        }
    });
}

pub(super) fn download(worker: &mut Worker, id: String) {
    if !worker
        .inflight_downloads
        .insert(("status".into(), id.clone()))
    {
        return;
    }
    let Some(client) = worker.client.clone() else {
        worker
            .inflight_downloads
            .remove(&("status".into(), id.clone()));
        finish(worker, id, Err("Not connected to WhatsApp".into()));
        return;
    };
    let raw = worker.archive.story_raw(&id).ok().flatten();
    let Some(message) = raw.and_then(|raw| wa::Message::decode_from_slice(&raw).ok()) else {
        worker
            .inflight_downloads
            .remove(&("status".into(), id.clone()));
        finish(
            worker,
            id,
            Err("Attachment download keys are missing".into()),
        );
        return;
    };
    let base = message.get_base_message().clone();
    let (downloadable, mime): (Box<dyn Downloadable + Send>, String) =
        if let Some(image) = base.image_message.as_option() {
            (
                Box::new(image.clone()),
                image
                    .mimetype
                    .clone()
                    .unwrap_or_else(|| "image/jpeg".into()),
            )
        } else if let Some(video) = base.video_message.as_option() {
            (
                Box::new(video.clone()),
                video.mimetype.clone().unwrap_or_else(|| "video/mp4".into()),
            )
        } else {
            worker
                .inflight_downloads
                .remove(&("status".into(), id.clone()));
            finish(
                worker,
                id,
                Err("This status has no downloadable file".into()),
            );
            return;
        };
    let dir = worker.dirs.status_cache_dir();
    let _ = std::fs::create_dir_all(&dir);
    let final_path = media_path(&dir, "status", &id, &mime, None);
    let mut temp_os = final_path.clone().into_os_string();
    temp_os.push(".part");
    let temp_path = PathBuf::from(temp_os);
    let commands = worker.commands.clone();
    let limits = super::download_limits_for(downloadable.file_length());
    tokio::spawn(async move {
        let _temp = super::TempGuard::new(temp_path.clone());
        let deadline = tokio::time::Instant::now() + limits.timeout;
        let result = match fetch_to_temp(
            &client,
            &*downloadable,
            &dir,
            &temp_path,
            limits,
            deadline,
        )
        .await
        {
            Ok(()) => publish_download(&temp_path, &final_path)
                .await
                .map(|()| final_path),
            Err(error) => Err(error.to_string()),
        };
        let _ = commands.send(Command::StoryDownloaded { id, result });
    });
}

pub(super) fn downloaded(worker: &mut Worker, id: String, result: Result<PathBuf, String>) {
    worker
        .inflight_downloads
        .remove(&("status".into(), id.clone()));
    match result {
        Ok(path) => {
            if let Err(error) = worker.archive.set_story_path(&id, &path) {
                log::debug!("status file path not stored: {error}");
            }
            trim(worker);
            worker.emit(Event::StoryFile {
                id,
                result: Ok(path),
            });
        }
        Err(error) => finish(worker, id, Err(error)),
    }
}

pub(super) fn pick_photo(worker: &Worker) {
    let commands = worker.commands.clone();
    tokio::task::spawn_blocking(move || {
        let path = rfd::FileDialog::new()
            .set_title("Status photo")
            .add_filter("Images", &["jpg", "jpeg", "png", "webp"])
            .pick_file();
        let _ = commands.send(Command::StatusPhotoPicked { path });
    });
}

pub(super) fn post_text(
    worker: &mut Worker,
    text: String,
    background: u32,
    font: i32,
    privacy: Privacy,
    picked: Vec<String>,
) {
    let text = text.trim().to_owned();
    if text.is_empty() {
        worker.emit(Event::StatusPosted {
            error: Some("Write something or choose a photo.".into()),
        });
        return;
    }
    let Some(client) = worker.client.clone() else {
        worker.emit(Event::StatusPosted {
            error: Some("Not connected to WhatsApp".into()),
        });
        return;
    };
    let recipients = recipients(worker, privacy, &picked);
    if recipients.is_empty() {
        worker.emit(Event::StatusPosted {
            error: Some(crate::stories::phrase("en", "no_contacts").into()),
        });
        return;
    }
    let commands = worker.commands.clone();
    let me = worker.me();
    tokio::spawn(async move {
        let options = send_options(privacy);
        let font_value = font_from(font);
        match client
            .status()
            .send_text(&text, background, font_value, &recipients, options)
            .await
        {
            Ok(sent) => {
                let _ = commands.send(Command::StatusPostFinished {
                    id: sent.message_id,
                    sender: me,
                    kind: StoryKind::Text {
                        text,
                        background,
                        font,
                    },
                    raw: Some(sent.message.encode_to_vec()),
                    path: None,
                    error: None,
                });
            }
            Err(error) => {
                let _ = commands.send(Command::StatusPostFinished {
                    id: String::new(),
                    sender: me,
                    kind: StoryKind::Text {
                        text,
                        background,
                        font,
                    },
                    raw: None,
                    path: None,
                    error: Some(error.to_string()),
                });
            }
        }
    });
}

pub(super) fn post_image(
    worker: &mut Worker,
    path: PathBuf,
    caption: String,
    privacy: Privacy,
    picked: Vec<String>,
) {
    let Some(client) = worker.client.clone() else {
        worker.emit(Event::StatusPosted {
            error: Some("Not connected to WhatsApp".into()),
        });
        return;
    };
    let recipients = recipients(worker, privacy, &picked);
    if recipients.is_empty() {
        worker.emit(Event::StatusPosted {
            error: Some(crate::stories::phrase("en", "no_contacts").into()),
        });
        return;
    }
    let commands = worker.commands.clone();
    let me = worker.me();
    tokio::spawn(async move {
        let caption = caption.trim().to_owned();
        let caption_ref = (!caption.is_empty()).then_some(caption.as_str());
        let posted = async {
            let bytes = tokio::fs::read(&path)
                .await
                .map_err(|error| error.to_string())?;
            let decoded = tokio::task::spawn_blocking(move || {
                image::load_from_memory(&bytes).map_err(|error| error.to_string())
            })
            .await
            .map_err(|error| error.to_string())??;
            let jpeg = super::encode_jpeg(&decoded, 88)?;
            let thumbnail = super::thumbnail_jpeg(&decoded).unwrap_or_default();
            let upload = client
                .upload(jpeg, MediaType::Image, UploadOptions::default())
                .await
                .map_err(|error| error.to_string())?;
            client
                .status()
                .send_image(
                    upload,
                    thumbnail,
                    caption_ref,
                    &recipients,
                    send_options(privacy),
                )
                .await
                .map_err(|error| error.to_string())
        };
        match posted.await {
            Ok(sent) => {
                let _ = commands.send(Command::StatusPostFinished {
                    id: sent.message_id,
                    sender: me,
                    kind: StoryKind::Image {
                        caption: (!caption.is_empty()).then_some(caption),
                    },
                    raw: Some(sent.message.encode_to_vec()),
                    path: Some(path),
                    error: None,
                });
            }
            Err(error) => {
                let _ = commands.send(Command::StatusPostFinished {
                    id: String::new(),
                    sender: me,
                    kind: StoryKind::Image { caption: None },
                    raw: None,
                    path: None,
                    error: Some(error),
                });
            }
        }
    });
}

pub(super) fn posted(
    worker: &mut Worker,
    id: String,
    sender: String,
    kind: StoryKind,
    raw: Option<Vec<u8>>,
    path: Option<PathBuf>,
    error: Option<String>,
) {
    if let Some(error) = error {
        worker.emit(Event::StatusPosted { error: Some(error) });
        return;
    }
    let story = Story {
        id,
        sender,
        sender_name: None,
        from_me: true,
        timestamp: crate::util::now(),
        kind,
        seen: true,
        path,
    };
    if let Err(error) = worker.archive.upsert_story(&story, raw.as_deref()) {
        log::debug!("posted status not stored: {error}");
    }
    worker.emit(Event::Story(story));
    worker.emit(Event::StatusPosted { error: None });
}

pub(super) fn quote(worker: &Worker, id: &str, target: &Jid) -> Option<(wa::ContextInfo, Message)> {
    let raw = worker.archive.story_raw(id).ok().flatten()?;
    let quoted = wa::Message::decode_from_slice(&raw).ok()?;
    let story = worker.archive.story(id).ok().flatten()?;
    let sender = story.sender.parse::<Jid>().ok()?;
    let status = Jid::status_broadcast();
    let context = whatsapp_rust::wacore::proto_helpers::build_quote_context_with_info(
        story.id.clone(),
        &sender,
        &status,
        target,
        &quoted,
    );
    let row = Message {
        id: story.id.clone(),
        chat: story.sender.clone(),
        sender: story.sender.clone(),
        sender_name: story.sender_name.clone(),
        from_me: story.from_me,
        timestamp: story.timestamp,
        content: match &story.kind {
            StoryKind::Text { text, .. } => Content::text(text.clone()),
            StoryKind::Image { caption } => Content::Image {
                caption: caption.clone(),
                media: idle_media("image/jpeg", story.path.clone()),
            },
            StoryKind::Video { caption } => Content::Video {
                caption: caption.clone(),
                media: idle_media("video/mp4", story.path.clone()),
                seconds: None,
                gif: false,
            },
        },
        status: Delivery::None,
        delivered_at: None,
        read_at: None,
        quoted: None,
        reactions: Vec::new(),
        edited: false,
        mentions: Vec::<MentionRef>::new(),
        forwarded: false,
        thumbnail: None,
    };
    Some((context, row))
}

fn idle_media(mime: &str, path: Option<PathBuf>) -> Media {
    Media {
        mime: mime.to_owned(),
        size: 0,
        width: None,
        height: None,
        path,
        hash: None,
        state: MediaState::Idle,
    }
}

fn forget(worker: &mut Worker, id: &str) {
    match worker.archive.delete_story(id) {
        Ok(path) => {
            if let Some(path) = path {
                let _ = std::fs::remove_file(path);
            }
            worker.emit(Event::StoryGone(id.to_owned()));
        }
        Err(error) => log::debug!("status not removed: {error}"),
    }
}

fn finish(worker: &mut Worker, id: String, result: Result<PathBuf, String>) {
    worker.emit(Event::StoryFile { id, result });
}

fn trim(worker: &Worker) {
    let dir = worker.dirs.status_cache_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let modified = meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|time| time.as_secs() as i64)
            .unwrap_or(0);
        files.push(CachedFile {
            path,
            modified,
            bytes: meta.len(),
        });
    }
    for path in trim_cache(
        &files,
        crate::stories::CACHE_MAX_FILES,
        crate::stories::CACHE_MAX_BYTES,
    ) {
        let _ = std::fs::remove_file(path);
    }
}

fn people(worker: &Worker) -> Vec<String> {
    let mut ids = Vec::new();
    if let Ok(chats) = worker.archive.chats() {
        for chat in chats {
            if chat.kind == ChatKind::Direct {
                ids.push(chat.id);
            }
        }
    }
    if let Ok(contacts) = worker.archive.contacts() {
        for contact in contacts {
            ids.push(contact.id);
        }
    }
    ids
}

fn recipients(worker: &Worker, privacy: Privacy, picked: &[String]) -> Vec<Jid> {
    audience(&people(worker), privacy, picked)
        .into_iter()
        .filter_map(|id| id.parse().ok())
        .collect()
}

fn send_options(privacy: Privacy) -> StatusSendOptions {
    StatusSendOptions {
        privacy: match privacy {
            Privacy::Contacts => StatusPrivacySetting::Contacts,
            Privacy::Allow => StatusPrivacySetting::AllowList,
            Privacy::Deny => StatusPrivacySetting::DenyList,
        },
        ..Default::default()
    }
}

fn kind_of(base: &wa::Message) -> Option<StoryKind> {
    if let Some(text) = base.extended_text_message.as_option() {
        return Some(StoryKind::Text {
            text: text.text.clone().unwrap_or_default(),
            background: text.background_argb.unwrap_or(0xFF111B21),
            font: text.font.map(font_code).unwrap_or(0),
        });
    }
    if let Some(text) = base.conversation.clone() {
        return Some(StoryKind::Text {
            text,
            background: 0xFF111B21,
            font: 0,
        });
    }
    if base.image_message.is_set() {
        return Some(StoryKind::Image {
            caption: base
                .image_message
                .as_option()
                .and_then(|image| image.caption.clone()),
        });
    }
    if base.video_message.is_set() {
        return Some(StoryKind::Video {
            caption: base
                .video_message
                .as_option()
                .and_then(|video| video.caption.clone()),
        });
    }
    None
}

fn font_code(font: wa::message::extended_text_message::FontType) -> i32 {
    use wa::message::extended_text_message::FontType::*;
    match font {
        SYSTEM => 0,
        SYSTEM_TEXT => 1,
        FB_SCRIPT => 2,
        SYSTEM_BOLD => 6,
        MORNINGBREEZE_REGULAR => 7,
        CALISTOGA_REGULAR => 8,
        EXO2_EXTRABOLD => 9,
        COURIERPRIME_BOLD => 10,
    }
}

fn font_from(code: i32) -> wa::message::extended_text_message::FontType {
    use wa::message::extended_text_message::FontType::*;
    match code {
        1 => SYSTEM_TEXT,
        2 => FB_SCRIPT,
        6 => SYSTEM_BOLD,
        7 => MORNINGBREEZE_REGULAR,
        8 => CALISTOGA_REGULAR,
        9 => EXO2_EXTRABOLD,
        10 => COURIERPRIME_BOLD,
        _ => SYSTEM,
    }
}
