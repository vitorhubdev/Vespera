//! UI models for chats, messages, and view actions.
//!
//! The backend translates protocol types into these models, keeping protobufs
//! out of views and giving the archive a stable shape.

use std::path::PathBuf;
use std::time::Instant;

use serde::{Deserialize, Serialize};

/// Chat JID string: `<phone>@s.whatsapp.net`, `<id>@g.us`, or `<id>@lid`.
/// What a file attachment is, for labels and for safe handling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    Image,
    Pdf,
    Audio,
    Video,
    Archive,
    /// Something that can run code on this computer.
    Installer,
    Other,
}

/// A sender file name as Windows will treat it on disk: trailing dots and
/// spaces are stripped from the final component, so `evil.bat ` opens as
/// `evil.bat`. Every parser that decides what a name is (classification,
/// saved extension, saved stem) must read this form, or the three disagree
/// about what runs.
pub fn canonical_file_name(name: &str) -> &str {
    name.trim_end_matches([' ', '.'])
}

impl FileKind {
    /// Reads the kind from the MIME type, with the name as the tie breaker.
    pub fn of(mime: &str, name: &str) -> Self {
        let extension = canonical_file_name(name)
            .rsplit_once('.')
            .map(|(_, extension)| extension.to_ascii_lowercase())
            .unwrap_or_default();
        match extension.as_str() {
            "exe" | "msi" | "bat" | "cmd" | "com" | "scr" | "ps1" | "jar" | "apk" | "dmg"
            | "appimage" | "deb" | "rpm" | "pif" | "lnk" | "vbs" | "vbe" | "jse" | "wsf"
            | "wsh" => return Self::Installer,
            "zip" | "rar" | "7z" | "tar" | "gz" | "xz" | "bz2" => return Self::Archive,
            _ => {}
        }
        let mime = mime.split(';').next().unwrap_or_default().trim();
        match mime {
            "application/pdf" => Self::Pdf,
            "application/zip"
            | "application/gzip"
            | "application/x-tar"
            | "application/x-7z-compressed"
            | "application/x-rar-compressed" => Self::Archive,
            "application/vnd.android.package-archive"
            | "application/x-msdownload"
            | "application/x-msi"
            | "application/x-executable" => Self::Installer,
            _ if mime.starts_with("image/") => Self::Image,
            _ if mime.starts_with("audio/") => Self::Audio,
            _ if mime.starts_with("video/") => Self::Video,
            _ => Self::Other,
        }
    }

    /// A short word for the file, shown beside its size.
    pub fn label(self) -> &'static str {
        match self {
            Self::Image => "Image",
            Self::Pdf => "PDF",
            Self::Audio => "Audio",
            Self::Video => "Video",
            Self::Archive => "Archive",
            Self::Installer => "Program",
            Self::Other => "File",
        }
    }

    /// A picture sent as a file is still a picture.
    pub fn is_image(self) -> bool {
        self == Self::Image
    }

    /// Opening one of these can run code, so it is saved first instead.
    pub fn runs_code(self) -> bool {
        self == Self::Installer
    }
}

/// Everything the "Show info" dialog lists about one attachment.
#[derive(Clone, Debug, PartialEq)]
pub struct FileInfo {
    pub title: String,
    /// Label and value pairs, in the order they are shown.
    pub rows: Vec<(String, String)>,
    /// A closing line, used for the caution on programs.
    pub note: Option<String>,
}
/// Chat JID string: `<phone>@s.whatsapp.net`, `<id>@g.us`, or `<id>@lid`.
pub type ChatId = String;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatKind {
    Direct,
    Group,
    /// Read-only newsletter or broadcast list.
    Broadcast,
}

