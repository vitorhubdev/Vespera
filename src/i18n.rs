//! Minimal localization base: English with a Simplified Chinese catalog.
//!
//! Not a full translation and not a framework import. UI code looks up
//! stable keys through [`t`]; Simplified Chinese covers the Settings screen,
//! the sticker picker tabs, and the Received footer, and everything else
//! falls back to English. Traditional Chinese systems stay in English
//! rather than show the wrong script (upstream parity).
//!
//! Fonts already prefer the locale regional Han cut
//! (`system_fonts::han_region`); this module only picks the string table.

use serde::{Deserialize, Serialize};

/// UI language preference.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    /// Follow the desktop locale: Simplified Chinese for Hans locales,
    /// English otherwise (including Traditional Chinese locales).
    #[default]
    Auto,
    English,
    Simplified,
}

impl Language {
    pub const ALL: [Language; 3] = [Language::Auto, Language::English, Language::Simplified];

    /// Option label. Language names stay in their own script on purpose.
    pub fn label(self) -> &'static str {
        match self {
            Language::Auto => "Auto",
            Language::English => "English",
            Language::Simplified => "Simplified Chinese",
        }
    }

    /// Effective table after resolving `Auto` against the desktop locale.
    pub fn resolved(self) -> Resolved {
        match self {
            Language::English => Resolved::En,
            Language::Simplified => Resolved::Sc,
            Language::Auto => detect(),
        }
    }
}

/// Resolved string table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolved {
    En,
    Sc,
}

/// Looks up a stable key. Simplified Chinese when resolved and present,
/// English otherwise. Unknown keys echo back (tests pin the catalog).
pub fn t(lang: Language, key: &str) -> String {
    if lang.resolved() == Resolved::Sc
        && let Some(text) = SC.iter().find(|(known, _)| *known == key)
    {
        return text.1.to_owned();
    }
    if let Some(text) = EN.iter().find(|(known, _)| *known == key) {
        return text.1.to_owned();
    }
    key.to_owned()
}

/// Lowercase user locale, or empty when unset (mirrors `system_fonts`).
fn locale() -> String {
    ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .find_map(|key| std::env::var(key).ok().filter(|value| !value.is_empty()))
        .unwrap_or_default()
        .to_lowercase()
}

/// Hans locales resolve to Simplified; Hant locales and everything else to
/// English. Specific Traditional markers win over the bare `zh` prefix.
fn detect() -> Resolved {
    detect_for(&locale())
}

