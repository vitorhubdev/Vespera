//! VLC, loaded from the user's install when it is there.
//!
//! libvlc is LGPL. This binary does not ship any VLC file: it opens the
//! library the user already installed. A missing or unwilling library is
//! not an error; playback stays on the included decoder.

use std::ffi::{CString, c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// Which engine should open a file.
pub fn engine_for(library_present: bool, in_process: bool) -> &'static str {
    if library_present {
        "vlc"
    } else if in_process {
        "software"
    } else {
        "external"
    }
}

pub fn quality_hint(locale: &str) -> &'static str {
    match crate::i18n::message_locale_tag(locale) {
        "pt" => "Instale o VLC para vídeo em qualidade máxima",
        "es" => "Instala VLC para ver el vídeo con la máxima calidad",
        _ => "Install VLC for full video quality",
    }
}

pub fn license_title(locale: &str) -> &'static str {
    match crate::i18n::message_locale_tag(locale) {
        "pt" => "Licenças",
        "es" => "Licencias",
        _ => "Licenses",
    }
}

pub fn license_notice(locale: &str) -> &'static str {
    match crate::i18n::message_locale_tag(locale) {
        "pt" => {
            "O VLC (libvlc, LGPL) não vem neste programa. Se estiver instalado, o Vespera carrega a biblioteca do seu computador para reproduzir vídeo. Sem o VLC, a reprodução continua pelo decodificador incluído."
        }
        "es" => {
            "VLC (libvlc, LGPL) no viene en este programa. Si está instalado, Vespera carga la biblioteca de su equipo para reproducir vídeo. Sin VLC, la reproducción sigue con el decodificador incluido."
        }
        _ => {
            "VLC (libvlc, LGPL) is not part of this program. When it is installed, Vespera loads that library from your computer to play video. Without VLC, playback stays on the included decoder."
        }
    }
}

/// The user's libvlc, found at most once per process.
pub fn available() -> bool {
    library_path().is_some()
}

pub fn library_path() -> Option<&'static Path> {
    path_slot().get().and_then(|path| path.as_deref())
}

fn path_slot() -> &'static OnceLock<Option<PathBuf>> {
    static PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
    PATH.get_or_init(find_library);
    &PATH
}

fn find_library() -> Option<PathBuf> {
    find_library_platform().filter(|path| path.is_file())
}

#[cfg(windows)]
fn find_library_platform() -> Option<PathBuf> {
    let install = registry_install_dir()?;
    let dll = install.join("libvlc.dll");
    dll.is_file().then_some(dll)
}

#[cfg(windows)]
fn registry_install_dir() -> Option<PathBuf> {
    for key in [
        r"HKLM\SOFTWARE\VideoLAN\VLC",
        r"HKLM\SOFTWARE\WOW6432Node\VideoLAN\VLC",
    ] {
        if let Some(dir) = registry_value(key, "InstallDir") {
            return Some(dir);
        }
    }
    None
}

