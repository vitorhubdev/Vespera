//! Desktop notifications when the app is hidden, unfocused, or on another chat.
//!
//! Delivery uses the platform notification service. Each notification runs on
//! its own thread because delivery and click handling can block.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[cfg(target_os = "windows")]
mod windows;

#[cfg(any(target_os = "macos", test))]
const MACOS_APPLICATION_ID: &str = "io.github.vitorhubdev.Vespera";

#[cfg(target_os = "macos")]
fn macos_application_ready() -> bool {
    static READY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *READY.get_or_init(|| {
        // The library's implicit default looks up an app named "use_default"
        // through AppleScript, which opens macOS's application chooser.
        match notify_rust::set_application(MACOS_APPLICATION_ID) {
            Ok(()) => true,
            Err(error) => {
                log::debug!("could not initialize notification application: {error}");
                false
            }
        }
    })
}

/// Cancellation is registered before delivery starts, so reading a chat while
/// its notification is still being delivered cannot leave a stale notification.
///
/// Policy: at most MAX_PENDING_GLOBAL deliveries wait for interaction,
/// oldest first. Evicting or clearing cancels the delivery: on Linux the shown
/// notification is closed; on Windows/macOS cancellation stops a pending
/// delivery, while an already shown toast stays in the center and stays
/// clickable. Shutdown drops all senders, which closes Linux waiters. A
/// failed thread spawn removes its entry instead of leaking it.
const MAX_PENDING_GLOBAL: usize = 32;
#[derive(Default)]
pub struct Notifications {
    /// Pending deliveries per chat, each tagged with its message id so one
    /// deleted message cancels only its own notification.
    pending: std::collections::HashMap<String, Vec<(String, tokio::sync::oneshot::Sender<()>)>>,
    /// Insertion order for oldest-first eviction across chats.
    order: std::collections::VecDeque<(String, String)>,
}

/// What the user did with a notification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToastChoice {
    /// The body was clicked.
    Open,
    /// A reply was typed. The text is already trimmed and non-empty.
    Reply(String),
    /// Mark the chat read without opening it.
    Read,
    /// Reply was clicked with nothing to send.
    Ignore,
}

/// A reply or read action queued for the interface thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotificationCommand {
    Reply { chat: String, text: String },
    Read { chat: String },
}

/// Identifies which chat/message a notification opens when clicked.
/// Grouping these keeps `show`/`deliver` under Clippy's argument limit.
#[derive(Clone, Debug)]
pub struct NotificationTarget {
    pub chat: String,
    pub message: String,
    pub opened: Arc<Mutex<Vec<(String, String)>>>,
    /// Reply and mark-as-read actions. Message toasts set `can_reply`.
    pub replies: Arc<Mutex<Vec<NotificationCommand>>>,
    pub can_reply: bool,
}

impl NotificationTarget {
    pub fn new(chat: String, message: String, opened: Arc<Mutex<Vec<(String, String)>>>) -> Self {
        Self {
            chat,
            message,
            opened,
            replies: Default::default(),
            can_reply: false,
        }
    }

    /// A message toast that can reply or mark the chat read.
    pub fn with_replies(mut self, replies: Arc<Mutex<Vec<NotificationCommand>>>) -> Self {
        self.replies = replies;
        self.can_reply = true;
        self
    }
}

/// Maps a toast activation to one choice. An empty reply does nothing.
pub fn toast_choice(argument: Option<&str>, reply: &str) -> ToastChoice {
    match argument {
        Some("reply") => {
            let reply = reply.trim();
            if reply.is_empty() {
                ToastChoice::Ignore
            } else {
                ToastChoice::Reply(reply.to_owned())
            }
        }
        Some("read") => ToastChoice::Read,
        _ => ToastChoice::Open,
    }
}

/// Applies one toast choice to the queues the interface drains.
pub fn apply_choice(choice: ToastChoice, target: &NotificationTarget, wake: impl Fn()) {
    match choice {
        ToastChoice::Ignore => {}
        ToastChoice::Open => {
            target
                .opened
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push((target.chat.clone(), target.message.clone()));
            wake();
        }
        ToastChoice::Reply(text) => {
            target
                .replies
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(NotificationCommand::Reply {
                    chat: target.chat.clone(),
                    text,
                });
            wake();
        }
        ToastChoice::Read => {
            target
                .replies
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(NotificationCommand::Read {
                    chat: target.chat.clone(),
                });
            wake();
        }
    }
}