impl ChatKind {
    pub fn from_id(id: &str) -> Self {
        match id.rsplit('@').next() {
            Some("g.us") => Self::Group,
            Some("newsletter") | Some("broadcast") => Self::Broadcast,
            _ => Self::Direct,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Chat {
    pub id: ChatId,
    /// Best known address-book, push, or phone-number name.
    pub name: String,
    pub kind: ChatKind,
    /// Latest-message Unix timestamp used for ordering.
    pub last_activity: i64,
    pub unread: u32,
    pub archived: bool,
    pub pinned: bool,
    /// Pin time in Unix milliseconds; zero for older archives with no ordering.
    pub pinned_at: i64,
    /// Mute end as Unix seconds; `Some(0)` means indefinite.
    pub muted_until: Option<i64>,
    /// Latest message shown in the chat list.
    pub last: Option<LastMessage>,
    /// Canonical group-member ids, empty until loaded.
    pub participants: Vec<String>,
    /// Whether this is an announcement group where we cannot post.
    pub read_only: bool,
    /// Whether this group is the parent container of a WhatsApp Community.
    pub community: bool,
    /// Disappearing-message duration in seconds, if enabled.
    pub ephemeral_expiration: Option<u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LastMessage {
    pub from_me: bool,
    pub sender: String,
    /// Group-message sender.
    pub sender_name: Option<String>,
    pub summary: String,
    /// The whole message behind `summary`, every line of it: the chat row
    /// shows it in a tooltip when the one-line preview cannot.
    pub full: String,
    pub status: Delivery,
}

impl Chat {
    pub fn new(id: ChatId, name: String) -> Self {
        let kind = ChatKind::from_id(&id);
        Self {
            id,
            name,
            kind,
            last_activity: 0,
            unread: 0,
            archived: false,
            pinned: false,
            pinned_at: 0,
            muted_until: None,
            last: None,
            participants: Vec::new(),
            read_only: false,
            community: false,
            ephemeral_expiration: None,
        }
    }

    pub fn is_group(&self) -> bool {
        self.kind == ChatKind::Group
    }

    pub fn muted(&self, now: i64) -> bool {
        matches!(self.muted_until, Some(0)) || self.muted_until.is_some_and(|until| until > now)
    }

    /// Whether this row belongs in the separate channels/communities view.
    pub fn is_channel_or_community(&self) -> bool {
        self.id.ends_with("@newsletter") || self.community
    }

    /// Whether this chat is a channel (newsletter).
    pub fn is_channel(&self) -> bool {
        self.id.ends_with("@newsletter")
    }

    /// Direct-chat phone number as digits.
    pub fn phone(&self) -> Option<&str> {
        phone_of(&self.id)
    }
}

/// Extracts digits from a `<phone>@s.whatsapp.net` id.
/// Whether the app may start sending to this chat.
///
/// Announcement groups stay muted through read_only. Channels deny by
/// default: the encrypted send path rejects newsletters, and publishing
/// there needs a proven role-gated path the app does not have yet.
/// Groups and community containers keep the read_only rule only.
///
/// This is the single decision point: the composer and every send action
/// consult it, and the worker re-checks it for incoming send commands.
pub fn can_send(chat: &Chat) -> bool {
    !chat.read_only && !chat.is_channel()
}

/// Whether this chat is Meta AI.
///
/// Same address rule as `JidExt::is_bot` in whatsapp-rust: a phone user
/// starting with `1313555` or `131655500`, or the `@bot` server. A device
/// suffix (`:12`) is ignored. Linked-device history stays visible; sending
/// is a separate decision.
pub fn is_meta_ai(id: &str) -> bool {
    let Some((user, server)) = id.split_once('@') else {
        return false;
    };
    let user = user.split(':').next().unwrap_or(user);
    server == "bot"
        || (server == "s.whatsapp.net"
            && (user.starts_with("1313555") || user.starts_with("131655500")))
}

pub fn phone_of(id: &str) -> Option<&str> {
    let (user, server) = id.split_once('@')?;
    (server == "s.whatsapp.net" && user.chars().all(|c| c.is_ascii_digit())).then_some(user)
}

/// Outgoing-message delivery state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Delivery {
    /// Incoming message without outgoing receipts.
    #[default]
    None,
    /// Sent to the backend but not acknowledged by the server.
    Pending,
    Sent,
    Delivered,
    Read,
    Played,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    /// WhatsApp message id, unique within a chat.
    pub id: String,
    pub chat: ChatId,
    /// Sender JID, including our own for outgoing messages.
    pub sender: String,
    /// Group sender's push name at receipt time.
    pub sender_name: Option<String>,
    pub from_me: bool,
    /// Unix seconds.
    pub timestamp: i64,
    pub content: Content,
    pub status: Delivery,
    /// First delivered-receipt Unix timestamp for outgoing messages.
    #[serde(default)]
    pub delivered_at: Option<i64>,
    /// First read or played receipt Unix timestamp.
    #[serde(default)]
    pub read_at: Option<i64>,
    pub quoted: Option<Quoted>,
    pub reactions: Vec<Reaction>,
    pub edited: bool,
    /// Mentions in the text or caption.
    #[serde(default)]
    pub mentions: Vec<MentionRef>,
    /// Forwarded from another chat.
    #[serde(default)]
    pub forwarded: bool,
    /// JPEG preview sent with an attachment or link.
    #[serde(default)]
    pub thumbnail: Option<Vec<u8>>,
}

/// Raw WhatsApp mention token and its canonical id.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MentionRef {
    pub user: String,
    pub id: String,
}

/// Link metadata attached by WhatsApp.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LinkPreview {
    pub url: String,
    pub title: Option<String>,
    pub description: Option<String>,
    /// WhatsApp sent this preview as a video. Opening it stays in the viewer.
    #[serde(default)]
    pub video: bool,
    /// Direct clip address when the message carried one.
    /// A web page address is never stored here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_url: Option<String>,
}

impl LinkPreview {
    /// Whether a click plays this preview in the viewer instead of a browser.
    pub fn opens_in_viewer(&self) -> bool {
        self.video
    }
}

impl Message {
    /// A click on this address stays in the viewer when it belongs to a video preview.
    pub fn link_stays_in_viewer(&self, url: &str) -> bool {
        let Content::Text {
            preview: Some(preview),
            ..
        } = &self.content
        else {
            return false;
        };
        preview.video
            && (same_link(&preview.url, url)
                || preview
                    .video_url
                    .as_deref()
                    .is_some_and(|video| same_link(video, url)))
    }
}

fn same_link(left: &str, right: &str) -> bool {
    left.trim().trim_end_matches('/') == right.trim().trim_end_matches('/')
}

impl Message {
    /// One-line summary used in chat rows and quotes.
    pub fn summary(&self) -> String {
        self.content.summary()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Quoted {
    pub id: String,
    pub sender: String,
    pub sender_name: Option<String>,
    pub summary: String,
    /// Mentions in quoted text.
    #[serde(default)]
    pub mentions: Vec<MentionRef>,
}

/// The hashes one media descriptor can prove, kept per domain.
///
/// Recovered from a message's persisted protobuf rather than from its media
/// JSON, so a row written before `Media.hash` existed still yields a real
/// identity instead of `None`. A proof that came from a protobuf always knows
/// which of `file_sha256` or `file_enc_sha256` it read, so it names its
/// domain; a proof read out of a stored `Media.hash` cannot and says so.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MediaIdentityProof {
    /// Which field the digest below was read from.
    pub domain: HashDomain,
    /// The hex digest, in the domain named above.
    pub content: Option<String>,
    /// The other domain's digest, when the descriptor carried both. Kept so a
    /// stored side can still be compared against a typed incoming side.
    pub encrypted: Option<String>,
}

impl MediaIdentityProof {
    /// Builds a typed proof from a protobuf field.
    pub fn new(domain: HashDomain, bytes: Option<&[u8]>) -> Self {
        let Some(bytes) = bytes.filter(|bytes| !bytes.is_empty()) else {
            return Self::default();
        };
        let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        match domain {
            HashDomain::Content => Self {
                domain: HashDomain::Content,
                content: Some(hex),
                encrypted: None,
            },
            HashDomain::Encrypted => Self {
                domain: HashDomain::Encrypted,
                content: Some(hex),
                encrypted: None,
            },
            // `from_stored` is the only way to reach an untagged proof.
            HashDomain::Unknown => Self::from_stored(Some(&hex)),
        }
    }

    /// Records the second domain's digest on an already typed proof, so a
    /// descriptor carrying both hashes keeps both.
    pub fn with(mut self, domain: HashDomain, bytes: Option<&[u8]>) -> Self {
        let Some(bytes) = bytes.filter(|bytes| !bytes.is_empty()) else {
            return self;
        };
        let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        match domain {
            HashDomain::Content => self.content = Some(hex),
            HashDomain::Encrypted => self.encrypted = Some(hex),
            HashDomain::Unknown => {}
        }
        self
    }

    /// Reads a stored `Media.hash`, whose domain was never recorded.
    ///
    /// `classify` writes `file_sha256` and falls back to `file_enc_sha256`,
    /// so the field does not say which it holds. The result is therefore
    /// untagged ([`HashDomain::Unknown`]) and may be compared only against
    /// another untagged side, never against a typed one. Guessing would let a
    /// plaintext hash answer for an encrypted one.
    pub fn from_stored(hex: Option<&str>) -> Self {
        let Some(hex) = hex.filter(|hex| !hex.is_empty()) else {
            return Self::default();
        };
        Self {
            domain: HashDomain::Unknown,
            content: Some(hex.to_owned()),
            encrypted: None,
        }
    }

    /// Verdict against another side that also names its domain.
    ///
    /// `Same` requires an exact match inside one and the same domain, so the
    /// same 32 bytes labelled `Content` and `Encrypted` are `Unknown`, not
    /// `Same`. With no comparable pair the answer is `Unknown`, because a
    /// missing counterpart is missing proof, not proof of a difference.
    pub fn verdict(&self, incoming: &Self) -> MediaIdentity {
        // Domains must line up. An untagged side matches only an untagged
        // one; a typed side never answers for the other domain.
        let comparable = match (self.domain, incoming.domain) {
            (HashDomain::Content, HashDomain::Content) => {
                self.content.as_deref().zip(incoming.content.as_deref())
            }
            (HashDomain::Encrypted, HashDomain::Encrypted) => {
                self.encrypted.as_deref().or(self.content.as_deref()).zip(
                    incoming
                        .encrypted
                        .as_deref()
                        .or(incoming.content.as_deref()),
                )
            }
            // An untagged stored hash can only be compared with another
            // untagged hash, in the slot it was written to.
            (HashDomain::Unknown, HashDomain::Unknown) => {
                self.content.as_deref().zip(incoming.content.as_deref())
            }
            _ => None,
        };
        match comparable {
            Some((left, right)) if left == right => MediaIdentity::Same,
            Some((_left, _right)) => MediaIdentity::Different,
            None => MediaIdentity::Unknown,
        }
    }

    /// Whether this proof carries any digest at all.
    pub fn is_empty(&self) -> bool {
        self.content.is_none() && self.encrypted.is_none()
    }