#[cfg(windows)]
fn registry_value(key: &str, name: &str) -> Option<PathBuf> {
    let output = std::process::Command::new("reg")
        .args(["query", key, "/v", name])
        .creation_flags(0x0800_0000)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        let Some((_, rest)) = line.split_once("REG_SZ") else {
            continue;
        };
        let dir = rest.trim();
        if !dir.is_empty() {
            return Some(PathBuf::from(dir));
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn find_library_platform() -> Option<PathBuf> {
    let path = PathBuf::from("/Applications/VLC.app/Contents/MacOS/lib/libvlc.dylib");
    path.is_file().then_some(path)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn find_library_platform() -> Option<PathBuf> {
    [
        "/usr/lib/libvlc.so.5",
        "/usr/lib/x86_64-linux-gnu/libvlc.so.5",
        "/usr/lib64/libvlc.so.5",
        "/usr/lib/aarch64-linux-gnu/libvlc.so.5",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|path| path.is_file())
}

/// One picture from libvlc, already copied out of the callback buffer.
pub enum Picture {
    Frame {
        rgba: Vec<u8>,
        width: u32,
        height: u32,
        pts: Duration,
    },
    End {
        produced: u64,
        dropped: u64,
    },
    Failed {
        reason: String,
        produced: u64,
    },
}

struct Queued {
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    pts: Duration,
}

/// Frames leave the VLC callback here. One player runs at a time, and a
/// full queue drops the oldest picture instead of blocking that thread.
static FRAMES: Mutex<Vec<Queued>> = Mutex::new(Vec::new());
static DROPPED: AtomicU64 = AtomicU64::new(0);

fn publish(frame: Queued) {
    let Ok(mut frames) = FRAMES.lock() else {
        DROPPED.fetch_add(1, Ordering::SeqCst);
        return;
    };
    if frames.len() >= 4 {
        frames.remove(0);
        DROPPED.fetch_add(1, Ordering::SeqCst);
    }
    frames.push(frame);
}

fn take_frames() -> Vec<Queued> {
    match FRAMES.lock() {
        Ok(mut frames) => std::mem::take(&mut *frames),
        Err(_) => Vec::new(),
    }
}

struct State {
    width: u32,
    height: u32,
    buffer: Vec<u8>,
    index: u64,
    fps_milli: u64,
    alive: Arc<AtomicBool>,
}

/// Plays `path` on a background thread. `sink` runs on that thread, never
/// on the interface.
pub fn start<F>(path: PathBuf, width: u32, height: u32, alive: Arc<AtomicBool>, sink: F)
where
    F: Fn(Picture) + Send + 'static,
{
    let width = width.max(2) & !1;
    let height = height.max(2) & !1;
    let _ = std::thread::Builder::new()
        .name("video-vlc".into())
        .spawn(move || play(path, width, height, alive, sink));
}

fn play<F>(path: PathBuf, width: u32, height: u32, alive: Arc<AtomicBool>, sink: F)
where
    F: Fn(Picture),
{
    let Some(library) = library_path().map(Path::to_path_buf) else {
        sink(Picture::Failed {
            reason: "VLC is not installed".to_owned(),
            produced: 0,
        });
        return;
    };
    let Ok(loaded) = (unsafe { libloading::Library::new(&library) }) else {
        sink(Picture::Failed {
            reason: "VLC could not be loaded".to_owned(),
            produced: 0,
        });
        return;
    };
    match session(&loaded, &library, &path, width, height, &alive, &sink) {
        Ok(()) => {}
        Err(reason) => sink(Picture::Failed {
            reason,
            produced: 0,
        }),
    }
}

fn session<F>(
    library: &libloading::Library,
    library_path: &Path,
    file: &Path,
    width: u32,
    height: u32,
    alive: &Arc<AtomicBool>,
    sink: &F,
) -> Result<(), String>
where
    F: Fn(Picture),
{
    unsafe {
        type New = unsafe extern "C" fn(c_int, *const *const c_char) -> *mut c_void;
        type Release = unsafe extern "C" fn(*mut c_void);
        type MediaNew = unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void;
        type MediaOption = unsafe extern "C" fn(*mut c_void, *const c_char);
        type PlayerNew = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
        type SetCallbacks = unsafe extern "C" fn(
            *mut c_void,
            LockCb,
            Option<UnlockCb>,
            Option<DisplayCb>,
            *mut c_void,
        );
        type SetFormat = unsafe extern "C" fn(*mut c_void, *const c_char, u32, u32, u32);
        type Play = unsafe extern "C" fn(*mut c_void) -> c_int;
        type Stop = unsafe extern "C" fn(*mut c_void);
        type GetFps = unsafe extern "C" fn(*mut c_void) -> f32;
        type IsPlaying = unsafe extern "C" fn(*mut c_void) -> c_int;

        let new: New = *symbol(library, b"libvlc_new\0")?;
        let release: Release = *symbol(library, b"libvlc_release\0")?;
        let media_new: MediaNew = *symbol(library, b"libvlc_media_new_location\0")?;
        let media_option: MediaOption = *symbol(library, b"libvlc_media_add_option\0")?;
        let player_new: PlayerNew = *symbol(library, b"libvlc_media_player_new_from_media\0")?;
        let media_release: Release = *symbol(library, b"libvlc_media_release\0")?;
        let set_callbacks: SetCallbacks = *symbol(library, b"libvlc_video_set_callbacks\0")?;
        let set_format: SetFormat = *symbol(library, b"libvlc_video_set_format\0")?;
        let play: Play = *symbol(library, b"libvlc_media_player_play\0")?;
        let stop: Stop = *symbol(library, b"libvlc_media_player_stop\0")?;
        let player_release: Release = *symbol(library, b"libvlc_media_player_release\0")?;
        let get_fps: GetFps = *symbol(library, b"libvlc_media_player_get_fps\0")?;
        let is_playing: IsPlaying = *symbol(library, b"libvlc_media_player_is_playing\0")?;

        let no_audio = CString::new("--no-audio").map_err(|_| "bad vlc argument".to_owned())?;
        let plugin = plugin_argument(library_path);
        let mut args = Vec::new();
        if let Some(plugin) = plugin.as_ref() {
            args.push(plugin.as_ptr());
        }
        args.push(no_audio.as_ptr());
        let instance = new(args.len() as c_int, args.as_ptr());
        if instance.is_null() {
            return Err("VLC did not start".to_owned());
        }
        let location = match CString::new(file_location(file)) {
            Ok(location) => location,
            Err(_) => {
                release(instance);
                return Err("the video path could not be passed to VLC".to_owned());
            }
        };
        let media = media_new(instance, location.as_ptr());
        if media.is_null() {
            release(instance);
            return Err("VLC could not open the file".to_owned());
        }
        if let Ok(option) = CString::new(":no-audio") {
            media_option(media, option.as_ptr());
        }
        let player = player_new(media);
        media_release(media);
        if player.is_null() {
            release(instance);
            return Err("VLC could not open a player".to_owned());
        }

        DROPPED.store(0, Ordering::SeqCst);
        let _ = take_frames();
        let state = Box::into_raw(Box::new(Mutex::new(State {
            width,
            height,
            buffer: vec![0u8; width as usize * height as usize * 4],
            index: 0,
            fps_milli: 30_000,
            alive: Arc::clone(alive),
        })));
        let chroma = CString::new("RV32").map_err(|_| "RV32".to_owned())?;
        set_callbacks(player, lock, Some(unlock), Some(display), state.cast());
        set_format(player, chroma.as_ptr(), width, height, width * 4);
        if play(player) != 0 {
            stop(player);
            player_release(player);
            release(instance);
            drop(Box::from_raw(state));
            return Err("VLC could not play the file".to_owned());
        }

        let mut produced = 0u64;
        let mut saw_fps = false;
        while alive.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(20));
            if !saw_fps {
                let fps = get_fps(player);
                if fps > 1.0
                    && let Ok(mut state) = (*state).lock()
                {
                    state.fps_milli = (fps * 1000.0) as u64;
                    saw_fps = true;
                }
            }
            for frame in take_frames() {
                produced = produced.saturating_add(1);
                sink(Picture::Frame {
                    rgba: frame.rgba,
                    width: frame.width,
                    height: frame.height,
                    pts: frame.pts,
                });
            }
            if is_playing(player) == 0 && produced > 0 {
                break;
            }
        }
        stop(player);
        player_release(player);
        release(instance);
        let state = Box::from_raw(state);
        let produced = state.lock().map(|state| state.index).unwrap_or(produced);
        sink(Picture::End {
            produced,
            dropped: DROPPED.load(Ordering::SeqCst),
        });
        Ok(())
    }
}

fn symbol<'a, T>(
    library: &'a libloading::Library,
    name: &[u8],
) -> Result<libloading::Symbol<'a, T>, String> {
    unsafe { library.get(name) }.map_err(|_| format!("VLC is missing {}", label(name)))
}

