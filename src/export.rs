//! Chat export: text, HTML, copied media, and a SHA-256 manifest.
//!
//! The set digest is SHA-256 over the sorted UTF-8 lines `path hash\n`.
//! The manifest records that digest and is not part of the set, so the
//! line can be checked without hashing the file that contains it.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// One archived message, already turned into text. Media paths are local files.
pub struct Note {
    pub when: i64,
    pub author: String,
    pub body: String,
    pub edited: bool,
    pub deleted: bool,
    pub files: Vec<PathBuf>,
}

/// Open text and HTML files for one export folder.
pub struct Writer {
    stem: String,
    txt_path: PathBuf,
    html_path: PathBuf,
    media: PathBuf,
    manifest_path: PathBuf,
    txt: std::fs::File,
    html: std::fs::File,
    hashes: Vec<(String, String)>,
    next_file: u32,
    messages: u64,
}

/// Inclusive calendar dates, or the whole archive when a side is blank.
/// The end instant is the start of the day after `until`.
pub fn period(from: &str, until: &str) -> Result<(i64, i64), String> {
    let start = day_bound(from, false)?;
    let end = day_bound(until, true)?;
    if start >= end {
        return Err("The end date is before the start date".to_owned());
    }
    Ok((start, end))
}

fn day_bound(text: &str, end: bool) -> Result<i64, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(if end { i64::MAX } else { 0 });
    }
    let date: jiff::civil::Date = text
        .parse()
        .map_err(|_| "Use a date like 2026-10-01".to_owned())?;
    let date = if end {
        date.checked_add(jiff::Span::new().days(1))
            .map_err(|_| "That date is not valid".to_owned())?
    } else {
        date
    };
    let zoned = date
        .at(0, 0, 0, 0)
        .to_zoned(jiff::tz::TimeZone::system())
        .map_err(|_| "That date is not valid".to_owned())?;
    Ok(zoned.timestamp().as_second())
}

/// A file-name stem that cannot leave the chosen folder.
pub fn stem(name: &str) -> String {
    let mut out = String::new();
    for character in name.chars() {
        if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
            out.push(character);
        }
        if out.len() >= 40 {
            break;
        }
    }
    if out.is_empty() {
        "chat".to_owned()
    } else {
        out
    }
}

impl Writer {
    pub fn begin(folder: &Path, stem: &str) -> Result<Self, String> {
        let stem = stem.to_owned();
        let txt_path = folder.join(format!("{stem}.txt"));
        let html_path = folder.join(format!("{stem}.html"));
        let media = folder.join(format!("{stem}-media"));
        let manifest_path = folder.join(format!("{stem}-manifest.txt"));
        std::fs::create_dir_all(&media).map_err(|error| error.to_string())?;
        let txt = std::fs::File::create(&txt_path).map_err(|error| error.to_string())?;
        let mut html = std::fs::File::create(&html_path).map_err(|error| error.to_string())?;
        html.write_all(
            b"<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\"><title>Chat export</title></head><body>\n",
        )
        .map_err(|error| error.to_string())?;
        Ok(Self {
            stem,
            txt_path,
            html_path,
            media,
            manifest_path,
            txt,
            html,
            hashes: Vec::new(),
            next_file: 0,
            messages: 0,
        })
    }

