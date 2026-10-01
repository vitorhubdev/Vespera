//! The files the app keeps for itself, and what is worth keeping.
//!
//! Attachments, phone stickers, sticker previews, and profile pictures live in
//! folders the app owns: every one of them can be fetched or built again.
//! Saved stickers and imported packs are the user's own files and are never
//! touched from here.
//!
//! The archive is the list of what still matters: the file each message points
//! at. Anything else in the attachment folder is left over from a failed
//! write, an interrupted download, or a message that is gone, and that is what
//! a sweep reclaims.

use std::path::Path;
use std::time::Duration;

/// How long a file must have sat on disk before a sweep may remove it.
///
/// A download writes its file and only then points its message at it, so this
/// wait keeps a sweep from pulling a file out from under a reader that is
/// about to keep the path. Younger files are never touched.
pub const SETTLE: Duration = Duration::from_secs(300);

/// Files and bytes held by one folder.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub files: u64,
    pub bytes: u64,
}

impl Usage {
    /// Adds another folder's numbers to this one.
    pub fn add(&mut self, other: Usage) {
        self.files += other.files;
        self.bytes += other.bytes;
    }

    /// Whether the folder holds nothing to report.
    pub fn is_empty(&self) -> bool {
        self.files == 0
    }
}

impl std::fmt::Display for Usage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} in {} files", size(self.bytes), self.files)
    }
}

/// A byte count the way a person reads it.
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// Measures the files directly inside one folder.
pub fn usage(dir: &Path) -> Usage {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Usage::default();
    };
    let mut total = Usage::default();
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.is_file() {
            total.files += 1;
            total.bytes += metadata.len();
        }
    }
    total
}

/// Deletes the files of one folder that the keep list rejects.
///
/// Returns what was reclaimed. A file written within SETTLE is always left
/// alone.
pub fn sweep(dir: &Path, keep: &dyn Fn(&Path) -> bool) -> Usage {
    prune(dir, &|path, age| age >= SETTLE && !keep(path))
}

/// Default media-cache ceiling: two gigabytes.
pub const MEDIA_CAP: u64 = 2 * 1024 * 1024 * 1024;

/// One file considered by the size cap, oldest first.
struct CachedFile {
    path: std::path::PathBuf,
    len: u64,
    used: std::time::SystemTime,
}

fn walk_files(dir: &Path, files: &mut Vec<CachedFile>, total: &mut u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for path in entries.flatten().map(|entry| entry.path()) {
        if path.is_dir() {
            walk_files(&path, files, total);
            continue;
        }
        if !path.is_file() {
            continue;
        }
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        let len = metadata.len();
        *total = total.saturating_add(len);
        files.push(CachedFile {
            path,
            len,
            used: metadata
                .modified()
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
        });
    }
}

/// Deletes the oldest files until the folder, including nested folders, is
/// within `cap` bytes.
///
/// `keep` is never removed. Callers put favourites, saved files, soundtrack
/// sidecars, and copies that cannot be downloaded again in that set.
/// Files younger than [`SETTLE`] stay, so a download in progress is left alone.
/// Returns what was removed.
pub fn trim_to(dir: &Path, cap: u64, keep: &dyn Fn(&Path) -> bool) -> Usage {
    let mut files = Vec::new();
    let mut total = 0u64;
    walk_files(dir, &mut files, &mut total);
    if total <= cap {
        return Usage::default();
    }
    files.sort_by_key(|item| item.used);
    let mut freed = Usage::default();
    for item in files {
        if total <= cap {
            break;
        }
        if keep(&item.path) {
            continue;
        }
        let age = item.used.elapsed().unwrap_or_default();
        if age < SETTLE {
            continue;
        }
        if std::fs::remove_file(&item.path).is_ok() {
            freed.files += 1;
            freed.bytes += item.len;
            total = total.saturating_sub(item.len);
        }
    }
    freed
}

/// Deletes the files of one folder last written more than the given age ago.
pub fn expire(dir: &Path, age: Duration) -> Usage {
    prune(dir, &|_, file_age| file_age >= age)
}