fn detect_for(locale: &str) -> Resolved {
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

/// English source texts by stable key.
const EN: &[(&str, &str)] = &[
    ("settings.title", "Settings"),
    ("settings.appearance", "Appearance"),
    ("settings.theme", "Theme"),
    (
        "settings.theme_follow_system",
        "Follow system uses your desktop's light or dark appearance.",
    ),
    (
        "settings.theme_follow_omarchy",
        "Follow system uses your Omarchy colours.",
    ),
    ("settings.theme_dark", "Dark"),
    ("settings.theme_light", "Light"),
    ("settings.theme_system", "Follow system"),
    ("settings.open_themes", "Open themes folder"),
    ("settings.zoom", "Zoom"),
    (
        "settings.zoom_hint",
        "You can also use Ctrl+plus and Ctrl+minus.",
    ),
    ("settings.larger", "Larger"),
    ("settings.smaller", "Smaller"),
    ("settings.language", "Language"),
    (
        "settings.language_detail",
        "Simplified Chinese covers Settings and the sticker picker for now; everything else stays in English.",
    ),
    ("settings.chats", "Chats"),
    ("settings.enter_sends", "Enter sends"),
    (
        "settings.enter_sends_detail",
        "When off, Enter adds a line and Ctrl+Enter sends.",
    ),
    ("settings.read_receipts", "Send read receipts"),
    (
        "settings.receipts_on_detail",
        "Let people see when you read messages or play voice messages. Your WhatsApp privacy setting still applies. Read state syncs between your devices either way.",
    ),
    (
        "settings.receipts_off_detail",
        "Read receipts are disabled for your WhatsApp account. Direct chats will not send them. When this switch is on, groups still do. Read state syncs between your devices either way.",
    ),
    ("settings.typing", "Show when you are typing"),
    (
        "settings.auto_download",
        "Download attachments automatically",
    ),
    (
        "settings.auto_download_detail",
        "Download pictures, videos, voice messages, and documents up to 64 MB when they enter view. When off, click a file to download it.",
    ),
    (
        "settings.sender_pictures",
        "Show sender pictures in every chat",
    ),
    (
        "settings.sender_pictures_detail",
        "WhatsApp shows them in groups only.",
    ),
    ("settings.names_contacts", "Names from your address book"),
    (
        "settings.names_contacts_detail",
        "Prefer saved contact names. When off, prefer public WhatsApp profile names. This applies throughout the app.",
    ),
    (
        "settings.save_contacts",
        "Save contacts to the phone's address book",
    ),
    (
        "settings.save_contacts_detail",
        "Also add contacts saved here to your phone's address book. When off, they remain WhatsApp contacts. Names sync to linked devices either way.",
    ),
    ("settings.shortcut_hints", "Show shortcut hints"),
    ("settings.audio", "Audio"),
    (
        "settings.audio_note",
        "Each audio bubble carries its own speed chip: 1x, 1.5x or 2x. Changing it takes effect straight away, from where the clip already is.",
    ),
    ("settings.play_next", "Play the next audio automatically"),
    (
        "settings.play_next_detail",
        "When a voice message or audio clip ends, continue with the next one in the same chat.",
    ),
    ("settings.window", "Window"),
    (
        "settings.keep_running",
        "Keep running when the window closes",
    ),
    (
        "settings.keep_running_detail",
        "Keep Vespera linked in the system tray. Quit from the tray menu or with Ctrl+Q.",
    ),
    ("settings.notify", "Notify about new messages"),
    (
        "settings.notify_detail",
        "Show desktop notifications when the window is hidden, in the background, or showing another chat. Muted chats do not notify you.",
    ),
    ("settings.auto_update", "Download updates automatically"),
    (
        "settings.auto_update_detail",
        "Download and verify new releases in the background. You choose when to restart. Native packages and Flatpak update through their package manager.",
    ),
    ("settings.check_updates", "Check for updates"),
    (
        "settings.check_updates_detail",
        "Ask GitHub once a day whether a newer Vespera release exists. The request identifies only Vespera and its version.",
    ),
    ("settings.update_channel", "Update channel"),
    (
        "settings.update_channel_detail",
        "Stable is the default. Testing also offers release candidates, which may be unfinished. A newer finished release always wins on either channel.",
    ),
    ("settings.channel_stable", "Stable"),
    ("settings.channel_testing", "Testing"),
    ("settings.account", "Account"),
    ("settings.linked_device", "Linked device"),
    ("settings.unlink", "Unlink this computer"),
    ("settings.files", "Files"),
    ("settings.archive", "Message archive"),
    ("settings.downloads", "Downloaded attachments"),
    ("settings.log", "Log of this run"),
    ("settings.open_folder", "Open folder"),
    ("settings.open", "Open"),
    ("settings.about", "About"),
    (
        "settings.about_detail",
        "A native WhatsApp client built with Rust, egui, and whatsapp-rust.",
    ),
    ("settings.shortcuts", "Shortcuts"),
    ("picker.emoji", "Emoji"),
    ("picker.stickers", "Stickers"),
    ("picker.received", "Received"),
    ("picker.favourites", "Favourites"),
    (
        "picker.received_empty",
        "Stickers you receive appear here, newest first. Send or receive a sticker to fill this tab.",
    ),
    ("picker.received_loading", "Loading received stickers…"),
    ("picker.show_more", "Show more received"),
    ("picker.loading_more", "Loading more received stickers…"),
    ("picker.fav_loading", "Loading your stickers…"),
    (
        "picker.fav_empty",
        "Right-click a sticker, here or in a chat, and choose Add to favourites to keep it in this tab.",
    ),
    (
        "picker.stickers_hint",
        "Your saved stickers appear first, then imported packs, then the phone recents. Mark a sticker as a favourite to keep it in its own tab.",
    ),
];

/// Simplified Chinese strings by the same stable keys. Traditional Chinese
/// locales intentionally resolve to English instead of this table.
const SC: &[(&str, &str)] = &[
    ("settings.title", "设置"),
    ("settings.appearance", "外观"),
    ("settings.theme", "主题"),
    (
        "settings.theme_follow_system",
        "跟随系统将使用您桌面的浅色或深色外观。",
    ),
    (
        "settings.theme_follow_omarchy",
        "跟随系统将使用您的 Omarchy 配色。",
    ),
    ("settings.theme_dark", "深色"),
    ("settings.theme_light", "浅色"),
    ("settings.theme_system", "跟随系统"),
    ("settings.open_themes", "打开主题文件夹"),
    ("settings.zoom", "缩放"),
    ("settings.zoom_hint", "您也可以使用 Ctrl+加号和 Ctrl+减号。"),
    ("settings.larger", "放大"),
    ("settings.smaller", "缩小"),
    ("settings.language", "语言"),
    (
        "settings.language_detail",
        "简体中文目前覆盖设置和贴纸选择器，其余部分仍为英文。",
    ),
    ("settings.chats", "聊天"),
    ("settings.enter_sends", "回车发送"),
    (
        "settings.enter_sends_detail",
        "关闭时，回车换行，Ctrl+回车发送。",
    ),
    ("settings.read_receipts", "发送已读回执"),
    (
        "settings.receipts_on_detail",
        "让对方看到您是否已读消息或播放语音消息。您的 WhatsApp 隐私设置仍然有效。已读状态会在您的设备之间同步。",
    ),
    (
        "settings.receipts_off_detail",
        "您的 WhatsApp 账号已禁用已读回执。单聊不会发送。开启此开关后，群组仍会发送。已读状态会在您的设备之间同步。",
    ),
    ("settings.typing", "显示我正在输入"),
    ("settings.auto_download", "自动下载附件"),
    (
        "settings.auto_download_detail",
        "进入视图时自动下载图片、视频、语音消息和文档（最大 64 MB）。关闭后，点击文件即可下载。",
    ),
    ("settings.sender_pictures", "在所有聊天中显示发送者头像"),
    (
        "settings.sender_pictures_detail",
        "WhatsApp 仅在群组中显示。",
    ),
    ("settings.names_contacts", "使用通讯录中的名称"),
    (
        "settings.names_contacts_detail",
        "优先使用已保存的联系人姓名。关闭时，优先使用 WhatsApp 公开个人资料名称。此设置应用于整个应用。",
    ),
    ("settings.save_contacts", "保存联系人到手机通讯录"),
    (
        "settings.save_contacts_detail",
        "同时将在此处保存的联系人添加到手机通讯录。关闭后，他们仍仅为 WhatsApp 联系人。姓名会同步到已关联设备。",
    ),
    ("settings.shortcut_hints", "显示快捷键提示"),
    ("settings.audio", "音频"),
    (
        "settings.audio_note",
        "每个语音气泡都有自己的变速按钮：1x、1.5x 或 2x。切换会立即生效，从当前播放位置继续。",
    ),
    ("settings.play_next", "自动播放下一条语音"),
    (
        "settings.play_next_detail",
        "当语音消息或音频片段播放结束时，继续播放同一聊天中的下一条。",
    ),
    ("settings.window", "窗口"),
    ("settings.keep_running", "关闭窗口时保持运行"),
    (
        "settings.keep_running_detail",
        "保持 Vespera 在系统托盘中关联。可以通过托盘菜单或 Ctrl+Q 退出。",
    ),
    ("settings.notify", "新消息通知"),
    (
        "settings.notify_detail",
        "当窗口隐藏、处于后台或正在查看其他聊天时显示桌面通知。已静音的聊天不会通知。",
    ),
    ("settings.auto_update", "自动下载更新"),
    (
        "settings.auto_update_detail",
        "在后台下载并验证新版本。由您选择何时重启。通过原生包和 Flatpak 安装的版本通过包管理器更新。",
    ),
    ("settings.check_updates", "检查更新"),
    (
        "settings.check_updates_detail",
        "每天询问一次 GitHub 是否有更新的 Vespera 版本。该请求仅标识 Vespera 及其版本。",
    ),
    ("settings.update_channel", "更新渠道"),
    (
        "settings.update_channel_detail",
        "默认使用稳定版。测试版还提供可能尚未完成的候选版本。无论哪个渠道，较新的正式版本优先。",
    ),
    ("settings.channel_stable", "稳定版"),
    ("settings.channel_testing", "测试版"),
    ("settings.account", "账号"),
    ("settings.linked_device", "已关联设备"),
    ("settings.unlink", "取消关联此电脑"),
    ("settings.files", "文件"),
    ("settings.archive", "消息存档"),
    ("settings.downloads", "已下载的附件"),
    ("settings.log", "本次运行日志"),
    ("settings.open_folder", "打开文件夹"),
    ("settings.open", "打开"),
    ("settings.about", "关于"),
    (
        "settings.about_detail",
        "使用 Rust、egui 和 whatsapp-rust 构建的原生 WhatsApp 客户端。",
    ),
    ("settings.shortcuts", "快捷键"),
    ("picker.emoji", "表情符号"),
    ("picker.stickers", "贴纸"),
    ("picker.received", "收到的"),
    ("picker.favourites", "收藏"),
    (
        "picker.received_empty",
        "您收到的贴纸会显示在这里，按最新优先排序。发送或接收贴纸即可填充此选项卡。",
    ),
    ("picker.received_loading", "正在加载收到的贴纸…"),
    ("picker.show_more", "显示更多收到的贴纸"),
    ("picker.loading_more", "正在加载更多收到的贴纸…"),
    ("picker.fav_loading", "正在加载您的贴纸…"),
    (
        "picker.fav_empty",
        "右键点击贴纸（此处或聊天中）并选择添加到收藏，即可将其保留在此选项卡中。",
    ),
    (
        "picker.stickers_hint",
        "您保存的贴纸排在前面，其次是导入的贴纸包，最后是手机上的最近使用。将贴纸标为收藏可将其保留在单独的选项卡中。",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn simplified_covers_stable_unique_nonempty_keys() {
        let english: HashSet<_> = EN.iter().map(|(key, _)| *key).collect();
        let simplified: HashSet<_> = SC.iter().map(|(key, _)| *key).collect();
        assert_eq!(EN.len(), english.len(), "duplicate English keys");
        assert_eq!(SC.len(), simplified.len(), "duplicate Simplified keys");
        assert_eq!(simplified, english, "both tables cover the same keys");
        for (key, text) in SC {
            assert!(!text.is_empty(), "{key} must not be empty");
        }
    }

    #[test]
    fn english_fallback_and_hans_hant_mapping() {
        assert_eq!(t(Language::English, "settings.title"), "Settings");
        assert_eq!(t(Language::Simplified, "settings.title"), "设置");
        // Missing keys echo back instead of panicking.
        assert_eq!(t(Language::Simplified, "no.such.key"), "no.such.key");
        // Traditional Chinese stays in English rather than Simplified.
        assert_eq!(detect_for("zh_cn.utf-8"), Resolved::Sc);
        assert_eq!(detect_for("zh_sg.utf-8"), Resolved::Sc);
        assert_eq!(detect_for("zh_hans"), Resolved::Sc);
        assert_eq!(detect_for("zh_tw.utf-8"), Resolved::En);
        assert_eq!(detect_for("zh_hk.utf-8"), Resolved::En);
        assert_eq!(detect_for("zh_hant"), Resolved::En);
        assert_eq!(detect_for("en_us.utf-8"), Resolved::En);
        assert_eq!(detect_for(""), Resolved::En);
        assert_eq!(Language::Auto.resolved(), detect());
    }
}