/// Toast XML for a message: a reply field and a mark-as-read button.
pub fn message_xml(title: &str, body: &str, picture: Option<&str>) -> String {
    let image = picture
        .map(|path| {
            format!(
                "<image placement=\"appLogoOverride\" hint-crop=\"circle\" src=\"file:///{}\" alt=\"Sender\" />",
                xml_text(path)
            )
        })
        .unwrap_or_default();
    format!(
        "<toast><visual><binding template=\"ToastGeneric\">{image}<text>{title}</text><text>{body}</text></binding></visual><actions><input id=\"reply\" type=\"text\" placeHolderContent=\"Reply\"/><action content=\"Reply\" arguments=\"reply\" hint-inputId=\"reply\"/><action content=\"Mark as read\" arguments=\"read\"/></actions></toast>",
        title = xml_text(title),
        body = xml_text(body),
    )
}

fn xml_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

impl Notifications {
    fn total(&self) -> usize {
        self.pending.values().map(|entries| entries.len()).sum()
    }

    fn remove_entry(&mut self, chat: &str, message: &str) {
        if let Some(entries) = self.pending.get_mut(chat) {
            entries.retain(|(id, _)| id != message);
            if entries.is_empty() {
                self.pending.remove(chat);
            }
        }
        self.order
            .retain(|(known_chat, known_id)| known_chat != chat || known_id != message);
    }

    fn evict_oldest(&mut self) {
        while let Some((chat, message)) = self.order.pop_front() {
            if let Some(entries) = self.pending.get_mut(&chat)
                && let Some(position) = entries.iter().position(|(id, _)| *id == message)
            {
                let (_, cancel) = entries.remove(position);
                let _ = cancel.send(());
                if entries.is_empty() {
                    self.pending.remove(&chat);
                }
                return;
            }
        }
    }

    fn register(&mut self, chat: &str, message: &str) -> tokio::sync::oneshot::Receiver<()> {
        self.pending.retain(|_, entries| {
            entries.retain(|(_, entry)| !entry.is_closed());
            !entries.is_empty()
        });
        self.order.retain(|(known_chat, known_id)| {
            self.pending
                .get(known_chat)
                .is_some_and(|entries| entries.iter().any(|(id, _)| id == known_id))
        });
        while self.total() >= MAX_PENDING_GLOBAL {
            self.evict_oldest();
            if self.order.is_empty() {
                break;
            }
        }
        let (cancel, cancelled) = tokio::sync::oneshot::channel();
        self.pending
            .entry(chat.to_owned())
            .or_default()
            .push((message.to_owned(), cancel));
        self.order.push_back((chat.to_owned(), message.to_owned()));
        cancelled
    }

    pub fn clear(&mut self, chat: &str) {
        if let Some(entries) = self.pending.remove(chat) {
            for (_, cancel) in entries {
                let _ = cancel.send(());
            }
        }
        self.order.retain(|(known_chat, _)| known_chat != chat);
    }

    /// Cancels the pending notification of one deleted message, if any.
    /// Delivered OS notifications cannot be retracted; this only stops
    /// one that has not gone out yet.
    pub fn clear_message(&mut self, chat: &str, message: &str) {
        self.remove_entry(chat, message);
    }

    /// Drops every waiter: Linux waiters close, pending pre-show deliveries
    /// stop, and already shown Windows/macOS toasts stay clickable.
    pub fn clear_all(&mut self) {
        self.pending.clear();
        self.order.clear();
    }

    /// Shows a notification; platform delivery runs outside the interface thread.
    pub fn show(
        &mut self,
        title: String,
        body: String,
        picture: Option<PathBuf>,
        target: NotificationTarget,
        wake: impl Fn() + Send + 'static,
    ) {
        let chat = target.chat.clone();
        let message = target.message.clone();
        let cancelled = self.register(&chat, &message);
        let spawned = std::thread::Builder::new()
            .name("notification".into())
            .spawn(move || deliver(&title, &body, picture.as_deref(), target, wake, cancelled));
        if let Err(error) = spawned {
            self.remove_entry(&chat, &message);
            log::debug!("no thread for a notification: {error}");
        }
    }
}

/// Builds the notification title and body, including the group sender.
pub fn lines(chat_name: &str, is_group: bool, sender: &str, summary: &str) -> (String, String) {
    let body = if is_group {
        format!("{sender}: {summary}")
    } else {
        summary.to_owned()
    };
    (chat_name.to_owned(), body)
}