    /// Digest of the primary domain, the one `classify` would write into
    /// `Media.hash`.
    ///
    /// `Content` reads `content`. `Encrypted` reads `encrypted` only, so a
    /// plaintext hash left in `content` cannot answer for the encrypted
    /// domain. `Unknown` reads the untagged slot. Callers that compare this
    /// with a stored `Media.hash` compare two untagged strings. [`Self::verdict`]
    /// still refuses a typed proof against an untagged one.
    pub fn primary_hex(&self) -> Option<&str> {
        match self.domain {
            HashDomain::Content | HashDomain::Unknown => self.content.as_deref(),
            HashDomain::Encrypted => self.encrypted.as_deref(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Reaction {
    pub sender: String,
    pub from_me: bool,
    pub emoji: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Content {
    Text {
        text: String,
        #[serde(default)]
        preview: Option<LinkPreview>,
    },
    Image {
        caption: Option<String>,
        media: Media,
    },
    Video {
        caption: Option<String>,
        media: Media,
        seconds: Option<u32>,
        gif: bool,
    },
    Audio {
        media: Media,
        seconds: Option<u32>,
        voice_note: bool,
        /// Sender-provided 64-bar voice waveform.
        #[serde(default)]
        waveform: Vec<u8>,
    },
    Document {
        media: Media,
        file_name: String,
        caption: Option<String>,
        pages: Option<u32>,
    },
    Sticker {
        media: Media,
        animated: bool,
    },
    /// A sticker pack shared in a chat, opened on demand.
    StickerPack {
        name: String,
        publisher: String,
        count: u32,
        caption: Option<String>,
    },
    /// A photo, video, or voice message marked as view once by its sender.
    /// `can_open` is set only when a file arrived. Opening it once clears the
    /// flag and deletes the file. The file is never stored as ordinary media.
    ViewOnce {
        what: String,
        #[serde(default)]
        can_open: bool,
    },
    Location {
        latitude: f64,
        longitude: f64,
        name: Option<String>,
        address: Option<String>,
    },
    Contact {
        display_name: String,
        vcard: String,
    },
    Poll {
        question: String,
        options: Vec<String>,
        #[serde(default)]
        state: PollState,
    },
    /// "This message was deleted."
    Revoked,
    /// Header, body, footer, and labels from an interactive, button, list, or template message.
    /// Labels are text: choosing one does not send a reply.
    Interactive {
        header: Option<InteractiveHeader>,
        body: Option<String>,
        footer: Option<String>,
        options: Vec<String>,
        /// A part this client cannot draw, shown only for that part.
        note: Option<String>,
    },
    /// Invite to a group, with the code used by the join button.
    GroupInvite {
        name: String,
        code: String,
        #[serde(default)]
        caption: Option<String>,
    },
    /// Event card: title, time, and place. Times are Unix seconds.
    Event {
        title: String,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        start: i64,
        #[serde(default)]
        end: Option<i64>,
        #[serde(default)]
        location: Option<String>,
        #[serde(default)]
        cancelled: bool,
    },
    /// A call record from history or a live signaling event.
    /// `outcome` is `missed`, `answered`, `rejected`, `failed`, or `ongoing`.
    CallLog {
        video: bool,
        outcome: String,
        #[serde(default)]
        seconds: Option<u64>,
        #[serde(default)]
        scheduled: bool,
    },
    /// Unsupported content. `what` is the protobuf field that arrived.
    /// `reason` is `official_app`, `phone`, or `unknown`.
    Unsupported {
        what: String,
        #[serde(default = "unknown_reason")]
        reason: String,
    },
}

fn unknown_reason() -> String {
    "unknown".to_owned()
}

/// The header of an interactive message. Media uses the same download path as a photo or video.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum InteractiveHeader {
    Title {
        text: String,
    },
    Image {
        media: Media,
    },
    Video {
        media: Media,
        seconds: Option<u32>,
        gif: bool,
    },
    Document {
        media: Media,
        file_name: String,
        pages: Option<u32>,
    },
}

/// Poll information safe to send to the interface; encryption keys stay in the worker.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PollState {
    pub selectable: usize,
    pub counts: Vec<usize>,
    pub selected: Vec<usize>,
    pub voters: usize,
    pub can_vote: bool,
    pub history_complete: bool,
    pub refresh_needed: bool,
    pub refreshing: bool,
    pub refresh_failed: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PollDraft {
    pub question: String,
    pub options: Vec<String>,
    pub multiple: bool,
}

impl Default for PollDraft {
    fn default() -> Self {
        Self {
            question: String::new(),
            options: vec![String::new(); 2],
            multiple: true,
        }
    }
}

impl PollDraft {
    pub fn validated(&self) -> Result<Self, &'static str> {
        let question = self.question.trim().to_owned();
        let options: Vec<String> = self
            .options
            .iter()
            .map(|option| option.trim().to_owned())
            .collect();
        if question.is_empty() || question.chars().count() > 255 {
            return Err("Enter a question of up to 255 characters.");
        }
        if !(2..=12).contains(&options.len())
            || options
                .iter()
                .any(|option| option.is_empty() || option.chars().count() > 100)
        {
            return Err("Add 2–12 answers, each with 1–100 characters.");
        }
        let mut unique = std::collections::HashSet::new();
        if options.iter().any(|option| !unique.insert(option)) {
            return Err("Each answer must be different.");
        }
        Ok(Self {
            question,
            options,
            multiple: self.multiple,
        })
    }

    pub fn selectable(&self) -> usize {
        if self.multiple { self.options.len() } else { 1 }
    }
}

impl Content {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text {
            text: text.into(),
            preview: None,
        }
    }

    pub fn summary(&self) -> String {
        match self {
            Self::Text { text, .. } => text.lines().next().unwrap_or_default().to_owned(),
            Self::Image { caption, .. } => with_caption("Photo", caption),
            Self::Video { caption, gif, .. } => with_caption(video_label(*gif), caption),
            Self::Audio {
                voice_note,
                seconds,
                ..
            } => {
                let label = if *voice_note {
                    "Voice message"
                } else {
                    "Audio"
                };
                match seconds {
                    Some(seconds) => format!("{label} ({})", crate::util::duration(*seconds)),
                    None => label.to_owned(),
                }
            }
            Self::Document { file_name, .. } => format!("Document: {file_name}"),
            Self::Sticker { .. } => "Sticker".to_owned(),
            Self::ViewOnce { what, .. } => format!("View once {what}"),
            Self::Location { name, .. } => match name {
                Some(name) => format!("Location: {name}"),
                None => "Location".to_owned(),
            },
            Self::Contact { display_name, .. } => format!("Contact: {display_name}"),
            Self::Poll { question, .. } => format!("Poll: {question}"),
            Self::Revoked => "This message was deleted".to_owned(),
            Self::StickerPack { name, .. } => format!("Sticker pack: {name}"),
            Self::Interactive {
                body,
                header,
                footer,
                options,
                ..
            } => interactive_preview(body, header, footer, options),
            Self::GroupInvite { name, .. } => format!("Group invite: {name}"),
            Self::Event { title, .. } => format!("Event: {title}"),
            Self::CallLog { video, outcome, .. } => {
                crate::explain::call_title("en", *video, outcome)
            }
            Self::Unsupported { what, reason } => crate::explain::notice("en", what, reason).title,
        }
    }

    /// The whole message as [`Self::summary`] would label it: every line of
    /// a text or a photo or video caption, plus a group-invite caption and an
    /// event's time, place, and description. Other content has nothing more
    /// to say than its summary.
    pub fn full_summary(&self) -> String {
        let captioned = |label: &str, caption: &Option<String>| match caption.as_deref() {
            Some(caption) if !caption.trim().is_empty() => format!("{label}: {caption}"),
            _ => label.to_owned(),
        };
        match self {
            Self::Text { text, .. } => text.clone(),
            Self::Image { caption, .. } => captioned("Photo", caption),
            Self::Video { caption, gif, .. } => captioned(video_label(*gif), caption),
            Self::Interactive {
                header,
                body,
                footer,
                options,
                ..
            } => {
                let mut lines = Vec::new();
                if let Some(InteractiveHeader::Title { text }) = header
                    && !text.trim().is_empty()
                {
                    lines.push(text.clone());
                }
                if let Some(body) = body.as_deref().filter(|body| !body.trim().is_empty()) {
                    lines.push(body.to_owned());
                }
                if let Some(footer) = footer.as_deref().filter(|footer| !footer.trim().is_empty()) {
                    lines.push(footer.to_owned());
                }
                lines.extend(
                    options
                        .iter()
                        .filter(|option| !option.trim().is_empty())
                        .cloned(),
                );
                if lines.is_empty() {
                    self.summary()
                } else {
                    lines.join("\n")
                }
            }
            Self::GroupInvite { name, caption, .. } => {
                let mut lines = vec![format!("Group invite: {name}")];
                if let Some(caption) = caption
                    .as_deref()
                    .filter(|caption| !caption.trim().is_empty())
                {
                    lines.push(caption.to_owned());
                }
                lines.join("\n")
            }
            Self::Event {
                title,
                description,
                start,
                location,
                cancelled,
                ..
            } => {
                let mut title_line = format!("Event: {title}");
                if *cancelled {
                    title_line.push_str(" (cancelled)");
                }
                let mut lines = vec![title_line];
                if *start > 0 {
                    let stamp = crate::util::copy_stamp(*start);
                    if !stamp.is_empty() {
                        lines.push(stamp);
                    }
                }
                if let Some(location) = location
                    .as_deref()
                    .filter(|location| !location.trim().is_empty())
                {
                    lines.push(location.to_owned());
                }
                if let Some(description) = description
                    .as_deref()
                    .filter(|description| !description.trim().is_empty())
                {
                    lines.push(description.to_owned());
                }
                lines.join("\n")
            }
            _ => self.summary(),
        }
    }

    pub fn media(&self) -> Option<&Media> {
        match self {
            Self::Image { media, .. }
            | Self::Video { media, .. }
            | Self::Audio { media, .. }
            | Self::Document { media, .. }
            | Self::Sticker { media, .. } => Some(media),
            Self::Interactive { header, .. } => header_media(header),
            _ => None,
        }
    }

    pub fn media_mut(&mut self) -> Option<&mut Media> {
        match self {
            Self::Image { media, .. }
            | Self::Video { media, .. }
            | Self::Audio { media, .. }
            | Self::Document { media, .. }
            | Self::Sticker { media, .. } => Some(media),
            Self::Interactive { header, .. } => header_media_mut(header),
            _ => None,
        }
    }

    /// Whether WhatsApp lets this message be forwarded. Mirrors the worker's
    /// send-time rule so the interface never offers a dead end.
    pub fn forwardable(&self) -> bool {
        !matches!(
            self,
            Self::Revoked
                | Self::Unsupported { .. }
                | Self::Poll { .. }
                | Self::GroupInvite { .. }
                | Self::Event { .. }
                | Self::CallLog { .. }
        )
    }
}

/// What a video is called in previews.
fn video_label(gif: bool) -> &'static str {
    if gif { "GIF" } else { "Video" }
}

fn header_media(header: &Option<InteractiveHeader>) -> Option<&Media> {
    match header {
        Some(
            InteractiveHeader::Image { media }
            | InteractiveHeader::Video { media, .. }
            | InteractiveHeader::Document { media, .. },
        ) => Some(media),
        _ => None,
    }
}

fn header_media_mut(header: &mut Option<InteractiveHeader>) -> Option<&mut Media> {
    match header {
        Some(
            InteractiveHeader::Image { media }
            | InteractiveHeader::Video { media, .. }
            | InteractiveHeader::Document { media, .. },
        ) => Some(media),
        _ => None,
    }
}

/// Chat-list line for an interactive message: the body, then a shorter fallback.
fn interactive_preview(
    body: &Option<String>,
    header: &Option<InteractiveHeader>,
    footer: &Option<String>,
    options: &[String],
) -> String {
    let line = |text: &str| {
        text.lines()
            .find(|line| !line.trim().is_empty())
            .map(str::trim)
            .map(str::to_owned)
    };
    if let Some(body) = body.as_deref().and_then(line) {
        return body;
    }
    if let Some(InteractiveHeader::Title { text }) = header
        && let Some(title) = line(text)
    {
        return title;
    }
    if let Some(footer) = footer.as_deref().and_then(line) {
        return footer;
    }
    if let Some(option) = options.iter().find_map(|option| line(option)) {
        return option;
    }
    match header {
        Some(InteractiveHeader::Image { .. }) => "Photo".to_owned(),
        Some(InteractiveHeader::Video { gif, .. }) => video_label(*gif).to_owned(),
        Some(InteractiveHeader::Document { file_name, .. }) => format!("Document: {file_name}"),
        _ => "Interactive message".to_owned(),
    }
}

fn with_caption(label: &str, caption: &Option<String>) -> String {
    match caption
        .as_deref()
        .and_then(|caption| caption.lines().next())
    {
        Some(caption) if !caption.is_empty() => format!("{label}: {caption}"),
        _ => label.to_owned(),
    }
}

/// Attachment metadata, download state, and optional local file. Download keys
/// remain in the archive's raw message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Media {
    pub mime: String,
    pub size: u64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Decrypted downloaded file.
    #[serde(default)]
    pub path: Option<PathBuf>,
    /// Content identity (hex `file_sha256`, else hex `file_enc_sha256`).
    /// Decides replay inheritance; never a download URL or direct path.
    #[serde(default)]
    pub hash: Option<String>,
    /// Non-persisted download state.
    #[serde(skip)]
    pub state: MediaState,
}

/// Content-identity verdict between two media descriptors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaIdentity {
    /// Both sides carry a conclusive hash and it matches.
    Same,
    /// Both sides carry a conclusive hash and it differs.
    Different,
    /// At least one side has no conclusive hash.
    Unknown,
}

/// Compares content identity only: never MIME, size, dimensions, URLs, or
/// direct paths. A different file with equal visual metadata must not
/// inherit; an unknown side follows the conservative legacy path.
pub fn media_identity(stored: &Media, incoming: &Media) -> MediaIdentity {
    match (&stored.hash, &incoming.hash) {
        (Some(known), Some(seen)) if known == seen => MediaIdentity::Same,
        (Some(_), Some(_)) => MediaIdentity::Different,
        _ => MediaIdentity::Unknown,
    }
}

/// Which hash domain a digest belongs to.
///
/// A plaintext content hash and a hash of the encrypted content are computed
/// over different bytes, so the two are never interchangeable: the domain
/// travels with the digest and a comparison never crosses it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HashDomain {
    /// `file_sha256`: the decrypted content.
    Content,
    /// `file_enc_sha256`: the encrypted bytes as stored on the CDN.
    Encrypted,
    /// A hash read from a stored `Media.hash`, which never recorded which of
    /// the two fields it came from. It compares only with another untagged
    /// hash, never with a typed one. This is the default, because a hash with
    /// no recorded domain is what an absent domain means.
    #[default]
    Unknown,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum MediaState {
    #[default]
    Idle,
    Downloading,
    Failed(String),
}

/// Contact names from app-state sync and message push names.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Contact {
    pub id: String,
    pub full_name: Option<String>,
    pub push_name: Option<String>,
}