/// Removes the files of one folder that the rule accepts.
fn prune(dir: &Path, remove: &dyn Fn(&Path, Duration) -> bool) -> Usage {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Usage::default();
    };
    let mut freed = Usage::default();
    for path in entries.flatten().map(|entry| entry.path()) {
        if !path.is_file() {
            continue;
        }
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        // A timestamp in the future (a clock that moved back) reads as an
        // age of zero, which no rule here accepts as old.
        let age = metadata
            .modified()
            .ok()
            .and_then(|when| when.elapsed().ok())
            .unwrap_or_default();
        if !remove(&path, age) {
            continue;
        }
        if std::fs::remove_file(&path).is_ok() {
            freed.files += 1;
            freed.bytes += metadata.len();
        }
    }
    freed
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::SystemTime;

    fn folder(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("vespera-cache-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("creates");
        dir
    }

    fn age(file: &std::path::Path, age: Duration) {
        std::fs::File::options()
            .write(true)
            .open(file)
            .expect("opens")
            .set_modified(SystemTime::now() - age)
            .expect("ages");
    }

    #[test]
    fn bytes_are_reported_the_way_people_read_them() {
        assert_eq!(size(0), "0 B");
        assert_eq!(size(999), "999 B");
        assert_eq!(size(1536), "1.5 KB");
        assert_eq!(size(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn a_sweep_keeps_what_the_archive_still_points_at() {
        let dir = folder("sweep");
        let kept = dir.join("kept.bin");
        let stale = dir.join("stale.bin");
        std::fs::write(&kept, b"kept").expect("writes");
        std::fs::write(&stale, b"stale").expect("writes");
        // Both files are fresh: a write in flight is never pulled away.
        assert_eq!(sweep(&dir, &|path| path == kept.as_path()).files, 0);
        assert!(stale.is_file());
        age(&kept, SETTLE + Duration::from_secs(60));
        age(&stale, SETTLE + Duration::from_secs(60));
        let freed = sweep(&dir, &|path| path == kept.as_path());
        assert_eq!(freed.files, 1);
        assert_eq!(freed.bytes, 5);
        assert!(kept.is_file());
        assert!(!stale.exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn usage_adds_up_one_folder() {
        let dir = folder("usage");
        std::fs::write(dir.join("one"), [0u8; 10]).expect("writes");
        std::fs::write(dir.join("two"), [0u8; 32]).expect("writes");
        let total = usage(&dir);
        assert_eq!(total.files, 2);
        assert_eq!(total.bytes, 42);
        assert!(!total.is_empty());
        assert!(usage(&dir.join("missing")).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_expiry_takes_only_what_aged_past_it() {
        let dir = folder("expire");
        let old = dir.join("old.bin");
        let fresh = dir.join("fresh.bin");
        std::fs::write(&old, b"old").expect("writes");
        std::fs::write(&fresh, b"fresh").expect("writes");
        age(&old, Duration::from_secs(3600));
        let freed = expire(&dir, Duration::from_secs(600));
        assert_eq!(freed.files, 1);
        assert!(!old.exists());
        assert!(fresh.is_file());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_cap_drops_the_oldest_unkept_file_and_leaves_the_kept_one() {
        let dir = folder("trim");
        let kept = dir.join("kept.bin");
        let old = dir.join("old.bin");
        let newer = dir.join("newer.bin");
        std::fs::write(&kept, vec![1u8; 40]).expect("writes");
        std::fs::write(&old, vec![2u8; 40]).expect("writes");
        std::fs::write(&newer, vec![3u8; 40]).expect("writes");
        age(&kept, SETTLE + Duration::from_secs(90));
        age(&old, SETTLE + Duration::from_secs(80));
        age(&newer, SETTLE + Duration::from_secs(70));
        let freed = trim_to(&dir, 50, &|path| path == kept.as_path());
        assert_eq!(freed.files, 2, "both unkept files go, oldest first");
        assert!(kept.is_file(), "a kept file stays even over the cap");
        assert!(!old.exists());
        assert!(!newer.exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_cap_counts_a_nested_file() {
        let dir = folder("trim-nested");
        let nested = dir.join("link-videos");
        std::fs::create_dir_all(&nested).expect("creates");
        let clip = nested.join("clip.mp4");
        std::fs::write(&clip, vec![9u8; 80]).expect("writes");
        age(&clip, SETTLE + Duration::from_secs(90));
        let freed = trim_to(&dir, 10, &|_| false);
        assert_eq!(freed.files, 1);
        assert!(!clip.exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}