#[cfg(target_os = "linux")]
fn deliver(
    title: &str,
    body: &str,
    picture: Option<&std::path::Path>,
    target: NotificationTarget,
    wake: impl Fn() + Send + 'static,
    mut cancelled: tokio::sync::oneshot::Receiver<()>,
) {
    let NotificationTarget {
        chat,
        message,
        opened,
        ..
    } = target;
    if !matches!(
        cancelled.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ) {
        return;
    }
    let mut notification = notify_rust::Notification::new();
    notification
        .appname("Vespera")
        .summary(title)
        .body(body)
        .icon("vespera")
        .action("default", "Open");
    if let Some(picture) = picture {
        notification.image_path(&picture.to_string_lossy());
    }
    match notification.show() {
        Ok(handle) => {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    handle.close();
                    log::debug!("no notification action runtime: {error}");
                    return;
                }
            };
            runtime.block_on(async {
                tokio::select! {
                    biased;
                    _ = &mut cancelled => handle.close_async().await,
                    _ = handle.wait_for_action_async(|action| {
                        if matches!(action, notify_rust::NotificationResponse::Default) {
                            opened
                                .lock()
                                .unwrap_or_else(|p| p.into_inner())
                                .push((chat, message));
                            wake();
                        }
                    }) => {}
                }
            });
        }
        Err(error) => log::debug!("no notification: {error}"),
    }
}