impl Contact {
    pub fn display_name(&self) -> Option<&str> {
        self.full_name
            .as_deref()
            .filter(|name| !name.is_empty())
            .or(self.push_name.as_deref().filter(|name| !name.is_empty()))
    }

    /// WhatsApp display name: address-book name or `~`-prefixed push name.
    pub fn label(&self) -> Option<String> {
        if let Some(name) = self.full_name.as_deref().filter(|name| !name.is_empty()) {
            return Some(name.to_owned());
        }
        self.push_name
            .as_deref()
            .filter(|name| !name.is_empty())
            .map(|name| format!("~{name}"))
    }
}

/// A message pinned in a chat until `until` (Unix seconds).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatPin {
    pub id: String,
    pub until: i64,
    pub preview: String,
}

/// One starred message on the Favorites screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FavoriteHit {
    pub chat: ChatId,
    pub id: String,
    pub timestamp: i64,
    pub preview: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Page {
    Chats,
    /// Call history. This device does not place calls.
    Calls,
    /// Status updates from the last 24 hours.
    Status,
    /// Starred messages, all chats or the open one.
    Favorites,
    Settings,
}

/// Destination chats allowed per forward action, matching WhatsApp: five
/// chats at once, or a single chat for frequently forwarded messages.
pub const FORWARD_CHAT_LIMIT: usize = 5;
/// Forwarding score at which WhatsApp treats a message as frequently
/// forwarded (the protocol library jumps to its sentinel at five forwards).
pub const FREQUENT_FORWARD_SCORE: u32 = 5;

