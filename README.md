<p align="center">
  <img src="assets/vespera.png" width="220" alt="Vespera logo">
</p>

# Vespera

**A community fork of ZapFast, native and fast.**

What this fork changes, compared with ZapFast 0.16.2, is listed in
[English](docs/differences.md), [português](docs/differences.pt-BR.md), and
[español](docs/differences.es.md).

Stable releases are on the GitHub Releases page; the latest stable is
**1.0.106**. Release candidates are opt-in builds on the same page,
marked **Pre-release**. They do not replace the stable download or the
updater's stable channel. Candidate builds carry accumulated fixes for
testing; live phone synchronization and installation rollback are not yet
validated. Archive and unarchive between this computer and the phone are
covered only by synthetic tests so far: live two-device validation is
still pending. Keep a backup of your existing profile before testing with
important data.

Vespera is an independent fork/mod maintained by
[Vitor (`@vitorhubdev`)](https://github.com/vitorhubdev). It is based on the
original [ZapFast](https://github.com/crmne/zapfast) project and keeps its MIT
license and original copyright notices. The fork adds its own desktop fixes,
packaging, updater path, branding, and extra behavior while preserving
compatibility-sensitive internal `vespera` identifiers where changing them
would break existing installations.

Vespera is written in Rust with [egui](https://github.com/emilk/egui) and uses
[whatsapp-rust](https://github.com/oxidezap/whatsapp-rust) for the WhatsApp Web
protocol. It runs on Linux, macOS, and Windows and links as a companion device
without embedding a browser engine.

**Downloads:** [latest Vespera release](https://github.com/vitorhubdev/Vespera/releases/latest)

> Vespera is not the upstream ZapFast project and is not affiliated with
> WhatsApp or Meta.

## What it does

- **Links to your phone.** Scan a QR code or link with your phone number.
  Recent history is copied to this computer after linking and stored here.
- **Chats.** See pinned, unread, muted, and archived chats, typing indicators,
  and message status. Search chats, saved messages, and contacts.
  Pinned chats stay in pin order (most recently pinned first), regardless of
  new messages. Chat and contact name searches ignore accents, so `Angel`
  finds `Ángel`.
  When a chat's one-line preview is cut short, resting the pointer on it
  shows the whole message in a tooltip, as in WhatsApp Web, without opening
  the chat or marking it read.
  Typing indicators show other participants, excluding your own linked devices.
- **Status.** See text, photo, and video status from the last 24 hours, unseen
  first. Opening one can send a seen receipt when read receipts are on. Reply
  opens that chat with the status quoted. Posting a text or photo asks for
  confirmation and a privacy choice. Expired status leaves on its own.
- **Pinned and favorite messages.** Pin a message in the chat for 24 hours,
  7 days, or 30 days. The banner at the top jumps to it, and several pins
  rotate. Star a message to keep it when a clear leaves favorites in place,
  and open Favorites to search them in one chat or all of them.
- **Group administration.** Create a group, add and remove people, make or
  dismiss admins, change the description and the send and edit rules, copy or
  revoke the invite link, and approve or deny join requests. Leave asks for
  confirmation. Those controls appear when you are an admin.
- **Export a chat.** Export one chat, or a range of dates, as text and HTML
  with media in a folder beside them. Deleted and edited messages that Vespera
  kept are included and marked. A manifest records the SHA-256 of each file
  and of the set. A panel shows progress, and Cancel stops the export.
- **Read state across devices.** Reading a chat syncs its unread badge with
  your phone and other linked devices, including when read receipts are off.
  Replies from another device clear preceding unread messages. The read-receipt
  toggle also controls voice-message played receipts; account privacy is checked
  before sending receipts in direct chats. A hidden window does not read messages.
- **Conversations.** See replies, reactions, edits, deleted messages, read
  receipts, sender names, and group pictures. Older messages load as you
  scroll up, first from the local archive and then from your phone.
  Deleting or clearing a chat on your phone applies here too: removed
  messages stay gone even when old history arrives later, while newer
  messages are kept. Each fresh connection also asks the phone for chat
  changes committed while this device was offline.
  The magnifier in the chat header searches inside that chat and jumps to any
  hit, and its results show the sender, the time, and the message text.
  Group messages show two gray checks after every recipient has received
  them, and blue checks after every recipient has read them. The recipient
  list and individual receipts are saved locally; later membership changes
  do not change that list. If the original recipients are unknown, Vespera
  waits for the phone's aggregate status instead of guessing from one reader.
- **WhatsApp formatting.** Bold, italic, strikethrough, code, lists, quotes,
  mentions, and link previews are supported. Links are clickable. Hebrew and
  Arabic RTL paragraphs keep logical word order by reordering font runs; this
  is not a full Unicode Bidirectional Algorithm. Emoji use the desktop's
  color emoji font, with a bundled fallback, and emoji-only messages are larger.
- **Send attachments with captions.** Paste a picture, drop files, or use the
  file picker. They stay in the composer until you send them or press Escape.
- **Mute chats** for eight hours, one week, or indefinitely. The setting also
  applies on your phone and to desktop notifications. Mute changes from your
  phone survive history arriving later, including during initial linking.
  Existing installations request one settings refresh after upgrading to
  recover previously lost mute settings and pin order, without relinking.
- **Voice messages.** Play, seek, record, reply with, and send voice messages
  in the chat. The app normalizes quiet recordings and handles OGG/Opus
  without external tools. A chip on every audio bubble walks that clip through
  1x, 1.5x and 2x with the pitch kept, taking effect at once and from where the clip already is,
  and the choice stays with that clip. When one ends, the next clip in the chat
  plays on its own unless you turn that off (Settings, Audio).
- **Send messages.** Press Enter to send text and Shift+Enter for a new line.
  You can swap these keys in Settings. The composer is focused when you open
  or return to a conversation; invoking search keeps focus in search, and
  Escape clears search and returns to the composer; another Escape closes the
  chat and saves your text draft. Open menus, dialogs, and unfinished actions
  are dismissed first. Type `:name` to autocomplete
  an emoji without leaving the composer, or `@` in a group to mention a member.
  Reply, react, edit, forward, delete, and check when a message was sent,
  delivered, or read. Clicking the quoted block of a message takes the chat to
  the message it answers and lights it up for a moment, so it is clear which
  one it refers to; the same happens when a search result opens a message.
  Right-click a message and choose Select,
  then forward several messages to up to five chats at once,
  or delete them together. Frequently forwarded messages go to
  one chat at a time, exactly like WhatsApp.
- **Disappearing-message timers.** Outgoing messages use the chat's known
  timer, including replies, attachments, edits, and forwards. Forwarded copies
  use the destination chat's timer. Received messages remain in the local archive
  after they expire on the phone.
  A clock badge on chat avatars shows enabled timers and follows changes from
  the phone. Changing the default timer for new chats leaves existing chats alone.
- **View attachments.** Vespera downloads files up to 64 MB automatically or
  on click. Each download streams to a temporary file with a size budget and
  a deadline, publishes only after validation, and never overwrites the last
  valid copy with a failed attempt. The media folder stays within 2 GB
  (`media_cache_bytes` in settings) by dropping the oldest files the archive
  no longer points at. Favourites, saved files, and a file that is the only
  remaining copy stay. A video soundtrack is decoded once and reused while
  that file is unchanged. Photos, stickers, GIFs, voice messages, audio, locations, contacts,
  polls, and link previews appear in the chat. Interactive messages show their header, body, footer, and button labels. A video link preview opens in the viewer: the clip plays when a video file is available, and otherwise the poster frame stays. Videos play in the app with
  their soundtrack: click one to watch it in the viewer, with play and pause
  (click the picture, or Space), a bar that jumps to the exact second with
  the keyframe still on screen while it catches up, a mute button with a
  level slider and the M key, and the true length on screen. A video shows
  the sender's poster until its first frame decodes. Dragging the progress
  bar holds playback and previews the destination with its time, then jumps
  once on release; Escape cancels the drag with no jump. Arrow keys step
  between files, or adjust the progress and volume sliders while one of
  them has focus. The soundtrack plays from the app itself, with no other
  program needed. Playback keeps the video's own size up to 1080p.
  On Windows the picture is decoded by Media Foundation, which can use the
  GPU and the codecs Windows already has, including HEVC when that codec is
  installed. OpenH264 plays the file when Media Foundation cannot open it.
  On macOS and Linux the in-process H.264 decoder still plays at that same
  size, and HEVC still opens in the default desktop app. Some files only play through an ffmpeg
  fallback: the app looks for an ffmpeg executable on PATH (installed and
  portable builds alike; nothing is bundled and nothing is downloaded
  automatically). Without it those files refuse with a message saying so,
  next to the same button that opens them in the default desktop app.
  Other documents open
  in their default desktop apps, and a PDF opens in the viewer built into this
  app, page by page: turn pages with the arrows or the buttons, type a page
  number in the top bar to jump to it, PageUp and PageDown walk ten pages at a
  time, and Home and End go to the first and last page. The page after the one
  on screen is rendered ahead, a page that was already drawn comes back from
  memory, and a protected or damaged file says so instead of waiting. A document
  reopens where its reader left it, turns a quarter with the Rotate button or
  the R key, and grows a strip of page previews on the left with the open page
  marked. Files say
  what they are: an image sent as a file opens in
  the viewer, a document shows its kind beside the size, and a program carries
  a warning with Save a copy in place of opening it. Clicking a program never
  runs it: it reveals the file selected in its folder instead. Every attachment menu has
  **Show info**, with the MIME type, size, pixels or length, the path on disk
  and the SHA-256 hash of the file. View-once photos and videos are labelled
  and left for your phone, and a download in progress shows a moving bar.
  Profile pictures and downloaded images support Windows drive paths and
  filenames with spaces or non-ASCII characters.
  Clicking a picture opens a full-window viewer: scroll to zoom, drag to move,
  the arrow keys walk the pictures of the chat, 0 or F fits it again, and Esc
  closes. The Copy button, or the right-click menu over the picture, puts it on
  the clipboard, and the bar can also save a copy or hand the file to the
  desktop. On Windows, hold a downloaded photo, video, document, or voice
  message waveform for a moment, then drag it onto a folder to copy it there.
  A card by the pointer shows the file while you hold it, and a quick flick
  stays a click. The message and the original file stay put, a
  missing or unfinished download does not start a drag, and Save a copy
  remains in the menu. A sticker opens
  bigger in a small dialog instead, with a Save button, and stays out of the
  order walked by the viewer.
  If an attachment has expired, Vespera asks your phone to upload it again.
- **Polls.** Use the checklist button beside the paperclip to create a poll with
  2–12 answers. Turn off **Allow multiple answers** for a single-choice poll.
  Click an answer in a poll to vote; click a selected answer again to remove
  it. Results and your selection are retained in the encrypted archive, including
  votes received through phone history. Visible polls automatically request earlier
  votes from your phone. If it is offline, results are labelled incomplete and the
  request retries with backoff; no refresh button or relinking is needed.
  Voting needs the original poll's key;
  if that key is missing, the message explains that voting is available on your
  phone. Creating polls in disappearing-message chats is not yet supported by
  the protocol library's poll API, so Vespera blocks it instead of ignoring the timer.
- **Emoji and sticker picker.** Four tabs: emoji, stickers, received, and favourites. The received tab lists stickers others sent you, newest first, deduplicated within itself across pages (a sticker kept elsewhere still appears there), with a "Show more received" footer.
  Search emoji, and save stickers with a right-click, which also marks a
  sticker as a favourite or clears the mark. The favourites tab holds every
  sticker you marked, wherever it came from, and a sticker in a chat can be
  marked from its own right-click menu. The sticker tab lists saved stickers
  first, then imported packs, then the phone recent list and the stickers you
  sent; a sticker that only passed through a chat is never offered. Packs are
  filed under the hash of each sticker, so the same picture is never listed
  twice, and the grid draws a small preview built in the background instead of
  decoding the full file for every tile. Clicking a sticker previews it
  for confirmation before sending, and the picker reopens on the last used
  tab. A sticker you send is on screen as soon as you confirm it: the copy is
  filed locally first and the upload fills in behind it, and a sticker sent
  while a reply was open goes as that reply. Tiles that fail to load retry with
  a fresh download instead of
  sticking on an error, and a page fills in a few tiles at a time instead of
  asking the server for everything at once. Emoji autocomplete and picker
  search select their first match; use the arrow keys and Enter to choose it.
 A sticker that is saved or marked as a favourite is not offered again under
 the phone recents, and clicking one in a chat shows it bigger with a Save
 button instead of the full viewer.
  Search the sticker tab by emoji, word, or pack name. Favourites are kept by
  content hash, so the same picture marked from a pack, the recents, or the
  saved stickers stays one favourite, and favourite changes sync toward the
  phone with retries. A sticker pack shared in a chat opens for preview from
  its card and can be kept as a new pack.
- **Sticker packs.** Open a .wastickers or zip file as a new pack, from the
  button at the top of the sticker tab. Animated packs remain animated. Packs
  are stored as WebP files on your computer, one folder per pack.
- **Where the ideas came from.** [STICKERS.md](STICKERS.md) lists the clients
  this one compared itself against (Signal Desktop, WhatsApp Web, Baileys,
  whatsmeow, ZapZap, SumatraPDF), what was taken from each, and which
  decisions are local to this fork.
- **Consistent names.** Prefer names from your address book, then the profile
  name people chose (shown as `~Name`), across chats, replies, mentions, and
  notifications. Chats without either show a readable number, including the
  Brazilian shape `+55 75 9 9539 9345`.
- **Groups.** See members, sender names, and sender pictures. Announcement
  groups are read-only for non-admins. Rename a group or change its photo from
  the group information dialog; changes confirm only after the server answers.
  A group invite shows the group name and its caption. Joining asks for
  confirmation first, then tries the invite code once. The code is not written
  to the log. A failed attempt is not retried on its own.
- **Presence.** See online, last-seen, and typing status, and send your typing
  status.
- **Idle rendering.** History-sync progress updates when data arrives. Animated
  stickers and GIFs play only while their message or picker tile is visible.
- **Sync recovery.** A conflicting app-state collection is recovered through
  whatsapp-rust, including requesting a fresh snapshot from the paired phone
  when validation fails. Private read-state updates run one at a time. Failures
  pause the whole queue with backoff from 30 seconds to 15 minutes; pending reads
  remain saved and resume automatically. New messages can still arrive.
- **Runs in the background.** Closing the window keeps Vespera linked in the
  system tray. Reopen it from the tray or by launching it again. Quit from the
  tray or with `Ctrl+Q`, or disable this behavior in Settings.
- **Desktop notifications.** Get notifications with the chat picture when you
  are away from the open chat. Muted chats do not notify you. Windows notifications
  identify Vespera as the sender and show chat pictures as small circular icons;
  installed and portable builds register this identity in the current user's registry.
  Clicking a Windows notification opens its chat and anchors on the exact
  notified message. A message notification also has a Reply field and Mark as
  read: the reply uses the same send as the composer and does not open the
  window, and Mark as read clears that chat. On Linux,
  clicking a notification opens the chat, and reading the chat here or on another
  device dismisses its outstanding notifications. On macOS, notifications use
  the installed Vespera application's identity without an application chooser;
  unregistered development builds skip notifications if that identity is unavailable.
- **Taskbar badge (Windows).** The taskbar button shows your unread count as a numeric overlay (99+ above 99), ignoring archived and muted chats like notifications do. It clears at zero without polling and returns after window recreation and Explorer restarts.
- **Update notices.** Vespera checks GitHub once a day and shows a download
  link when a newer release is available. You can turn this off in Settings.
- **Themes.** Light, dark, follow the system, or a local JSON palette. Native
  Linux packages can follow Omarchy colors without restarting the app. Zoom with
  Ctrl+plus and Ctrl+minus.
- **Copy text.** Select part of a message or copy across messages in
  WhatsApp's `[time, date] Name:` format. Contact names and numbers are also
  selectable.
- **Keyboard shortcuts.** `Ctrl+K` searches, `Alt+↑/↓` switches chats and
  keeps the active chat visible in the list, `Esc` cancels the current action,
  `Ctrl+L` focuses the message input, and `Ctrl+/` lists all shortcuts (use
  Command instead of Ctrl on macOS). The × at the left of the shortcut hints
  hides the bar; restore it with **Show shortcut hints** in Settings.
- **Local storage.** Messages, contacts and sticker metadata are stored in a
  SQLCipher-encrypted archive, unlocked automatically through your OS keyring.
  Existing plaintext archives are migrated on first use. Attachments remain
  ordinary files in the cache directory, filed under the hash of their content
  when they are stickers. A sweep once per run reclaims attachment files no
  message points at any more, and what it freed goes to the log. Unlinking
  deletes both and removes this device from
  your phone.

## What it does not do yet

- Reply to a message with an attachment.
- Keep a view-once photo, video, or voice message. When WhatsApp includes
  the file, it opens once on this computer and is then deleted. When it does
  not, the message stays on the phone.
- Place or answer a voice or video call. An incoming call notifies you and
  shows "Answer on your phone". Decline is offered once, after confirmation.
  A missed call appears in the chat and on the Calls screen. Communities and
  newsletters.

## Installing

For Vespera, download the fork build from GitHub Releases. The upstream Homebrew and AUR recipes belong to the original ZapFast project and are not published by this fork. The fork's own Arch and Homebrew recipes are templates for reference only (see [PACKAGING.md](PACKAGING.md)), so install from the release files below.

Vespera was previously called ZapExt. Version 0.13.0 introduces the new
package and executable names (`vespera`).

The current release is **1.0.106** on the
[Vespera releases page](https://github.com/vitorhubdev/Vespera/releases).
Every release ships a `checksums.txt` alongside the files.

| System | File |
| --- | --- |
| Linux x86_64 (most desktop PCs) | `vespera-v1.0.106-x86_64-unknown-linux-gnu.tar.gz`, with the desktop file and icon in `packaging/` |
| Linux arm64 | `vespera-v1.0.106-aarch64-unknown-linux-gnu.tar.gz`, with the desktop file and icon in `packaging/` |
| Linux Flatpak (sandboxed) | `vespera-v1.0.106-x86_64.flatpak`; see Flatpak below |
| Windows x64 (most PCs) | installer `vespera-v1.0.106-x86_64-pc-windows-msvc-setup.exe`, portable `Vespera-v1.0.106-windows-x64-portable.exe`, or `vespera-v1.0.106-x86_64-pc-windows-msvc.zip` |
| Windows arm64 | installer `vespera-v1.0.106-aarch64-pc-windows-msvc-setup.exe`, portable `Vespera-v1.0.106-windows-arm64-portable.exe`, or `vespera-v1.0.106-aarch64-pc-windows-msvc.zip` |
| macOS, Apple Silicon and Intel | `vespera-v1.0.106-macos-universal.dmg` |

The portable executables need no installation: keep `vespera-portable.txt`
beside the `.exe` so the updater recognizes the install. The `.zip` files
carry the same portable files in an archive.

On macOS, the rounded Dock icon matches the app bundle. Native menus provide
Settings, editing, search, view controls, and window commands. The traffic
lights share the chat header, leaving more room for conversations in a normal
window. Settings is also available with `⌘,`.

The macOS release process always validates the universal app and its entitlements.
When Apple Developer credentials are configured it also signs with Developer ID,
notarizes the DMG, and validates the stapled ticket. Without those credentials,
the release uses an ad-hoc signature and skips only the Apple notarization checks. Open the DMG and drag **Vespera** to Applications.
When upgrading from Vespera on macOS, quit the old app and remove its
application bundle after installing Vespera.

Releases before 0.13.0 keep their original Vespera filenames.

### Flatpak

Flatpak packaging lives in `packaging/flatpak/`, following Spotifast's source
manifest and release-bundle setup. Future releases will attach an x86_64
`.flatpak` bundle; install a downloaded bundle with `flatpak install --user FILE`
and run `flatpak run rocks.vespera.Vespera`. Flathub publication is pending;
Vespera is not yet listed there. See [PACKAGING.md](PACKAGING.md) for local builds
and preparing a Flathub submission. File selection uses desktop portals;
the sandbox has no general access to your home directory.

### Archive encryption

The archive key is a random 256-bit secret in Secret Service on Linux, Keychain
on macOS, or Windows Credential Manager. Linux needs a working Secret Service
provider (for example GNOME Keyring or KeePassXC with Secret Service enabled).
If the keyring is locked or unavailable, unlock it and click Retry; Vespera keeps
its archive intact and waits before connecting. It never saves a replacement
plaintext archive. Back up both the archive and its OS keyring key: copying only
`archive.db` to another computer is insufficient.

Only `archive.db` and its SQLite journal/WAL are encrypted. Device credentials in
`session.db`, downloaded media, profile pictures, saved sticker files and settings
remain ordinary files. Use full-disk encryption for those files, swap, backups and
remnants of the old plaintext archive. Migration removes the original only after
verifying its encrypted copy; deletion cannot guarantee erasure from SSDs or
snapshots. Keyring unlocking also does not protect against software running as you
while your login is unlocked.

### From source

Vespera needs Rust, a C/C++ toolchain, CMake and Perl (for bundled OpenSSL). `rust-toolchain.toml` pins the exact version. On Linux,
it also needs GUI development packages:

```sh
# Debian and Ubuntu
sudo apt install libxkbcommon-dev libwayland-dev libgl1-mesa-dev libasound2-dev cmake perl
# Arch
sudo pacman -S libxkbcommon wayland mesa alsa-lib cmake perl
```

Then:

```sh
cargo install --path .
vespera
```

The desktop file and icon are in `packaging/`. The window, tray, executable,
bundle, and desktop icons are generated from the master logo artwork with
`python scripts/make-icons.py --source logo.png` (needs Pillow, NumPy, and
SciPy); the script writes `assets/vespera.png`, `packaging/icons/vespera.svg`,
`packaging/macos/icon-1024.png`, and `packaging/windows/vespera.ico` so every
surface shows the same mark.

`whatsapp-rust` is pinned to a Git commit because version 0.7.0 on crates.io
enables a `simd` feature that needs nightly Rust. The pinned commit builds on
stable Rust and includes the upstream fixes for missing app-state snapshots and
conflicts that make no progress. Vespera does not reset your session to recover
a collection.

## Using it

On first start, scan the QR code from WhatsApp under **Linked devices**,
**Link a device**. To link without the camera, click **Link with phone number
instead**, enter your number with its country code, then enter the shown code
on your phone.

WhatsApp then sends your recent history. This can take a few minutes. A banner
shows the progress. New messages arrive live, and your phone does not need to
stay on the same network.

If your network answers DNS with an IPv6 address that cannot be reached, the
connection moves on to IPv4 by itself instead of waiting for a timeout. This is
separate from the two WhatsApp endpoints Vespera races: the endpoint race picks
the port, and the address fallback picks the family. Both are needed, and
neither replaces the other.

Right-click a chat or message to open its menu. Open Settings from the gear or
with `Ctrl+,`. Use the pencil to message a new number or save a contact. You
can also open a group member's contact card. Saved names sync through WhatsApp
to your phone and linked devices. A contact someone shares with you has a
Message button that opens a chat with that number without saving it to your
contacts; a card with several numbers lets you pick one.

## Files

| What | Linux | Notes |
| --- | --- | --- |
| Settings | `~/.config/vespera/settings.json` | JSON, safe to edit |
| Device keys | `~/.local/state/vespera/session.db` | Owned by whatsapp-rust; deleting it unlinks |
| Messages | `~/.local/state/vespera/archive.db` | SQLCipher-encrypted SQLite, unlocked by the OS keyring; raw messages retain attachment keys |
| Attachments, avatars | `~/.cache/vespera/` | Safe to delete |
| Saved stickers and packs | `~/.local/state/vespera/stickers/` | Plain WebP files; each pack is a folder |
| Log of the last run | `~/.local/state/vespera/vespera.log` | `--verbose` for more |

macOS and Windows use the standard platform directories selected by the
`directories` crate. On first start, Vespera moves settings, the linked session,
message archive, saved stickers, caches, and window state from `vespera`
(or the earlier `vespera`) paths. Existing Vespera directories take
precedence and are never overwritten. Quit Vespera before starting Vespera;
if an older copy is still running, the new launch brings its window forward.
Your phone may keep showing the old linked-device name until you link again.

On Linux and macOS, Vespera restricts its configuration, state, and cache
directories to the current user (`0700`), including existing installations.
Startup stops if those directories cannot be created or secured, before opening
logs or databases. Windows uses the permissions inherited from your user profile.

### Local themes

**Settings → Appearance → Theme** uses the same picker as Spotifast, with
Follow system, Light, Dark, and its Catppuccin, Catppuccin Latte, Nord, Ristretto,
and Tokyo Night palettes. Choose **Open themes folder** below the picker to add
JSON palettes beside `settings.json`. **Settings → Appearance → Language** offers Auto, English, Portuguese (Brazil), Spanish, and Simplified Chinese: Auto follows the desktop locale (Portuguese for `pt-*`, Spanish for `es-*`, Simplified for Hans locales, English otherwise including Traditional). Each language is one JSON file under `locales/`, embedded in the binary; adding a language means adding the file plus one variant in `Language` and one arm in `Resolved::source` (the module docs in `src/i18n.rs` list the steps, and a test fails if a file is left unregistered). The interface text, including the phone-linking screen, the dialogs and the toasts, goes through that catalog, and a key missing from a language falls back to English. The text of incoming messages, the status cards and the call notices carry their own per-language phrases and stay in English for the languages that do not have them yet. A local file with a bundled palette's name
overrides it. For example:

```json
{"base":"dark","colors":{"accent":"#89b4fa","bubble_out":"#293954"}}
```

Unspecified colors inherit the light or dark base. Spotifast palettes also work:
chat backgrounds, bubbles, and links derive from their interface colors when not
specified. Color names match `Palette`
in `src/theme.rs`; use `#RRGGBB` or `#RRGGBBAA`. The last accepted palette is cached
in settings, so a missing or damaged theme file does not reset your appearance.
Linux watches the themes folder for changes without periodic repaints. On other
platforms, use `vespera reload-themes` after editing. The command also works while
the window is closed and never launches a stopped app.

On Omarchy, **Follow system** and **Omarchy** read the active desktop palette and
follow its changes in native, portable, and source builds, even without installed
hooks. Other desktops keep their normal light/dark system preference. Native
packages additionally register a missing per-user template and theme hook on
first launch; existing user files are preserved. Flatpak uses the desktop's
light/dark preference and does not read host theme files or install desktop hooks.

### Updating Vespera

Vespera uses the fork's GitHub Releases API at
`https://api.github.com/repos/vitorhubdev/Vespera/releases/latest`.
It checks once a day when **Check for updates** is enabled. **Update channel** selects Stable (finished releases, the default) or Testing (also release candidates, never older builds). The current fork
version has one canonical source in the repository root: [`VERSION`](VERSION).
Release tags are validated against that file before binaries are built.
Click **Update** in the banner to download and verify a newer release, then
**Restart to update** when convenient. **Download updates automatically** is
optional and off by default; it downloads in the background and still waits for
you to restart. Downloads contact GitHub's API and release-asset hosts and are
checked against the release's SHA-256 checksums. The updater keeps a backup and
restores it if the updated app cannot start.

The in-app updater supports marked portable downloads, the Windows installer,
and the macOS app in Applications. Keep `vespera-portable.txt` beside a portable
executable. AUR, DEB, RPM, Flatpak, Cargo and Homebrew installations use their
package manager. Older portable downloads without the marker need one manual
upgrade. No account or additional service is needed.

## Reporting problems

Report bugs and request features on the
[issue tracker](https://github.com/vitorhubdev/Vespera/issues).
Include the Vespera version (Settings, or `vespera --version`), what you
did, what you expected, and what happened instead. For crashes or failed
updates, attach the log of the last run (`vespera.log` next to the files
listed under Files above).

## Developing

```sh
cargo run --features demo -- --demo            # sample chats, no connection
cargo run --features demo -- --demo-page login # or settings, pair, info, light, …
cargo run --features demo -- --demo-shot shot.png --demo-page chat,light
cargo run --features demo -- --demo-tour      # Space starts/replays a 35-second tour
cargo test --all-features                      # includes a headless layout of every screen
cargo clippy --all-targets --all-features -- -D warnings
```

`AGENTS.md` describes the architecture and the rules for changes.

### Recording a demo

The `demo` feature uses offline sample chats in a fresh temporary directory.
It does not open your linked account, read your message archive, connect to
WhatsApp, or register a tray icon. You can run it alongside your regular app.

```sh
cargo build --locked --features demo
./target/debug/vespera --demo-tour --demo-size 1280x800
```

The **Vespera Demo** window waits for **Space**. The 35-second tour starts with
search, switches chats with keyboard shortcuts, scrolls, right-clicks a message
and selects Reply, types quickly, completes emoji and mentions, sends a still
sticker from the picker, opens group information and the shortcut list,
and changes themes through Settings. It uses the normal mouse and keyboard handlers;
a local responder handles outgoing messages with no WhatsApp connection.
The still stickers are rendered from the bundled Noto emoji font. The tour makes
no sound and holds its final frame. Space rebuilds the sample and replays.
For an automatic start, add `--demo-tour-delay 5000` (milliseconds).
Use `--demo` instead of `--demo-tour` to explore the sample chats yourself.
For deterministic theme screenshots, `--demo-page settings,omarchy` and
`--demo-page settings,omarchy-light` preview following dark and light Omarchy
palettes without changing the desktop theme.

On Omarchy, run `omarchy screenrecord`, select the demo window, then press Space
in Vespera. Recording has no audio unless you explicitly enable desktop or
microphone audio. Stop with `omarchy screenrecord --stop-recording` after the
tour finishes. The default capture records a fixed rectangle, so keep the demo
window visible and stationary until recording stops.

To annotate the video with a visible pointer, click rings, and outlined shortcut
labels, add `--demo-tour-events tour.json` when launching the tour. After
recording, run:

```sh
python3 scripts/render-demo.py recording.mp4 tour.json launch.mp4 --start 0.8
```

Set `--start` to the recording time (in seconds) when you pressed Space. The
export trims the setup footage, adds a caption band below the app, and produces
a silent H.264 MP4. It requires `ffmpeg` with libass support and `ffprobe`.
These annotations are added during video export, not drawn by the app. The
trace contains only pointer coordinates and shortcut labels, not typed text.

## Disclaimer

Vespera is an unofficial client and is not affiliated with WhatsApp or
Meta. Using an unofficial client may be against WhatsApp's terms of service
and could get an account suspended. Use it at your own risk.

## Packaging maintenance

Release packaging uses the [native-packages](https://rubygems.org/gems/native-packages) gem. macOS release builds automatically sign and notarize when the Apple CI credentials are configured. `native-packages.yaml` declares packages and downstream repositories; native recipes and installation assets live in `packaging/`; see [PACKAGING.md](PACKAGING.md) for local commands and CI behavior.

## Credits

Vespera modifications and fork releases are maintained by
[Vitor (`@vitorhubdev`)](https://github.com/vitorhubdev).

The original project is [ZapFast](https://github.com/crmne/zapfast), created
and developed by its original authors and contributors. Vespera is a derivative
MIT-licensed mod/fork. The original license and copyright notice remain in
[`LICENSE`](LICENSE).

## License

MIT. Inter and Noto Color Emoji are under the SIL Open Font License; the icons
are from [Lucide](https://lucide.dev) (ISC).