    pub fn push(&mut self, note: &Note) -> Result<(), String> {
        let stamp = stamp(note.when);
        let author = one_line(&note.author);
        let mut marks = String::new();
        if note.edited {
            marks.push_str(" [edited]");
        }
        if note.deleted {
            marks.push_str(" [deleted]");
        }
        let header = format!("[{stamp}] {author}{marks}");
        writeln!(self.txt, "{header}").map_err(|error| error.to_string())?;
        for line in note.body.lines() {
            writeln!(self.txt, "{line}").map_err(|error| error.to_string())?;
        }
        writeln!(self.html, "<article><p>{}</p>", escape(&header))
            .map_err(|error| error.to_string())?;
        writeln!(self.html, "<pre>{}</pre>", escape(&note.body))
            .map_err(|error| error.to_string())?;
        for source in &note.files {
            if !source.is_file() {
                writeln!(self.txt, "(file not on this computer)")
                    .map_err(|error| error.to_string())?;
                writeln!(self.html, "<p>(file not on this computer)</p>")
                    .map_err(|error| error.to_string())?;
                continue;
            }
            self.next_file += 1;
            let file_name = format!("{:04}-{}", self.next_file, safe_name(source));
            let dest = self.media.join(&file_name);
            std::fs::copy(source, &dest).map_err(|error| error.to_string())?;
            let hash = sha256_file(&dest)?;
            let relative = format!("{}-media/{file_name}", self.stem);
            writeln!(self.txt, "[file] {relative}").map_err(|error| error.to_string())?;
            writeln!(
                self.html,
                "<p><a href=\"{}\">{}</a></p>",
                escape(&relative),
                escape(&file_name)
            )
            .map_err(|error| error.to_string())?;
            self.hashes.push((relative, hash));
        }
        writeln!(self.txt).map_err(|error| error.to_string())?;
        writeln!(self.html, "</article>").map_err(|error| error.to_string())?;
        self.messages += 1;
        Ok(())
    }

    pub fn finish(mut self) -> Result<(), String> {
        self.html
            .write_all(b"</body></html>\n")
            .map_err(|error| error.to_string())?;
        self.txt.flush().map_err(|error| error.to_string())?;
        self.html.flush().map_err(|error| error.to_string())?;
        let messages = self.messages;
        self.hashes
            .push((file_name(&self.txt_path), sha256_file(&self.txt_path)?));
        self.hashes
            .push((file_name(&self.html_path), sha256_file(&self.html_path)?));
        self.hashes.sort_by(|left, right| left.0.cmp(&right.0));
        let set = set_digest(&self.hashes);
        let mut manifest =
            std::fs::File::create(&self.manifest_path).map_err(|error| error.to_string())?;
        writeln!(manifest, "# Vespera chat export").map_err(|error| error.to_string())?;
        writeln!(
            manifest,
            "# set is SHA-256 of the sorted lines \"path hash\" below, each ending in a newline."
        )
        .map_err(|error| error.to_string())?;
        writeln!(manifest, "messages {messages}").map_err(|error| error.to_string())?;
        for (path, hash) in &self.hashes {
            writeln!(manifest, "{path} {hash}").map_err(|error| error.to_string())?;
        }
        writeln!(manifest, "set {set}").map_err(|error| error.to_string())?;
        Ok(())
    }

    /// Removes the files this export created. A cancelled run leaves no manifest.
    pub fn discard(self) {
        drop(self.txt);
        drop(self.html);
        let _ = std::fs::remove_file(&self.txt_path);
        let _ = std::fs::remove_file(&self.html_path);
        let _ = std::fs::remove_dir_all(&self.media);
        let _ = std::fs::remove_file(&self.manifest_path);
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_owned())
}

fn one_line(text: &str) -> String {
    text.replace(['\n', '\r'], " ")
}

fn safe_name(path: &Path) -> String {
    let raw = file_name(path);
    let mut out = String::new();
    for character in raw.chars() {
        if character.is_ascii_alphanumeric()
            || character == '.'
            || character == '-'
            || character == '_'
        {
            out.push(character);
        } else {
            out.push('_');
        }
        if out.len() >= 80 {
            break;
        }
    }
    if out.is_empty() || out == "." || out == ".." {
        "file".to_owned()
    } else {
        out
    }
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(character),
        }
    }
    out
}

/// Local time `YYYY-MM-DD HH:MM:SS`. A timestamp that will not convert
/// is written as its Unix second so the line still exists.
pub fn stamp(unix_seconds: i64) -> String {
    let Some(timestamp) = jiff::Timestamp::from_second(unix_seconds).ok() else {
        return unix_seconds.to_string();
    };
    let zoned = timestamp.to_zoned(jiff::tz::TimeZone::system());
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        zoned.year(),
        zoned.month(),
        zoned.day(),
        zoned.hour(),
        zoned.minute(),
        zoned.second()
    )
}