/// The tabs of the picker above the composer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PickerTab {
    #[default]
    Emoji,
    Stickers,
    Received,
    Favorites,
}

/// Imported sticker pack stored as a named WebP directory.
#[derive(Clone, Debug, PartialEq)]
pub struct StickerPack {
    pub name: String,
    pub dir: PathBuf,
    pub stickers: Vec<PathBuf>,
}

/// Full-window viewer over the pictures and stickers of one chat.
#[derive(Clone, Debug, PartialEq)]
pub struct Viewer {
    pub chat: ChatId,
    /// Every viewable file in the chat, oldest first.
    pub items: Vec<ViewerItem>,
    /// Index of the item on screen.
    pub index: usize,
    /// Zoom factor; 1.0 fits the window.
    pub zoom: f32,
    /// Pan in points from the fitted position.
    pub offset: (f32, f32),
    /// Page on screen when the current item is a PDF.
    pub pdf_page: usize,
    /// Pages the current PDF holds, once the first one has been rendered.
    pub pdf_pages: usize,
    /// Quarter turns clockwise of the PDF page on screen.
    pub pdf_rotate: u8,
}

impl Viewer {
    /// Smallest and largest zoom the viewer allows.
    pub const MIN_ZOOM: f32 = 0.2;
    pub const MAX_ZOOM: f32 = 8.0;

    /// The item on screen.
    pub fn current(&self) -> Option<&ViewerItem> {
        self.items.get(self.index)
    }

    /// Moves by the given number of items, stopping at either end, and
    /// returns the view to fit.
    pub fn step(&mut self, step: i32) {
        let last = self.items.len().saturating_sub(1) as i64;
        let next = (self.index as i64 + i64::from(step)).clamp(0, last);
        self.index = next as usize;
        self.zoom = 1.0;
        self.offset = (0.0, 0.0);
        self.pdf_page = 0;
        self.pdf_pages = 0;
        self.pdf_rotate = 0;
    }

    /// Moves to another page of the PDF on screen, stopping at either end.
    pub fn page_by(&mut self, step: i32) {
        // The count arrives with the first render; stepping before that is a
        // no-op instead of clamping against an empty range.
        if self.pdf_pages == 0 {
            return;
        }
        let last = self.pdf_pages.saturating_sub(1) as i64;
        let next = (self.pdf_page as i64 + i64::from(step)).clamp(0, last);
        self.pdf_page = next as usize;
    }

    /// Jumps to a page of the PDF on screen, counted from one, stopping at
    /// either end of the document.
    pub fn page_to(&mut self, page: usize) {
        if self.pdf_pages == 0 {
            return;
        }
        let last = self.pdf_pages.saturating_sub(1);
        self.pdf_page = page.saturating_sub(1).min(last);
    }

    /// Multiplies the zoom around an anchor in points from the centre.
    pub fn zoom_by(&mut self, factor: f32, anchor: (f32, f32)) {
        let zoom = (self.zoom * factor).clamp(Self::MIN_ZOOM, Self::MAX_ZOOM);
        let ratio = zoom / self.zoom;
        let (x, y) = (anchor.0, anchor.1);
        self.offset = (
            x - (x - self.offset.0) * ratio,
            y - (y - self.offset.1) * ratio,
        );
        self.zoom = zoom;
    }
}

/// Scrub drag in progress over the open video: preview state only, the
/// definitive jump still goes through VideoSeek on release.
#[derive(Clone, Debug, PartialEq)]
pub struct VideoScrub {
    /// File being scrubbed: answers for anything else never paint.
    pub path: PathBuf,
    /// Drag generation from the preview decoder.
    pub generation: u64,
    /// Whether the clip played before the drag held it.
    pub was_playing: bool,
    /// Latest drag destination, 0 to 1: the only jump a release makes.
    pub target: f32,
}

/// One file the viewer can show.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewerItem {
    pub message: String,
    pub path: PathBuf,
    pub kind: ViewerKind,
}

/// What a viewer item holds and how it is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewerKind {
    Picture,
    /// A PDF, rendered one page at a time.
    Pdf,
    /// A video, decoded and played in-app with its soundtrack.
    Video,
}

/// What this account can do in one group, from the latest metadata.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GroupProfile {
    pub admin: bool,
    pub description: String,
    pub locked: bool,
    pub approval: bool,
    pub admins: Vec<String>,
}

/// A group change that waits for confirmation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GroupConfirm {
    Leave(ChatId),
    Remove { chat: ChatId, person: String },
    Demote { chat: ChatId, person: String },
    Revoke(ChatId),
    Deny { chat: ChatId, person: String },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Dialog {
    Shortcuts,
    About,
    ConfirmUnlink,
    /// Phone number used for pairing-code linking.
    PairWithPhone,
    /// Manually entered number for messaging or saving a contact.
    NewContact,
    ChatInfo(ChatId),
    /// Chooses destinations for forwarded messages.
    Forward {
        chat: ChatId,
        messages: Vec<String>,
    },
    /// Confirms joining a group from an invite card.
    ConfirmJoin {
        name: String,
        code: String,
    },
    /// Confirms declining a ringing call from this linked device.
    ConfirmRejectCall,
    /// How long a message stays pinned in the chat.
    PinMessage {
        chat: ChatId,
        message: String,
    },
    /// The account session ended. History stays until the user links again.
    Disconnected {
        kind: crate::unlink::EndKind,
        at: i64,
    },
    /// The new pairing belongs to a different account.
    ConfirmOtherAccount,
    /// Confirms deleting several messages, splitting the revocable ones out.
    ConfirmDeleteMany {
        chat: ChatId,
        ids: Vec<String>,
        revocable: usize,
    },
    /// Confirms sending a sticker, showing it first.
    ConfirmSticker {
        path: PathBuf,
    },
    /// Shows a sticker on its own, bigger, with a save button.
    PeekSticker {
        path: PathBuf,
    },
    /// Previews a sticker pack shared in a chat, with a button to keep it.
    StickerPackView {
        name: String,
        publisher: String,
        dir: PathBuf,
        stickers: Vec<PathBuf>,
    },
    /// Lists everything known about one attachment.
    FileInfo(Box<FileInfo>),
    CreatePoll(ChatId),
    /// Name and numbers for a group this account creates.
    NewGroup,
    /// Confirms one destructive group action.
    ConfirmGroup {
        title: String,
        body: String,
        action: GroupConfirm,
    },
    /// Optional dates for exporting one chat.
    ExportChat(ChatId),
}

/// Which group edit is in flight for sending-state and result mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupEditOp {
    Rename,
    Photo,
    RemovePhoto,
}

/// Rename/photo edit state for one group chat, with generation so stale or
/// other-chat answers never apply to the current edit.
#[derive(Clone, Debug, PartialEq)]
pub struct GroupEdit {
    pub generation: u64,
    /// Rename text buffer.
    pub name: String,
    /// Last photo path for retry (choose again if missing).
    pub photo: Option<PathBuf>,
    /// In-flight operation, if any.
    pub sending: Option<GroupEditOp>,
    /// Last failure, preserved with the old name/photo for retry.
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Error,
}