fn label(name: &[u8]) -> String {
    name.iter()
        .take_while(|byte| **byte != 0)
        .map(|byte| *byte as char)
        .collect()
}

type LockCb = unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> *mut c_void;
type UnlockCb = unsafe extern "C" fn(*mut c_void, *mut c_void, *const *mut c_void);
type DisplayCb = unsafe extern "C" fn(*mut c_void, *mut c_void);

unsafe extern "C" fn lock(opaque: *mut c_void, planes: *mut *mut c_void) -> *mut c_void {
    unsafe {
        let state = &*(opaque as *const Mutex<State>);
        if let Ok(mut state) = state.lock()
            && !planes.is_null()
            && !state.buffer.is_empty()
        {
            *planes = state.buffer.as_mut_ptr().cast();
        }
    }
    opaque
}

unsafe extern "C" fn unlock(
    _opaque: *mut c_void,
    _picture: *mut c_void,
    _planes: *const *mut c_void,
) {
}

unsafe extern "C" fn display(opaque: *mut c_void, _picture: *mut c_void) {
    let state = unsafe { &*(opaque as *const Mutex<State>) };
    let Ok(mut state) = state.lock() else {
        return;
    };
    if !state.alive.load(Ordering::SeqCst) {
        return;
    }
    let rgba = state.buffer.clone();
    let width = state.width;
    let height = state.height;
    let index = state.index;
    state.index = state.index.saturating_add(1);
    let fps = state.fps_milli.max(1) as f64 / 1000.0;
    publish(Queued {
        rgba,
        width,
        height,
        pts: Duration::from_secs_f64(index as f64 / fps),
    });
}

fn plugin_argument(library: &Path) -> Option<CString> {
    let plugins = library.parent()?.join("plugins");
    if !plugins.is_dir() {
        return None;
    }
    CString::new(format!("--plugin-path={}", plugins.display())).ok()
}

fn file_location(path: &Path) -> String {
    let raw = path.to_string_lossy().replace('\\', "/");
    let prefixed = if raw.starts_with('/') {
        raw
    } else {
        format!("/{raw}")
    };
    format!("file://{prefixed}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_engine_is_vlc_only_when_the_library_is_present() {
        assert_eq!(engine_for(true, true), "vlc");
        assert_eq!(engine_for(true, false), "vlc");
        assert_eq!(engine_for(false, true), "software");
        assert_eq!(engine_for(false, false), "external");
    }

    #[test]
    fn the_hint_and_the_license_name_vlc() {
        assert_eq!(
            quality_hint("pt-BR"),
            "Instale o VLC para vídeo em qualidade máxima"
        );
        assert!(quality_hint("es").contains("VLC"));
        assert_eq!(license_title("pt"), "Licenças");
        assert!(license_notice("en").contains("LGPL"));
        assert!(license_notice("pt").contains("LGPL"));
    }

    #[test]
    fn a_missing_library_is_not_an_error() {
        let _ = available();
        let _ = library_path();
    }
}