#[cfg(target_os = "windows")]
fn deliver(
    title: &str,
    body: &str,
    picture: Option<&std::path::Path>,
    target: NotificationTarget,
    wake: impl Fn() + Send + 'static,
    mut cancelled: tokio::sync::oneshot::Receiver<()>,
) {
    if !matches!(
        cancelled.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ) {
        return;
    }
    if target.can_reply {
        if let Err(error) = windows::show_message(title, body, picture, move |choice| {
            apply_choice(choice, &target, &wake);
        }) {
            log::debug!("no Windows notification: {error}");
        }
        return;
    }
    let NotificationTarget {
        chat,
        message,
        opened,
        ..
    } = target;
    let activated = move || {
        opened
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push((chat.clone(), message.clone()));
        wake();
    };
    if let Err(error) = windows::show(title, body, picture, activated) {
        log::debug!("no Windows notification: {error}");
    }
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
fn deliver(
    title: &str,
    body: &str,
    picture: Option<&std::path::Path>,
    _target: NotificationTarget,
    _wake: impl Fn() + Send + 'static,
    mut cancelled: tokio::sync::oneshot::Receiver<()>,
) {
    // Never fall back to application discovery, including for unbundled builds.
    #[cfg(target_os = "macos")]
    if !macos_application_ready() {
        return;
    }
    if !matches!(
        cancelled.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ) {
        return;
    }
    let mut notification = notify_rust::Notification::new();
    notification.appname("Vespera").summary(title).body(body);
    // Windows uses the image; macOS always uses the app icon.
    if let Some(picture) = picture {
        notification.image_path(&picture.to_string_lossy());
    }
    if let Err(error) = notification.show() {
        log::debug!("no notification: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_notification_identity_matches_the_packaged_application() {
        let plist = include_str!("../packaging/macos/Info.plist");
        assert!(plist.contains(&format!(
            "<key>CFBundleIdentifier</key><string>{MACOS_APPLICATION_ID}</string>"
        )));
    }

    #[test]
    fn reading_cancels_delivered_and_pending_notifications_for_only_that_chat() {
        let mut notifications = Notifications::default();
        let mut first = notifications.register("a", "m1");
        let mut second = notifications.register("a", "m2");
        let mut other = notifications.register("b", "m3");
        notifications.clear("a");
        assert_eq!(first.try_recv(), Ok(()));
        assert_eq!(second.try_recv(), Ok(()));
        assert_eq!(
            other.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        );
        let mut next = notifications.register("a", "m4");
        assert_eq!(
            next.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        );
        notifications.clear_all();
        assert!(other.try_recv().is_err());
        assert_eq!(
            next.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Closed)
        );
    }

    #[test]
    fn expired_notifications_do_not_accumulate() {
        let mut notifications = Notifications::default();
        drop(notifications.register("a", "m1"));
        let _next = notifications.register("b", "m2");
        assert!(!notifications.pending.contains_key("a"));
    }

    #[test]
    fn deleting_one_message_cancels_only_its_notification() {
        let mut notifications = Notifications::default();
        let mut first = notifications.register("a", "m1");
        let mut second = notifications.register("a", "m2");
        notifications.clear_message("a", "m1");
        assert_eq!(
            first.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Closed)
        );
        assert_eq!(
            second.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        );
        notifications.clear_message("missing", "m1");
        notifications.clear_message("a", "missing");
    }

    /// Shows a test notification with an optional cached picture:
    /// `cargo test --all-features shows_one -- --ignored --nocapture`.
    #[test]
    #[ignore = "shows a real notification"]
    fn shows_one_on_this_desktop() {
        let picture = std::fs::read_dir(crate::paths::AppDirs::discover().avatar_cache_dir())
            .ok()
            .and_then(|entries| entries.flatten().map(|entry| entry.path()).next());
        let mut notifications = Notifications::default();
        notifications.show(
            "Ada Lovelace".into(),
            "A test from Vespera, with a picture".into(),
            picture,
            NotificationTarget::new("test".into(), "test-message".into(), Default::default()),
            || {},
        );
        std::thread::sleep(std::time::Duration::from_secs(2));
    }

    #[test]
    fn notification_target_keeps_chat_and_message_together() {
        let opened: Arc<Mutex<Vec<(String, String)>>> = Default::default();
        let target = NotificationTarget::new("chat-1".into(), "msg-1".into(), Arc::clone(&opened));
        assert_eq!(target.chat, "chat-1");
        assert_eq!(target.message, "msg-1");
        target
            .opened
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push((target.chat.clone(), target.message.clone()));
        assert_eq!(
            *opened.lock().unwrap_or_else(|p| p.into_inner()),
            vec![("chat-1".to_owned(), "msg-1".to_owned())]
        );
    }

    #[test]
    fn notification_lines_cover_groups_directs_and_empty_summaries() {
        assert_eq!(
            lines("Rust Berlin", true, "Mira", "Save me a seat"),
            ("Rust Berlin".to_owned(), "Mira: Save me a seat".to_owned())
        );
        assert_eq!(
            lines("Ada Lovelace", false, "Ada Lovelace", "Photo"),
            ("Ada Lovelace".to_owned(), "Photo".to_owned())
        );
        // Empty summary still yields a usable title/body pair.
        assert_eq!(
            lines("Chat", false, "Someone", ""),
            ("Chat".to_owned(), String::new())
        );
        assert_eq!(
            lines("Group", true, "", "hi"),
            ("Group".to_owned(), ": hi".to_owned())
        );
    }

    #[test]
    fn a_group_names_the_sender_and_a_chat_does_not() {
        assert_eq!(
            lines("Rust Berlin", true, "Mira", "Save me a seat"),
            ("Rust Berlin".to_owned(), "Mira: Save me a seat".to_owned())
        );
        assert_eq!(
            lines("Ada Lovelace", false, "Ada Lovelace", "Photo"),
            ("Ada Lovelace".to_owned(), "Photo".to_owned())
        );
    }

    #[test]
    fn a_reply_and_a_read_do_not_open_the_chat() {
        let opened = Arc::new(Mutex::new(Vec::new()));
        let replies = Arc::new(Mutex::new(Vec::new()));
        let target = NotificationTarget::new("chat".into(), "m1".into(), Arc::clone(&opened))
            .with_replies(Arc::clone(&replies));
        let wakes = std::cell::Cell::new(0);
        apply_choice(
            toast_choice(Some("reply"), "  On my way  "),
            &target,
            || wakes.set(wakes.get() + 1),
        );
        apply_choice(toast_choice(Some("reply"), "   "), &target, || {
            wakes.set(wakes.get() + 1);
        });
        apply_choice(toast_choice(Some("read"), ""), &target, || {
            wakes.set(wakes.get() + 1);
        });
        assert_eq!(wakes.get(), 2);
        assert!(opened.lock().unwrap().is_empty());
        assert_eq!(
            replies.lock().unwrap().clone(),
            vec![
                NotificationCommand::Reply {
                    chat: "chat".into(),
                    text: "On my way".into(),
                },
                NotificationCommand::Read {
                    chat: "chat".into()
                },
            ]
        );
    }

    #[test]
    fn the_message_toast_has_a_reply_field_and_escapes_text() {
        let xml = message_xml("A & B", "hello <there>", Some(r"C:\pic.png"));
        assert!(xml.contains("id=\"reply\""));
        assert!(xml.contains("placeHolderContent=\"Reply\""));
        assert!(xml.contains("arguments=\"read\""));
        assert!(xml.contains("Mark as read"));
        assert!(xml.contains("A &amp; B"));
        assert!(xml.contains("hello &lt;there&gt;"));
        assert!(!xml.contains("hello <there>"));
        assert!(xml.contains("file:///C:\\pic.png"));
    }

    struct FakeStat {
        started: std::sync::atomic::AtomicUsize,
        finished: std::sync::atomic::AtomicUsize,
        peak: std::sync::atomic::AtomicUsize,
    }

    fn fake_delivery(stat: &FakeStat, mut cancelled: tokio::sync::oneshot::Receiver<()>) {
        use std::sync::atomic::Ordering::SeqCst;
        let started = stat.started.fetch_add(1, SeqCst) + 1;
        let flying = started - stat.finished.load(SeqCst);
        stat.peak.fetch_max(flying, SeqCst);
        while matches!(
            cancelled.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ) {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        stat.finished.fetch_add(1, SeqCst);
    }

    #[test]
    fn global_cap_evicts_oldest_without_response() {
        let mut notifications = Notifications::default();
        let mut waiting = Vec::new();
        for index in 0..MAX_PENDING_GLOBAL {
            waiting.push(notifications.register("chat", &format!("m{index}")));
        }
        assert_eq!(notifications.total(), MAX_PENDING_GLOBAL);
        let mut evicted = notifications.register("chat", "newest");
        assert_eq!(notifications.total(), MAX_PENDING_GLOBAL);
        assert_eq!(notifications.order.len(), MAX_PENDING_GLOBAL);
        assert_eq!(waiting[0].try_recv(), Ok(()));
        assert!(matches!(
            evicted.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
    }

    #[test]
    fn eviction_terminates_a_blocked_delivery_under_cap() {
        use std::sync::atomic::Ordering::SeqCst;
        // Cancelling the map entry is not the proof: the evicted delivery
        // thread itself must observe the cancel and finish, keeping live
        // deliveries within the cap.
        let stat = std::sync::Arc::new(FakeStat {
            started: 0.into(),
            finished: 0.into(),
            peak: 0.into(),
        });
        let mut notifications = Notifications::default();
        let mut handles = Vec::new();
        for index in 0..MAX_PENDING_GLOBAL {
            let cancelled = notifications.register("chat", &format!("m{index}"));
            let stat_clone = std::sync::Arc::clone(&stat);
            handles.push(std::thread::spawn(move || {
                fake_delivery(&stat_clone, cancelled)
            }));
        }
        // Let every delivery block inside its cancel wait.
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert_eq!(stat.started.load(SeqCst), MAX_PENDING_GLOBAL);
        // The 33rd entry evicts the oldest live delivery.
        let _newest = notifications.register("chat", "newest");
        assert_eq!(notifications.total(), MAX_PENDING_GLOBAL);
        // The evicted thread terminates on its own: joining it is the
        // proof the entry removal was not.
        let evicted = handles.remove(0);
        evicted.join().expect("evicted delivery finishes");
        assert_eq!(stat.finished.load(SeqCst), 1);
        assert!(stat.peak.load(SeqCst) <= MAX_PENDING_GLOBAL);
        notifications.clear_all();
        for handle in handles {
            let _ = handle.join();
        }
        assert_eq!(stat.finished.load(SeqCst), MAX_PENDING_GLOBAL);
        assert_eq!(notifications.total(), 0);
    }

    #[test]
    fn slow_delivery_cancel_and_spawn_failure_stay_bounded() {
        use std::sync::atomic::Ordering::SeqCst;
        let stat = std::sync::Arc::new(FakeStat {
            started: 0.into(),
            finished: 0.into(),
            peak: 0.into(),
        });
        let mut notifications = Notifications::default();
        let mut handles = Vec::new();
        for index in 0..8 {
            let cancelled = notifications.register("chat", &format!("m{index}"));
            let stat_clone = std::sync::Arc::clone(&stat);
            handles.push(std::thread::spawn(move || {
                fake_delivery(&stat_clone, cancelled)
            }));
        }
        notifications.clear_message("chat", "m0");
        for handle in handles {
            notifications.clear_all();
            let _ = handle.join();
        }
        assert_eq!(stat.finished.load(SeqCst), 8);
        assert!(stat.peak.load(SeqCst) <= 8);
        assert_eq!(notifications.total(), 0);
        let _open = notifications.register("chat", "live");
        notifications.remove_entry("chat", "live");
        assert_eq!(notifications.total(), 0);
    }
}