#[derive(Clone, Debug)]
pub struct Toast {
    pub message: String,
    pub kind: ToastKind,
    pub created: Instant,
}

/// Actions queued by views and applied after drawing.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Open(Page),
    OpenChat(ChatId),
    /// Creates and opens a chat for a contact without one.
    StartChat {
        id: ChatId,
        name: String,
    },
    /// Opens a chat at a message search result.
    OpenMessage {
        chat: ChatId,
        message: String,
    },
    CloseChat,
    SendText {
        chat: ChatId,
        text: String,
        /// Quoted message id.
        quoting: Option<String>,
    },
    CreatePoll {
        chat: ChatId,
        draft: PollDraft,
    },
    RefreshPoll {
        chat: ChatId,
        message: String,
    },
    VotePoll {
        chat: ChatId,
        message: String,
        choices: Vec<usize>,
    },
    /// Updates our typing state in a chat.
    Composing {
        chat: ChatId,
        composing: bool,
    },
    MarkRead(ChatId),
    /// Joins a group with an invite code after the user confirms.
    JoinGroup {
        code: String,
    },
    /// Opens a view-once file a single time. The file is not kept.
    OpenViewOnce {
        chat: ChatId,
        message: String,
    },
    /// Opens the group rename/photo editor for a group chat.
    OpenGroupEdit(ChatId),
    /// Renames a group; confirmed only after the server answers.
    GroupRename {
        chat: ChatId,
        name: String,
    },
    /// Replaces a group photo from a file; confirmed after the server.
    GroupSetPhoto {
        chat: ChatId,
        path: PathBuf,
    },
    /// Removes a group photo; confirmed after the server answers.
    GroupRemovePhoto(ChatId),
    /// Opens the file picker for a group photo; cancel does nothing.
    GroupPickPhoto(ChatId),
    /// Discards a group edit without sending (Esc/cancel).
    GroupEditCancel(ChatId),
    LoadOlder(ChatId),
    /// Requests messages older than the local archive.
    FetchOlder(ChatId),
    Download {
        chat: ChatId,
        message: String,
    },
    /// Plays or pauses a downloaded voice or audio message.
    PlayVoice {
        message: String,
        path: PathBuf,
    },
    /// Seeks to a fraction from 0 to 1 and starts playback.
    SeekVoice {
        message: String,
        path: PathBuf,
        fraction: f32,
    },
    /// Starts, cancels, or sends a voice recording.
    StartRecording,
    CancelRecording,
    SendRecording,
    OpenFile(PathBuf),
    /// A short error the interface shows, with no other effect.
    ToastError(String),
    /// A short confirmation the interface shows, with no other effect.
    ToastInfo(String),
    /// Reveals a file in its folder, selecting it. Programs open this way
    /// instead of running: clicking one must never execute it.
    ShowInFolder(PathBuf),
    /// Opens the media viewer on a picture or sticker in a chat.
    OpenViewer {
        chat: ChatId,
        message: String,
    },
    /// Moves the media viewer one item forwards or backwards.
    ViewerStep(i32),
    /// Moves to another page of the PDF in the media viewer.
    ViewerPage(i32),
    /// Jumps to a page of the PDF in the media viewer, counted from one.
    ViewerPageTo(usize),
    /// Turns the PDF page on screen a quarter clockwise.
    ViewerRotate,
    /// Zooms the media viewer around an anchor in points from its centre.
    ViewerZoom {
        factor: f32,
        anchor: (f32, f32),
    },
    /// Drags the zoomed picture in the media viewer.
    ViewerPan((f32, f32)),
    /// Returns the media viewer to fit the window.
    ViewerFit,
    /// Plays or pauses the video open in the media viewer.
    VideoToggle,
    /// Jumps to a fraction of the video open in the viewer, from 0 to 1.
    VideoSeek(f32),
    /// Cancels a scrub drag: restores the pre-drag state with no jump.
    VideoScrubCancel,
    /// Applies a video output level while it plays; saving follows on release.
    VideoVolume(f32),
    /// Mutes or unmutes the video open in the viewer.
    VideoMuteToggle,
    /// Closes the media viewer.
    CloseViewer,
    /// Opens or closes the search bar inside the open chat.
    ToggleChatSearch,
    /// The in-chat search text changed.
    ChatSearch(String),
    /// Closes the search bar inside the open chat.
    CloseChatSearch,
    /// Asks for a path and saves a copy of a file the app shows.
    SaveCopy(PathBuf),
    /// Steps one message's playback speed through its fixed cycle.
    CycleAudioSpeed(String),
    OpenUrl(String),
    /// Opens a video link preview in the viewer. A page address is not loaded.
    OpenLinkVideo {
        chat: ChatId,
        message: String,
    },
    CopyText(String),
    /// Starts a reply to a message in the open chat.
    Reply(String),
    CancelReply,
    /// Forwards one message to another chat.
    Forward {
        from_chat: ChatId,
        message: String,
        to_chat: ChatId,
    },
    /// Forwards selected messages to up to five chats. The worker caps
    /// frequently forwarded messages at one destination, like WhatsApp.
    ForwardMany {
        from_chat: ChatId,
        messages: Vec<String>,
        to_chats: Vec<ChatId>,
    },
    /// Toggles a message in the multi-select set of the open chat.
    ToggleSelect(String),
    /// Selects every selectable message from the previous selection anchor to this one.
    SelectRange(String),
    /// Leaves multi-select mode without doing anything.
    ClearSelection,
    /// Loads an outgoing message into the composer for editing.
    Edit(String),
    CancelEdit,
    /// Revokes an outgoing message for everyone.
    DeleteForEveryone(String),
    /// Deletes a message locally.
    DeleteForMe(String),
    /// Deletes selected messages: the revocable ones for everyone when
    /// asked, everything else only here.
    DeleteMany {
        ids: Vec<String>,
        for_everyone: bool,
    },
    /// Opens the attachment picker for the current chat.
    Attach,
    SendFiles(Vec<PathBuf>),
    /// Clipboard image as straight-alpha RGBA.
    PasteImage {
        width: usize,
        height: usize,
        rgba: Vec<u8>,
    },
    /// Toggles a picker tab.
    TogglePicker(PickerTab),
    ClosePicker,
    /// Requests the next Received-stickers page for the current generation.
    LoadMoreReceived,
    /// Inserts an emoji at the composer cursor.
    InsertEmoji(String),
    /// Replaces an active `:query` with its selected emoji.
    InsertEmojiCompletion {
        emoji: String,
        start: usize,
        end: usize,
    },
    CloseEmojiSuggestions,
    /// Replaces the active `@` query with a selected group member.
    InsertMention {
        id: String,
        name: String,
        start: usize,
        end: usize,
    },
    CloseMentions,
    SendSticker(PathBuf),
    /// Deletes a cached file that never decodes and downloads it again.
    HealSticker {
        path: PathBuf,
    },
    /// Deletes a thumbnail that never decodes so the worker rebuilds it
    /// from the original file. The original is never touched.
    HealStickerThumb {
        path: PathBuf,
    },
    /// Manual update check from the About dialog.
    CheckUpdatesNow,
    /// Saves a sticker for the picker.
    SaveSticker(PathBuf),
    /// Removes a saved sticker.
    ForgetSticker(PathBuf),
    /// Marks a sticker as a favourite, or clears the mark.
    FavoriteSticker(PathBuf),
    /// Downloads a sticker pack shared in a chat for preview.
    ViewStickerPack {
        chat: ChatId,
        message: String,
    },
    /// Copies a previewed pack into the packs folder.
    AddStickerPack {
        dir: PathBuf,
        name: String,
    },
    /// Shows one sticker bigger, without opening the media viewer.
    PeekSticker(PathBuf),
    /// Asks for the details of one attachment.
    ShowFileInfo {
        chat: ChatId,
        message: String,
    },
    /// Copies a picture to the system clipboard.
    CopyImage(PathBuf),
    /// Selects and imports a .wastickers or zip file.
    PickStickerArchive,
    /// Deletes an imported pack directory.
    DeleteStickerPack(PathBuf),
    /// Opens the prefilled contact-name editor.
    EditContact(String),
    /// Saves a contact through WhatsApp contact sync. `first` is the short
    /// display name and `last` completes the full name.
    SaveContact {
        id: String,
        first: String,
        last: String,
    },
    /// Checks a number, optionally saves it, and opens its chat.
    NewContact {
        phone: String,
        first: String,
        last: String,
    },
    React {
        chat: ChatId,
        message: String,
        emoji: String,
    },
    SetArchived(ChatId, bool),
    SetPinned(ChatId, bool),
    ShowDialog(Dialog),
    CloseDialog,
    ToggleSidebar,
    FocusSearch,
    FocusComposer,
    HideShortcutHints,
    ScrollToBottom,
    /// Scrolls the open chat to a message.
    ScrollTo(String),
    /// Updates chat-list search text.
    Search(String),
    ShowUpdate,
    CloseUpdate,
    DownloadUpdate,
    InstallUpdate,
    SetTheme(crate::settings::ThemeChoice),
    SetCustomTheme(String),
    ReloadThemes,
    OpenThemesFolder,
    SettingsChanged,
    ZoomBy(f32),
    ResetZoom,
    /// Requests a pairing code for a phone number.
    PairWithPhone(String),
    /// Unlinks the device remotely and locally.
    Unlink,
    Reconnect,
    /// Links again with a QR, keeping history until the accounts are compared.
    BeginPair,
    /// Links with a phone number, keeping history until the accounts are compared.
    BeginPairPhone(String),
    /// Deletes the previous archive after a different account paired.
    AcceptNewAccount,
    /// Drops the new pairing and keeps the previous archive.
    KeepOldAccount,
    Quit,
    /// Shows the window, creating it when running headless.
    ShowWindow,
    /// Closes the window while keeping the app in the tray.
    HideWindow,
    /// Applies the configured close-button behavior.
    CloseWindow,
    /// Mutes until Unix time, indefinitely with `Some(0)`, or unmutes with `None`.
    SetMuted(ChatId, Option<i64>),
    /// Sends pending attachments with the composer text as caption.
    SendPending {
        chat: ChatId,
        caption: String,
    },
    /// Removes one pending attachment.
    RemovePending(usize),
    /// Removes all pending attachments.
    ClearPending,
    /// Declines the call that is ringing now.
    RejectCall,
    /// Downloads one status photo or video.
    DownloadStory(String),
    /// Marks the open status seen.
    MarkStorySeen {
        id: String,
        sender: String,
    },
    /// Opens the sender's chat with this status quoted.
    ReplyToStatus {
        sender: String,
        id: String,
    },
    /// Publishes the status draft after the user confirmed.
    PostStatus,
    /// Asks for a photo to attach to the status draft.
    PickStatusPhoto,
    /// Moves the open status by one step. Negative goes back.
    StoryStep(i32),
    /// Starts the open status video in the in-app player.
    StatusVideo(PathBuf),
    /// Closes the open status.
    CloseStory,
    /// Pins or unpins one message. `seconds` is 0 to unpin.
    PinChatMessage {
        chat: ChatId,
        message: String,
        seconds: u32,
    },
    /// Stars or unstars one message. Synced with the phone.
    StarMessage {
        chat: ChatId,
        message: String,
        starred: bool,
    },
    /// Loads the Favorites screen. `chat` limits the list to one conversation.
    LoadFavorites {
        chat: Option<ChatId>,
        query: String,
        limit: u32,
    },
    /// Creates a group and adds the listed phone numbers.
    CreateGroup {
        name: String,
        participants: Vec<String>,
    },
    /// Adds one person to a group. Admin only.
    AddGroupMember {
        chat: ChatId,
        person: String,
    },
    /// Removes one person. Confirmed first.
    RemoveGroupMember {
        chat: ChatId,
        person: String,
    },
    /// Makes one person an admin.
    PromoteGroupMember {
        chat: ChatId,
        person: String,
    },
    /// Removes admin from one person. Confirmed first.
    DemoteGroupMember {
        chat: ChatId,
        person: String,
    },
    /// Sets the group description. Empty clears it.
    SetGroupDescription {
        chat: ChatId,
        description: String,
    },
    /// Only admins can send when `on` is set.
    SetGroupAnnounce {
        chat: ChatId,
        on: bool,
    },
    /// Only admins can edit the group info when `on` is set.
    SetGroupLocked {
        chat: ChatId,
        on: bool,
    },
    /// New members need approval when `on` is set.
    SetGroupApproval {
        chat: ChatId,
        on: bool,
    },
    /// Asks for the invite link. `reset` revokes the current one first.
    GroupInvite {
        chat: ChatId,
        reset: bool,
    },
    /// Loads people waiting to join.
    LoadJoinRequests {
        chat: ChatId,
    },
    /// Approves or denies one join request.
    DecideJoin {
        chat: ChatId,
        person: String,
        approve: bool,
    },
    /// Leaves the group. Confirmed first.
    LeaveGroup {
        chat: ChatId,
    },
    /// Writes one chat to a folder the user picks. Dates are Unix seconds.
    ExportChat {
        chat: ChatId,
        from: i64,
        until: i64,
    },
    /// Stops the export that is running.
    CancelExport,
}