pub fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex(&hasher.finalize()))
}

pub fn set_digest(entries: &[(String, String)]) -> String {
    let mut lines: Vec<String> = entries
        .iter()
        .map(|(path, hash)| format!("{path} {hash}\n"))
        .collect();
    lines.sort();
    let mut hasher = Sha256::new();
    for line in &lines {
        hasher.update(line.as_bytes());
    }
    hex(&hasher.finalize())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_dates_cover_everything_and_a_day_moves_forward() {
        let (start, end) = period("", "").expect("whole range");
        assert_eq!(start, 0);
        assert_eq!(end, i64::MAX);
        let (day, next) = period("2026-01-15", "2026-01-15").expect("one day");
        let span = next - day;
        assert!((23 * 3600..=25 * 3600).contains(&span), "{span}");
        assert!(period("2026-02-01", "2026-01-01").is_err());
        assert!(period("tomorrow", "").is_err());
    }

    #[test]
    fn stem_stays_inside_the_folder() {
        assert_eq!(stem(".."), "chat");
        assert_eq!(stem("Ada & Bob"), "AdaBob");
        assert_eq!(stem(""), "chat");
    }

    #[test]
    fn export_marks_edits_and_deletions_and_hashes_the_set() {
        let root = tempfile::tempdir().expect("dir");
        let media = root.path().join("source.jpg");
        std::fs::write(&media, b"picture-bytes").expect("media");
        let mut writer = Writer::begin(root.path(), "Ada").expect("begin");
        writer
            .push(&Note {
                when: 1_700_000_000,
                author: "Ada".to_owned(),
                body: "hello <there>".to_owned(),
                edited: true,
                deleted: false,
                files: vec![media],
            })
            .expect("push");
        writer
            .push(&Note {
                when: 1_700_000_100,
                author: "You".to_owned(),
                body: "This message was deleted".to_owned(),
                edited: false,
                deleted: true,
                files: vec![PathBuf::from("missing-file")],
            })
            .expect("deleted");
        writer.finish().expect("finish");
        let text = std::fs::read_to_string(root.path().join("Ada.txt")).expect("txt");
        assert!(text.contains("[edited]"));
        assert!(text.contains("hello <there>"));
        assert!(text.contains("[deleted]"));
        assert!(text.contains("(file not on this computer)"));
        assert!(text.contains("[file] Ada-media/0001-source.jpg"));
        let html = std::fs::read_to_string(root.path().join("Ada.html")).expect("html");
        assert!(html.contains("hello &lt;there&gt;"));
        assert!(!html.contains("hello <there>"));
        let copied = std::fs::read(root.path().join("Ada-media/0001-source.jpg")).expect("copy");
        assert_eq!(copied, b"picture-bytes");
        let manifest = std::fs::read_to_string(root.path().join("Ada-manifest.txt")).expect("man");
        assert!(manifest.contains("messages 2"));
        let mut entries = Vec::new();
        let mut set = String::new();
        for line in manifest.lines() {
            if let Some(hash) = line.strip_prefix("set ") {
                set = hash.to_owned();
            } else if let Some((path, hash)) = line.split_once(' ')
                && !path.starts_with('#')
                && path != "messages"
            {
                entries.push((path.to_owned(), hash.to_owned()));
            }
        }
        assert_eq!(set, set_digest(&entries));
        assert_eq!(entries.len(), 3);
        for (path, hash) in &entries {
            let file = root.path().join(path);
            assert_eq!(sha256_file(&file).expect("hash"), *hash, "{path}");
        }
    }

    #[test]
    fn a_cancelled_export_removes_its_files() {
        let root = tempfile::tempdir().expect("dir");
        let writer = Writer::begin(root.path(), "chat").expect("begin");
        writer.discard();
        assert!(!root.path().join("chat.txt").exists());
        assert!(!root.path().join("chat.html").exists());
        assert!(!root.path().join("chat-media").exists());
        assert!(!root.path().join("chat-manifest.txt").exists());
    }
}