/// A call that is ringing on the phone. `peer` and `creator` are signaling
/// addresses used only to decline. They are not shown.
#[derive(Clone, Debug, PartialEq)]
pub struct LiveCall {
    pub chat: ChatId,
    pub call_id: String,
    pub peer: String,
    pub creator: String,
    pub name: String,
    pub video: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_ai_is_the_bot_address_only() {
        assert!(is_meta_ai("13135550002@s.whatsapp.net"));
        assert!(is_meta_ai("13135550002:12@s.whatsapp.net"));
        assert!(is_meta_ai("1316555009000@s.whatsapp.net"));
        assert!(is_meta_ai("assistant@bot"));
        assert!(!is_meta_ai("393331234567@s.whatsapp.net"));
        assert!(!is_meta_ai("13135550002@lid"));
        assert!(!is_meta_ai("not a jid"));
    }

    #[test]
    fn trailing_dots_and_spaces_do_not_hide_programs() {
        // Windows strips trailing dots and spaces on disk, so the check
        // reads the name the same way.
        assert!(FileKind::of("application/octet-stream", "evil.bat ").runs_code());
        assert!(FileKind::of("application/octet-stream", "evil.exe.").runs_code());
        assert!(FileKind::of("application/octet-stream", "EVIL.PS1 . ").runs_code());
        assert!(FileKind::of("application/octet-stream", "note.vbs").runs_code());
        assert!(FileKind::of("application/octet-stream", "shortcut.lnk").runs_code());
        assert!(!FileKind::of("application/pdf", "report.pdf.").runs_code());
        assert!(!FileKind::of("image/jpeg", "photo.jpg ").runs_code());
        assert_eq!(canonical_file_name("evil.bat. . "), "evil.bat");
        assert_eq!(canonical_file_name("..."), "");
    }

    #[test]
    fn polls_validate_trimmed_questions_and_distinct_bounded_answers() {
        let mut draft = PollDraft {
            question: " Lunch? ".into(),
            options: vec![" Pizza ".into(), "Pasta".into()],
            multiple: false,
        };
        let valid = draft.validated().unwrap();
        assert_eq!(valid.question, "Lunch?");
        assert_eq!(valid.options, ["Pizza", "Pasta"]);
        assert_eq!(valid.selectable(), 1);
        draft.options[1] = "Pizza".into();
        assert!(draft.validated().is_err());
        draft.options[1].clear();
        assert!(draft.validated().is_err());
        draft.options = (0..13).map(|i| format!("Answer {i}")).collect();
        assert!(draft.validated().is_err());
        draft.options.pop();
        draft.multiple = true;
        assert_eq!(draft.validated().unwrap().selectable(), 12);
        draft.question = "🍕".repeat(256);
        assert!(draft.validated().is_err());
    }

    fn media() -> Media {
        Media {
            hash: None,
            mime: "image/jpeg".into(),
            size: 1,
            width: None,
            height: None,
            path: None,
            state: MediaState::Idle,
        }
    }

    #[test]
    fn full_summaries_keep_every_line_behind_the_summary_label() {
        assert_eq!(Content::text("hi\nthere").full_summary(), "hi\nthere");
        assert_eq!(
            Content::Image {
                caption: Some("look\nat this".into()),
                media: media()
            }
            .full_summary(),
            "Photo: look\nat this"
        );
        let voice = Content::Audio {
            media: media(),
            seconds: Some(65),
            voice_note: true,
            waveform: Vec::new(),
        };
        assert_eq!(voice.full_summary(), voice.summary());
    }

    #[test]
    fn only_plain_content_can_be_forwarded() {
        assert!(Content::text("hi").forwardable());
        assert!(
            Content::Image {
                media: media(),
                caption: None,
            }
            .forwardable()
        );
        assert!(!Content::Revoked.forwardable());
        assert!(
            !Content::Unsupported {
                what: "x".into(),
                reason: "unknown".into(),
            }
            .forwardable()
        );
        assert!(
            !Content::Poll {
                question: "q".into(),
                options: vec!["a".into(), "b".into()],
                state: Default::default(),
            }
            .forwardable()
        );
    }

    #[test]
    fn kinds_come_from_the_server_part() {
        assert_eq!(ChatKind::from_id("1@s.whatsapp.net"), ChatKind::Direct);
        assert_eq!(ChatKind::from_id("1@lid"), ChatKind::Direct);
        assert_eq!(ChatKind::from_id("1-2@g.us"), ChatKind::Group);
        assert_eq!(ChatKind::from_id("1@newsletter"), ChatKind::Broadcast);
    }

    #[test]
    fn sending_is_denied_for_channels_and_muted_groups_only() {
        let direct = Chat::new("1@s.whatsapp.net".into(), "Ada".into());
        assert!(can_send(&direct));
        let group = Chat::new("1-2@g.us".into(), "Club".into());
        assert!(can_send(&group));
        let mut community = Chat::new("9-9@g.us".into(), "Campus".into());
        community.community = true;
        assert!(can_send(&community));
        let channel = Chat::new("1@newsletter".into(), "News".into());
        assert!(!can_send(&channel));
        let mut muted = Chat::new("2@s.whatsapp.net".into(), "Bob".into());
        muted.read_only = true;
        assert!(!can_send(&muted));
        let mut admin_channel = Chat::new("2@newsletter".into(), "News".into());
        admin_channel.read_only = false;
        assert!(!can_send(&admin_channel));
    }

    #[test]
    fn summaries_read_like_whatsapp() {
        assert_eq!(Content::text("hi\nthere").summary(), "hi");
        assert_eq!(
            Content::Image {
                caption: Some("look".into()),
                media: media()
            }
            .summary(),
            "Photo: look"
        );
        assert_eq!(
            Content::Image {
                caption: None,
                media: media()
            }
            .summary(),
            "Photo"
        );
        assert_eq!(
            Content::Audio {
                media: media(),
                seconds: Some(65),
                voice_note: true,
                waveform: Vec::new()
            }
            .summary(),
            "Voice message (1:05)"
        );
        let invite = Content::Interactive {
            header: Some(InteractiveHeader::Title {
                text: "Thursday".into(),
            }),
            body: Some("Doors at 18:30".into()),
            footer: Some("Bring a jacket".into()),
            options: vec!["I'll be there".into()],
            note: None,
        };
        assert_eq!(invite.summary(), "Doors at 18:30");
        let group = Content::GroupInvite {
            name: "Picnic".into(),
            code: "abc".into(),
            caption: Some("Bring a blanket".into()),
        };
        assert_eq!(group.summary(), "Group invite: Picnic");
        assert_eq!(
            group.full_summary(),
            "Group invite: Picnic\nBring a blanket"
        );
        let event = Content::Event {
            title: "Talk".into(),
            description: Some("Slides after".into()),
            start: 0,
            end: None,
            location: Some("Hall".into()),
            cancelled: true,
        };
        assert_eq!(event.summary(), "Event: Talk");
        assert_eq!(
            event.full_summary(),
            "Event: Talk (cancelled)\nHall\nSlides after"
        );
        assert!(invite.full_summary().contains("Bring a jacket"));
        assert!(invite.full_summary().contains("I'll be there"));
    }

    #[test]
    fn a_video_preview_link_stays_in_the_viewer() {
        let preview = LinkPreview {
            url: "https://example.com/watch/clip".into(),
            title: Some("Evening".into()),
            description: None,
            video: true,
            video_url: Some("https://cdn.example.com/clip.mp4".into()),
        };
        assert!(preview.opens_in_viewer());
        let message = Message {
            id: "m".into(),
            chat: "1@s.whatsapp.net".into(),
            sender: "1@s.whatsapp.net".into(),
            sender_name: None,
            from_me: false,
            timestamp: 0,
            content: Content::Text {
                text: "https://example.com/watch/clip".into(),
                preview: Some(preview),
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
        };
        assert!(message.link_stays_in_viewer("https://example.com/watch/clip/"));
        assert!(message.link_stays_in_viewer("https://cdn.example.com/clip.mp4"));
        assert!(!message.link_stays_in_viewer("https://example.com/other"));
        let page = LinkPreview {
            url: "https://example.com/article".into(),
            title: None,
            description: None,
            video: false,
            video_url: None,
        };
        assert!(!page.opens_in_viewer());
    }

    #[test]
    fn phones_only_come_from_phone_ids() {
        assert_eq!(
            phone_of("393331234567@s.whatsapp.net"),
            Some("393331234567")
        );
        assert_eq!(phone_of("12345@lid"), None);
        assert_eq!(phone_of("1-2@g.us"), None);
    }

    #[test]
    fn labels_mark_names_people_chose_themselves() {
        let saved = Contact {
            id: "1".into(),
            full_name: Some("Ada".into()),
            push_name: Some("ada l".into()),
        };
        assert_eq!(saved.label().as_deref(), Some("Ada"));
        let stranger = Contact {
            id: "2".into(),
            full_name: None,
            push_name: Some("Bob".into()),
        };
        assert_eq!(stranger.label().as_deref(), Some("~Bob"));
        assert_eq!(Contact::default().label(), None);
    }

    #[test]
    fn old_text_content_still_parses() {
        let old: Content = serde_json::from_str(r#"{"kind":"text","text":"hi"}"#).expect("parses");
        assert_eq!(old, Content::text("hi"));
    }

    #[test]
    fn content_survives_json() {
        let content = Content::Document {
            media: media(),
            file_name: "a.pdf".into(),
            caption: None,
            pages: Some(3),
        };
        let json = serde_json::to_string(&content).expect("serializes");
        let back: Content = serde_json::from_str(&json).expect("parses");
        assert_eq!(back, content);
    }
}
