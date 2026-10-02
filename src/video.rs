//! In-app playback for chat videos.
//!
//! On Windows the picture comes from Media Foundation, which can use the
//! GPU and the codecs the system already has. OpenH264 remains when that
//! decoder cannot open the file. Playback stays at the source size up to
//! 1080p. The soundtrack still plays through rodio.

use std::cell::Cell;
use std::collections::VecDeque;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    mpsc::{Receiver, SyncSender, sync_channel},
};
use std::time::{Duration, Instant};

use egui::{ColorImage, TextureHandle, TextureOptions, Vec2};

/// Widest frame decoded for a chat poster. The moving picture uses
/// [`playback_limit`], not this cap.
const POSTER_WIDTH: u32 = 480;
/// Widest frame decoded for a scrub preview. Thumbnails do not need playback
/// width: 320 keeps text legible while cutting resize and texture bytes.
const PREVIEW_WIDTH: u32 = 320;

/// How many frames may wait. A small picture keeps 60. A 1080p frame is
/// about 8 MB, so the queue stays near 32 MB instead of hundreds.
pub fn frame_budget(width: u32, height: u32) -> usize {
    let bytes = (width as usize)
        .saturating_mul(height as usize)
        .saturating_mul(4)
        .max(1);
    const MAX_QUEUED: usize = 32 * 1024 * 1024;
    (MAX_QUEUED / bytes).clamp(2, BUFFER_FRAMES)
}

/// Even output size for playback: the source size without downscaling.
pub fn playback_limit(width: u32, height: u32) -> (u32, u32) {
    let width = width.max(2);
    let height = height.max(2);
    ((width & !1), (height & !1))
}
/// Sample rate of extracted soundtracks, matching the voice pipeline.
const PCM_RATE: u32 = 48_000;
/// How much soundtrack is kept: five minutes cover any chat video.
const PCM_CAP_SECS: u64 = 300;
/// Frames waiting to be shown. Caps memory while surviving decode hiccups.
const BUFFER_FRAMES: usize = 60;
/// Samples reported per span by the cached soundtrack. Rodio rebuilds its
/// rate converter at span boundaries; an unbounded span would freeze the
/// converter on the opening rate (see audio playback for the same fix).
const SPAN_SAMPLES: usize = 4_096;
/// How long a shown frame is kept behind the buffer for a pause or a seek.
const KEEP_BEHIND: Duration = Duration::from_secs(1);

static PLAYBACK_WORKERS: AtomicU64 = AtomicU64::new(0);
static ENGINES_ALIVE: AtomicU64 = AtomicU64::new(0);
static ENGINE_PEAK: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static FRAME_GENERATION: Cell<u64> = const { Cell::new(0) };
}

/// Highest number of playback decoders alive at once since the last reset.
#[cfg(test)]
pub(crate) fn engine_peak() -> u64 {
    ENGINE_PEAK.load(Ordering::Relaxed)
}

#[cfg(test)]
pub(crate) fn reset_engine_peak() {
    ENGINE_PEAK.store(ENGINES_ALIVE.load(Ordering::Relaxed), Ordering::Relaxed);
}

/// One live playback decoder. Dropped when that decoder is dropped.
struct EngineGuard;

impl EngineGuard {
    fn enter() -> Self {
        let now = ENGINES_ALIVE.fetch_add(1, Ordering::Relaxed) + 1;
        let mut peak = ENGINE_PEAK.load(Ordering::Relaxed);
        while now > peak {
            match ENGINE_PEAK.compare_exchange_weak(peak, now, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => break,
                Err(seen) => peak = seen,
            }
        }
        Self
    }
}

impl Drop for EngineGuard {
    fn drop(&mut self) {
        ENGINES_ALIVE.fetch_sub(1, Ordering::Relaxed);
    }
}

/// How close before the target a picture still counts as landing the
/// seek. A jump to the very end aims past the last sample timestamp, so
/// the final picture (about one frame interval early) must settle the
/// seek instead of reading as an undecodable file. Same 80 ms the viewer
/// already allows when settling a jump on buffered pictures.
const SEEK_LANDING_TOLERANCE: Duration = Duration::from_millis(80);

/// A picture at `pts` is the first one a seek may send.
fn frame_reaches_seek(pts: Duration, target: Duration) -> bool {
    pts.saturating_add(SEEK_LANDING_TOLERANCE) >= target
}

/// Commands for the single decode thread of one open video.
struct PlaybackControl {
    target: Mutex<Duration>,
    paused: Arc<AtomicBool>,
    stop: AtomicBool,
    ffmpeg: AtomicBool,
    volume: Arc<AtomicU32>,
    seek_ms: Arc<AtomicU64>,
    clock_us: Arc<AtomicU64>,
    native_audio: Arc<AtomicBool>,
    pair: Mutex<()>,
    wake: Condvar,
}

impl PlaybackControl {
    fn new(target: Duration, ffmpeg: bool, initial_volume: f32) -> Arc<Self> {
        Arc::new(Self {
            target: Mutex::new(target),
            paused: Arc::new(AtomicBool::new(false)),
            stop: AtomicBool::new(false),
            ffmpeg: AtomicBool::new(ffmpeg),
            volume: Arc::new(AtomicU32::new(initial_volume.to_bits())),
            seek_ms: Arc::new(AtomicU64::new(u64::MAX)),
            clock_us: Arc::new(AtomicU64::new(u64::MAX)),
            native_audio: Arc::new(AtomicBool::new(false)),
            pair: Mutex::new(()),
            wake: Condvar::new(),
        })
    }

    fn clock(&self) -> Option<Duration> {
        let us = self.clock_us.load(Ordering::Acquire);
        if us == u64::MAX {
            None
        } else {
            Some(Duration::from_micros(us))
        }
    }

    #[cfg(target_os = "windows")]
    fn set_clock(&self, clock: Duration) {
        self.clock_us
            .store(clock.as_micros() as u64, Ordering::Release);
    }

    fn clear_clock(&self) {
        self.clock_us.store(u64::MAX, Ordering::Release);
    }

    fn has_native_audio(&self) -> bool {
        self.native_audio.load(Ordering::Acquire)
    }

    fn set_native_audio(&self, active: bool) {
        self.native_audio.store(active, Ordering::Release);
    }

    fn set_volume(&self, volume: f32) {
        self.volume.store(volume.to_bits(), Ordering::Release);
    }

    fn target(&self) -> Duration {
        *self
            .target
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn set_target(&self, target: Duration) {
        *self
            .target
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = target;
        self.seek_ms
            .store(target.as_millis() as u64, Ordering::Release);
        self.clear_clock();
        self.notify();
    }

    fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::SeqCst);
        self.notify();
    }

    fn request_ffmpeg(&self) {
        self.ffmpeg.store(true, Ordering::SeqCst);
        self.notify();
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    fn notify(&self) {
        let _guard = self
            .pair
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        self.wake.notify_all();
    }

    fn block_while_paused(&self, generation: &AtomicU64, pass: u64) {
        if !self.paused.load(Ordering::SeqCst) {
            return;
        }
        let mut guard = self
            .pair
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        while self.paused.load(Ordering::SeqCst)
            && !self.stopped()
            && !self.ffmpeg.load(Ordering::SeqCst)
            && generation.load(Ordering::SeqCst) == pass
        {
            let (next, _) = self
                .wake
                .wait_timeout(guard, Duration::from_millis(200))
                .unwrap_or_else(|poison| poison.into_inner());
            guard = next;
        }
    }

    fn wait_for_work(&self, generation: &AtomicU64, pass: u64) {
        let mut guard = self
            .pair
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        loop {
            if self.stopped()
                || self.ffmpeg.load(Ordering::SeqCst)
                || generation.load(Ordering::SeqCst) != pass
            {
                return;
            }
            let (next, _) = self
                .wake
                .wait_timeout(guard, Duration::from_millis(200))
                .unwrap_or_else(|poison| poison.into_inner());
            guard = next;
        }
    }
}

/// What the viewer needs to know before the first frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Clip {
    pub duration: Duration,
    pub width: u32,
    pub height: u32,
    pub has_audio: bool,
    /// True when frames come from ffmpeg instead of the in-process decoder.
    pub ffmpeg: bool,
}

/// Reads the header of an MP4 and reports whether it can play in-app.
///
/// Only H.264 video tracks decode in-process. Anything else (HEVC, a broken
/// header, no video track at all) reports why, and the viewer offers the
/// system player instead of a spinner that never ends.
pub fn probe(path: &Path) -> Result<Clip, String> {
    let file =
        std::fs::File::open(path).map_err(|error| format!("Could not open the video: {error}"))?;
    let size = file
        .metadata()
        .map_err(|error| format!("Could not read the video: {error}"))?
        .len();
    let mut mp4 = mp4::Mp4Reader::read_header(BufReader::new(file), size)
        .map_err(|_| "This video is not an MP4 file, or it is damaged.".to_owned())?;
    let track = mp4
        .tracks()
        .values()
        .find(|track| track.track_type().ok() == Some(mp4::TrackType::Video))
        .ok_or_else(|| "This file has no video track.".to_owned())?;
    // Parameter sets only exist for H.264 tracks; their absence means a
    // codec openh264 cannot read, like HEVC (iPhones) or AV1/VP9.
    if track.sequence_parameter_set().is_err() || track.picture_parameter_set().is_err() {
        // Windows plays this through Media Foundation, which has the system
        // codecs (including HEVC when the machine has them).
        #[cfg(windows)]
        {
            return system_clip(&mut mp4);
        }
        #[cfg(not(windows))]
        {
            let kind = track
                .box_type()
                .map(|kind| kind.to_string())
                .unwrap_or_default();
            let lower = kind.to_ascii_lowercase();
            if lower.contains("hvc") || lower.contains("hev") {
                return Err("This video uses HEVC (often from iPhone), which this app cannot play in-process. Open it in the default app instead.".to_owned());
            }
            if lower.contains("av01") || lower.contains("vp09") || lower.contains("vp08") {
                return Err(format!(
                    "This video uses {kind}, which this app cannot play in-process. Open it in the default app instead."
                ));
            }
            return Err(
                "This video uses a codec this app cannot play. Open it in the default app instead."
                    .to_owned(),
            );
        }
    }
    // Fragmented files stamp no length in any header; the last sample stamps it.
    let (track_id, timescale, width, height, header_count) = (
        track.track_id(),
        u64::from(track.timescale().max(1)),
        track.width(),
        track.height(),
        track.sample_count(),
    );
    // The profile check ends the shared borrow of the header here, before
    // anything takes the reader mutably below.
    let non_baseline = !matches!(
        track.video_profile(),
        Ok(mp4::AvcProfile::AvcBaseline) | Ok(mp4::AvcProfile::AvcConstrainedBaseline)
    );
    let mut duration = mp4.duration();
    if duration.is_zero() {
        duration = sniff_duration(&mut mp4, track_id, timescale, header_count);
    }
    if duration.is_zero() {
        if mp4.is_fragmented() {
            return Err("This video is fragmented and needs ffmpeg to play in-process. Open it in the default app instead.".to_owned());
        }
        return Err("This video has no readable length.".to_owned());
    }
    let has_audio = mp4
        .tracks()
        .values()
        .any(|track| track.track_type().ok() == Some(mp4::TrackType::Audio));
    // The in-process decoder stamps pictures in decode order, which only
    // matches presentation order for baseline layouts without B-frames.
    // Reordered tracks play through ffmpeg, which presents correctly.
    if reorder_needs_ffmpeg(non_baseline, &mut mp4, track_id) {
        #[cfg(windows)]
        {
            return Ok(Clip {
                duration,
                width: u32::from(width.max(2)),
                height: u32::from(height.max(2)),
                has_audio,
                ffmpeg: false,
            });
        }
        #[cfg(not(windows))]
        {
            if ffmpeg_present() {
                return Ok(Clip {
                    duration,
                    width: u32::from(width.max(2)),
                    height: u32::from(height.max(2)),
                    has_audio,
                    ffmpeg: true,
                });
            }
            return Err("This video reorders frames (B-frames), which needs ffmpeg to play in-process. Open it in the default app instead.".to_owned());
        }
    }
    Ok(Clip {
        duration,
        width: u32::from(width.max(2)),
        height: u32::from(height.max(2)),
        has_audio,
        ffmpeg: false,
    })
}

/// A clip Media Foundation can open when the in-process H.264 reader cannot.
#[cfg(windows)]
fn system_clip(
    mp4: &mut mp4::Mp4Reader<std::io::BufReader<std::fs::File>>,
) -> Result<Clip, String> {
    let track = mp4
        .tracks()
        .values()
        .find(|track| track.track_type().ok() == Some(mp4::TrackType::Video))
        .ok_or_else(|| "This file has no video track.".to_owned())?;
    let (track_id, timescale, width, height, header_count) = (
        track.track_id(),
        u64::from(track.timescale().max(1)),
        track.width(),
        track.height(),
        track.sample_count(),
    );
    let mut duration = mp4.duration();
    if duration.is_zero() {
        duration = sniff_duration(mp4, track_id, timescale, header_count);
    }
    if duration.is_zero() {
        return Err("This video has no readable length.".to_owned());
    }
    let has_audio = mp4
        .tracks()
        .values()
        .any(|track| track.track_type().ok() == Some(mp4::TrackType::Audio));
    Ok(Clip {
        duration,
        width: u32::from(width.max(2)),
        height: u32::from(height.max(2)),
        has_audio,
        ffmpeg: false,
    })
}

/// Early samples bounding the reorder scan: composition offsets of a
/// reordered track show up from the start, and the scan never holds pixels.
const REORDER_SCAN_SAMPLES: u32 = 64;

/// Whether the track reorders frames past what the in-process decoder can
/// stamp. Only non-baseline profiles may carry B-frames, and only an
/// actual composition offset proves it; higher profiles without offsets
/// keep the fast in-process path.
fn reorder_needs_ffmpeg(
    non_baseline: bool,
    mp4: &mut mp4::Mp4Reader<std::io::BufReader<std::fs::File>>,
    track_id: u32,
) -> bool {
    if !non_baseline {
        return false;
    }
    let count = mp4
        .sample_count(track_id)
        .unwrap_or(0)
        .min(REORDER_SCAN_SAMPLES);
    for sample_id in 1..=count {
        match mp4.read_sample(track_id, sample_id) {
            Ok(Some(sample)) if sample.rendering_offset != 0 => return true,
            Ok(_) => {}
            Err(_) => break,
        }
    }
    false
}

/// Length from the last sample, for files whose headers carry none.
///
/// Fragmented files stamp no duration anywhere; one sample read at the end
/// measures the whole track. Absurd counts refuse outright.
fn sniff_duration(
    mp4: &mut mp4::Mp4Reader<BufReader<std::fs::File>>,
    track_id: u32,
    timescale: u64,
    header_count: u32,
) -> Duration {
    // The reader counts whole files; fragments only list per fragment, so the
    // header count backs it up.
    let count = mp4.sample_count(track_id).unwrap_or(0).max(header_count);
    if count == 0 || count > 1_000_000 {
        return Duration::ZERO;
    }
    match mp4.read_sample(track_id, count) {
        Ok(Some(sample)) => stamp(
            sample.start_time.saturating_add(u64::from(sample.duration)),
            0,
            timescale,
        ),
        _ => Duration::ZERO,
    }
}

/// One decoded frame and when it shows, measured from the start.
struct Frame {
    pts: Duration,
    image: ColorImage,
    /// Playback pass that produced this picture. Zero is a test fixture.
    generation: u64,
}
/// What a decode task reports: pictures, clean exhaustion, or a structured
/// failure the viewer can act on instead of guessing silence means loading.
enum DecodeMsg {
    Frame(Frame),
    /// Clean exhaustion from the playback pass stamped inside. Zero means
    /// no pass (test and preview fixtures) and is always honoured.
    End(u64),
    /// A structured failure from the playback pass stamped inside, with
    /// the same zero-means-any rule as `End`.
    Error(DecodeError, u64),
}
/// A decode failure with its engine, for diagnostics and fallback choice.
/// Carries counts, never paths: these lines ship in bug reports.
#[derive(Clone, Debug)]
struct DecodeError {
    engine: &'static str,
    reason: String,
    /// Pictures produced before the failure; zero with many samples read
    /// means the decoder stalled instead of hitting one bad sample.
    produced: u64,
    samples: u64,
}
/// Reads a file ffmpeg understands when the in-process header cannot.
///
/// Fragmented files and other codecs play through ffmpeg's pipe instead of
/// refusing, as long as ffmpeg is installed.
pub fn probe_ffmpeg(path: &Path) -> Result<Clip, String> {
    if !ffmpeg_present() {
        return Err("This video needs ffmpeg to play, which is not installed. Open it in the default app instead.".to_owned());
    }
    let dims = ffprobe(
        path,
        [
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height",
            "-of",
            "csv=p=0",
        ],
    )?;
    let mut parts = dims.trim().split(',');
    let width: u32 = parts
        .next()
        .and_then(|part| part.trim().parse().ok())
        .unwrap_or(0);
    let height: u32 = parts
        .next()
        .and_then(|part| part.trim().parse().ok())
        .unwrap_or(0);
    if width == 0 || height == 0 {
        return Err("This video has no readable picture.".to_owned());
    }
    let duration: f64 = ffprobe(path, ["-show_entries", "format=duration", "-of", "csv=p=0"])?
        .trim()
        .parse()
        .map_err(|_| "This video has no readable length.".to_owned())?;
    if !duration.is_finite() || duration <= 0.0 {
        return Err("This video has no readable length.".to_owned());
    }
    let streams = ffprobe(
        path,
        ["-show_entries", "stream=codec_type", "-of", "csv=p=0"],
    )?;
    let has_audio = streams.lines().any(|line| line.trim() == "audio");
    Ok(Clip {
        duration: Duration::from_secs_f64(duration),
        width,
        height,
        has_audio,
        ffmpeg: true,
    })
}
/// Runs ffprobe with extra arguments and returns its standard output.
fn ffprobe<I, S>(path: &Path, args: I) -> Result<String, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let mut probe = std::process::Command::new("ffprobe");
    let output = quiet(&mut probe)
        .args(["-v", "error"])
        .args(args)
        .arg(path)
        .output()
        .map_err(|_| "Cannot run ffprobe.".to_owned())?;
    if !output.status.success() {
        return Err("This file is not a readable video.".to_owned());
    }
    String::from_utf8(output.stdout).map_err(|_| "This file is not a readable video.".to_owned())
}
/// Mono 48 kHz samples extracted up front, played from any offset.
///
/// Some files arrange their metadata so the streaming decoder cannot read
/// the soundtrack at all. Those are decoded once in the background and
/// sliced here, so seeking stays instant after the first wait.
struct MemSamples {
    data: Arc<Vec<f32>>,
    pos: usize,
    total: Duration,
}
impl MemSamples {
    fn from_pcm(pcm: Arc<Vec<f32>>, skip: usize) -> Self {
        let pos = skip.min(pcm.len());
        let total =
            Duration::from_secs_f32((pcm.len().saturating_sub(pos) as f32) / PCM_RATE as f32);
        Self {
            data: pcm,
            pos,
            total,
        }
    }
}
impl Iterator for MemSamples {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        let sample = *self.data.get(self.pos)?;
        self.pos += 1;
        Some(sample)
    }
}
impl rodio::Source for MemSamples {
    fn current_span_len(&self) -> Option<usize> {
        Some((self.data.len() - self.pos).clamp(1, SPAN_SAMPLES))
    }
    fn channels(&self) -> std::num::NonZero<u16> {
        std::num::NonZero::<u16>::MIN
    }
    fn sample_rate(&self) -> std::num::NonZero<u32> {
        std::num::NonZero::new(PCM_RATE).expect("48 kHz is not zero")
    }
    fn total_duration(&self) -> Option<Duration> {
        Some(self.total)
    }
}
/// What one background pass over a downloaded video learns: its real
/// length and a poster worth showing. The phone's metadata and thumbnail
/// stay on screen until this answers.
pub struct VideoAnalysis {
    pub seconds: Option<u32>,
    pub poster: Option<Vec<u8>>,
}

/// Reads a downloaded video's length and builds a poster from its own
/// frames. Zero stays unknown: the bubble omits the length instead of
/// showing a misleading zero.
pub fn analyze(path: &Path) -> VideoAnalysis {
    let seconds = probe(path)
        .or_else(|_| probe_ffmpeg(path))
        .ok()
        .map(|clip| clip.duration.as_secs() as u32)
        .filter(|seconds| *seconds > 0);
    let poster = best_frame(path, POSTER_WIDTH)
        .or_else(|| ffmpeg_poster(path))
        .and_then(encode_poster);
    VideoAnalysis { seconds, poster }
}

/// The first usable picture of a video, for posters.
///
/// A black opening or a transition is skipped in favour of the frames
/// right after it; what cannot be read keeps the phone's thumbnail.
fn best_frame(path: &Path, width: u32) -> Option<image::RgbaImage> {
    let (mut first, mut bright) = (None, None);
    decode_frames(path, width, 60, &mut |image| {
        if first.is_none() {
            first = Some(image.clone());
        }
        if bright.is_none() && mean_luma(&image) > 10.0 {
            bright = Some(image.clone());
        }
        bright.is_some()
    });
    bright.or(first)
}

/// Mean brightness of a frame, from black (0) to white (255).
fn mean_luma(image: &image::RgbaImage) -> f32 {
    if image.as_raw().is_empty() {
        return 0.0;
    }
    let (mut sum, mut count) = (0u64, 0u64);
    for pixel in image.pixels().step_by(7) {
        let [red, green, blue, _] = pixel.0;
        sum += u64::from(red) + u64::from(green) + u64::from(blue);
        count += 1;
    }
    if count == 0 {
        return 0.0;
    }
    sum as f32 / (3.0 * count as f32)
}

/// OpenH264 keeps process-global decoder state. A second
/// `WelsCreateDecoder` while another decoder still exists access-violates
/// on Windows (the chat player thread outliving a test, then the next
/// clip). Hold this for the whole life of one `Decoder`.
pub(crate) fn openh264_session() -> std::sync::MutexGuard<'static, ()> {
    static SESSION: std::sync::Mutex<()> = std::sync::Mutex::new(());
    SESSION.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// Decodes up to `limit` pictures from the start, scaled to a width,
/// stopping early when the visitor has seen enough.
fn decode_frames(
    path: &Path,
    width: u32,
    limit: u32,
    visit: &mut dyn FnMut(image::RgbaImage) -> bool,
) {
    let _session = openh264_session();
    let Ok(file) = std::fs::File::open(path) else {
        return;
    };
    let Ok(size) = file.metadata().map(|meta| meta.len()) else {
        return;
    };
    let Ok(mut mp4) = mp4::Mp4Reader::read_header(BufReader::new(file), size) else {
        return;
    };
    let Some(track) = mp4
        .tracks()
        .values()
        .find(|track| track.track_type().ok() == Some(mp4::TrackType::Video))
    else {
        return;
    };
    // Fragmented files list their samples per fragment; the header count stays empty.
    let (track_id, count) = (
        track.track_id(),
        mp4.sample_count(track.track_id()).unwrap_or(0),
    );
    let (Ok(sps), Ok(pps)) = (
        track.sequence_parameter_set().map(|bytes| bytes.to_vec()),
        track.picture_parameter_set().map(|bytes| bytes.to_vec()),
    ) else {
        return;
    };
    let Ok(mut decoder) = openh264::decoder::Decoder::new() else {
        return;
    };
    let mut parameters = Vec::new();
    push_annex_b(&mut parameters, &sps);
    push_annex_b(&mut parameters, &pps);
    let _ = decoder.decode(&parameters);
    let mut length_size = None;
    for sample_id in 1..=count.min(limit) {
        let Ok(Some(sample)) = mp4.read_sample(track_id, sample_id) else {
            continue;
        };
        // Length size audited once against the first sample, as in playback.
        let size = match length_size {
            Some(size) => size,
            None => match avcc_length_size(&sample.bytes) {
                Some(size) => {
                    length_size = Some(size);
                    size
                }
                None => continue,
            },
        };
        let mut annex_b = Vec::with_capacity(sample.bytes.len() + 16);
        if avcc_to_annex_b(&mut annex_b, &sample.bytes, size).is_err() {
            continue;
        }
        let Ok(Some(yuv)) = decoder.decode(&annex_b) else {
            continue;
        };
        use openh264::formats::YUVSource;
        let (w, h) = yuv.dimensions();
        if w == 0 || h == 0 {
            continue;
        }
        let mut rgba = vec![0u8; w * h * 4];
        yuv.write_rgba8(&mut rgba);
        let Some(image) = image::RgbaImage::from_raw(w as u32, h as u32, rgba) else {
            continue;
        };
        let out_width = (w as u32).min(width).max(2);
        let out_height = ((h as u64 * u64::from(out_width) / w as u64) as u32).max(2);
        let image = if out_width == w as u32 {
            image
        } else {
            image::imageops::resize(
                &image,
                out_width,
                out_height,
                image::imageops::FilterType::Triangle,
            )
        };
        if visit(image) {
            break;
        }
    }
}

/// Encodes a poster frame as JPEG bytes for the archive.
fn encode_poster(image: image::RgbaImage) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgba8(image)
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Jpeg,
        )
        .ok()?;
    Some(bytes)
}
/// One still through ffmpeg, for files the in-process decoder cannot open.
fn ffmpeg_poster(path: &Path) -> Option<image::RgbaImage> {
    if !ffmpeg_present() {
        return None;
    }
    let mut poster = std::process::Command::new("ffmpeg");
    let output = quiet(&mut poster)
        .args(["-v", "error", "-i"])
        .arg(path)
        .args([
            "-frames:v",
            "1",
            "-f",
            "image2pipe",
            "-vcodec",
            "png",
            "pipe:1",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let image = image::load_from_memory(&output.stdout).ok()?.to_rgba8();
    let out_width = image.width().clamp(2, 320);
    let out_height = ((u64::from(image.height()) * u64::from(out_width)
        / u64::from(image.width().max(1))) as u32)
        .max(2);
    Some(if out_width == image.width() {
        image
    } else {
        image::imageops::resize(
            &image,
            out_width,
            out_height,
            image::imageops::FilterType::Triangle,
        )
    })
}
struct Active {
    path: PathBuf,
    clip: Clip,
    /// The soundtrack. The device is held so the stream stays open; only the
    /// sink is read, which is why this is a tuple and not a named struct.
    audio: Option<(rodio::MixerDeviceSink, rodio::Player)>,
    /// A soundtrack decoded up front, sliced instantly on later jumps.
    pcm: Option<Arc<Vec<f32>>>,
    /// A soundtrack still being decoded in the background.
    audio_rx: Option<std::sync::mpsc::Receiver<Vec<f32>>>,
    /// A soundtrack opening in the background after a jump: streaming
    /// sound first, a full extraction when seeking defeats it.
    audio_task: Option<(u64, std::sync::mpsc::Receiver<SeekAudio>)>,
    playing: bool,
    /// Position when playback last (re)started; the sink or the wall clock
    /// counts from there.
    base: Duration,
    /// Sink position when base was established. The sink keeps its own
    /// count across a pause, so resuming from base plus position alone would
    /// count the stretch before the pause twice and jump the picture ahead.
    anchor: Duration,
    started: Instant,
    /// A jump is still catching up: the keyframe still shows until live frames arrive.
    seeking: bool,
    frames: Receiver<DecodeMsg>,
    /// How many pictures may wait in `buffered` and in the decode channel.
    slots: usize,
    buffered: VecDeque<Frame>,
    /// One arrival that did not fit, kept for the next tick. The channel
    /// cannot take it back, so without this slot a full buffer would drop
    /// one frame per tick.
    held: Option<Frame>,
    shown: Duration,
    /// One texture for the whole clip, updated in place. Allocating a fresh
    /// texture per frame churned GPU memory for nothing, and the handle has
    /// to survive a seek because the picture size never changes.
    texture: Option<TextureHandle>,
    generation: Arc<AtomicU64>,
    /// The one decode thread. Seeks retarget it; they do not start another.
    control: Arc<PlaybackControl>,
    /// This activation's decode worker number. Seeks must leave it
    /// untouched; the process-global counter also moves for other tests'
    /// players, so only this per-player identity proves one decoder.
    /// Production code never branches on it: it exists so tests can
    /// assert worker stability without the racy global delta.
    #[allow(dead_code)]
    worker: u64,
    /// Presentation time of the first picture accepted after the current seek.
    landed_pts: Option<Duration>,
    /// Newest pre-target picture seen during the current seek. The drain
    /// drops those so stale keyframe stills never fill the buffer; when the
    /// stream ends without a live picture (a jump aimed past the last
    /// sample) this one settles the seek instead of refusing the file.
    trailing: Option<Frame>,
    decode_done: bool,
    finished: bool,
    /// The in-process decode already failed over to ffmpeg once on this
    /// activation: a second failure refuses instead of restarting forever.
    fallback_used: bool,
    /// The last decode failure, to refuse with its reason instead of a
    /// generic message when nothing else can play the file.
    decode_error: Option<DecodeError>,
    /// Timings of the active jump, logged once it lands. Durations only,
    /// never paths.
    seek_diag: Option<SeekDiag>,
}

/// Timings of one jump, from request to resumed playback.
struct SeekDiag {
    requested: Instant,
    target: Duration,
    first_frame: Option<Instant>,
    audio_ready: Option<Instant>,
}

/// Plays the video open in the viewer. Only one plays at a time.
pub struct Player {
    active: Option<Active>,
    /// The file the viewer already refused, and why. The view reads this so
    /// opening an unplayable file complains once instead of every frame.
    refused: Option<(PathBuf, String)>,
    volume: f32,
    muted: bool,
}
impl Default for Player {
    fn default() -> Self {
        Self {
            active: None,
            refused: None,
            volume: 1.0,
            muted: false,
        }
    }
}

/// What the viewer paints for the open video.
pub enum State {
    /// The decoder thread is still on its way to the first frame.
    Loading,
    /// A frame to paint, with where playback stands.
    Showing {
        texture: TextureHandle,
        size: Vec2,
        position: Duration,
        total: Duration,
        playing: bool,
        finished: bool,
        /// A jump is catching up: the keyframe still shows, not a scan.
        seeking: bool,
    },
    /// The file cannot play in-process; the viewer offers the system player.
    Unsupported(String),
}

impl Player {
    /// Plays or pauses the open video. A new file starts from the beginning,
    /// after anything else making sound has stopped.
    pub fn toggle(&mut self, path: &Path, stop_audio: &mut dyn FnMut()) -> Result<(), String> {
        if let Some(active) = self.active.as_mut()
            && active.path == path
        {
            if active.playing {
                active.base = active.position();
                if let Some((_, sink)) = &active.audio {
                    active.anchor = sink.get_pos();
                    sink.pause();
                }
                active.playing = false;
                active.control.set_paused(true);
            } else {
                active.finished = false;
                if active.base >= active.clip.duration {
                    // Replaying from the end always starts playing: a
                    // finished clip holds no paused position worth keeping,
                    // and a bare seek would preserve the pause.
                    self.seek(path, 0.0)?;
                    if let Some(active) = self.active.as_mut()
                        && active.path == path
                    {
                        active.started = Instant::now();
                        active.playing = true;
                        if let Some((_, sink)) = &active.audio {
                            sink.play();
                        }
                        active.control.set_paused(false);
                    }
                    return Ok(());
                }
                // Sound cached while paused joins from where it stopped.
                if active.audio.is_none() && active.pcm.is_some() {
                    let base = active.base;
                    let (volume, muted) = (self.volume, self.muted);
                    attach_cached(active, volume, muted, base);
                }
                if let Some((_, sink)) = &active.audio {
                    sink.play();
                }
                active.started = Instant::now();
                active.playing = true;
                active.control.set_paused(false);
            }
            return Ok(());
        }
        stop_audio();
        self.open(path, Duration::ZERO)
    }

    /// Jumps to a fraction of the clip, from 0 to 1, and keeps playing.
    pub fn seek(&mut self, path: &Path, fraction: f32) -> Result<(), String> {
        let total = match self.active.as_ref() {
            Some(active) if active.path == path => active.clip.duration,
            _ => return self.open(path, Duration::ZERO),
        };
        let target = total.mul_f32(fraction.clamp(0.0, 1.0));
        // A jump at the very end would start the decoder past the last
        // sample: it would find no frames and a valid file would read as
        // undecodable. One millisecond earlier still shows the last picture
        // and lets the end state arrive.
        let target = if target >= total && total > Duration::from_millis(1) {
            total - Duration::from_millis(1)
        } else {
            target
        };
        // Cached sound makes the jump instant; otherwise the file reopens.
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.pcm.is_some() && !active.control.has_native_audio())
        {
            let (volume, muted) = (self.volume, self.muted);
            let active = self.active.as_mut().expect("just checked");
            // The same decode thread seeks. A new generation only retires
            // pictures and sound that belonged to the old position.
            note_seek(active, target);
            attach_cached(active, volume, muted, target);
            return Ok(());
        }
        // Without cached sound the jump retargets in place: the headers
        // stay read, the still stays on screen, and the clock holds the
        // target until live frames arrive. Reopening here would reread
        // everything on the interface thread and restart picture and
        // sound apart.
        let active = self.active.as_mut().expect("same path checked");
        let target = target.min(active.clip.duration);
        // A new pass over the same counter stands down the old extraction
        // and the old soundtrack task together: only the newest jump may
        // answer. The picture decoder stays the one opened with the file.
        let current = note_seek(active, target);
        active.base = target;
        active.anchor = Duration::ZERO;
        active.started = Instant::now();
        // The old soundtrack belongs to the old position.
        active.audio = None;
        active.audio_rx = None;
        active.audio_task = None;
        if !active.control.has_native_audio() {
            let (generation, file) = (active.generation.clone(), active.path.clone());
            let (atx, arx) = std::sync::mpsc::channel();
            std::thread::Builder::new()
                .name("video-seek-audio".into())
                .spawn(move || {
                    let _ = atx.send(open_seek_audio(&file, target, &generation, current));
                })
                .ok();
            active.audio_task = Some((current, arx));
        }
        Ok(())
    }

    /// Opens a file at a position, replacing whatever was playing.
    fn open(&mut self, path: &Path, at: Duration) -> Result<(), String> {
        self.stop();
        // Fragmented files and other codecs fall back to ffmpeg instead of refusing.
        let clip = match probe(path) {
            Ok(clip) => {
                if self
                    .refused
                    .as_ref()
                    .is_some_and(|(known, _)| known == path)
                {
                    self.refused = None;
                }
                clip
            }
            // The in-process header failed; ffmpeg gets its chance before refusing.
            Err(_) => match probe_ffmpeg(path) {
                Ok(clip) => {
                    if self
                        .refused
                        .as_ref()
                        .is_some_and(|(known, _)| known == path)
                    {
                        self.refused = None;
                    }
                    clip
                }
                Err(error) => {
                    self.refused = Some((path.to_path_buf(), error.clone()));
                    return Err(error);
                }
            },
        };
        let at = at.min(clip.duration);
        // The counter exists before the soundtrack does, so a jump that
        // lands while it still opens retires this extraction as well.
        let generation = Arc::new(AtomicU64::new(1));
        let mut audio_rx = None;
        let (audio, at) = match audio_at(path, at, &generation, 1) {
            Audio::Sound(audio) => (Some(audio), at),
            Audio::Extracting(rx) => {
                audio_rx = Some(rx);
                (None, at)
            }
            Audio::Silent => (None, at),
        };
        // A jump opens on its keyframe still and resumes at the target once
        // live frames arrive; opening at zero plays straight away.
        let seeking = !at.is_zero();
        let slots = frame_budget(clip.width, clip.height);
        let (tx, rx) = sync_channel::<DecodeMsg>(slots);
        let effective_vol = if self.muted { 0.0 } else { self.volume };
        let control = PlaybackControl::new(at, clip.ffmpeg, effective_vol);
        let worker = spawn_worker(
            path.to_path_buf(),
            clip.duration,
            clip.width,
            clip.height,
            generation.clone(),
            Arc::clone(&control),
            tx,
        );
        if let Some((_, sink)) = &audio {
            sink.set_volume(if self.muted { 0.0 } else { self.volume });
            // Sound waits for the first picture. A slow decoder must not
            // run a short clip out, or finish it, before anything is shown.
            sink.pause();
        }
        self.active = Some(Active {
            path: path.to_path_buf(),
            clip,
            audio,
            pcm: None,
            audio_rx,
            audio_task: None,
            playing: true,
            base: at,
            anchor: Duration::ZERO,
            started: Instant::now(),
            frames: rx,
            slots,
            buffered: VecDeque::new(),
            // Nothing has shown yet; a first frame stamped at zero must still
            // upload instead of looking already painted.
            shown: Duration::MAX,
            held: None,
            texture: None,
            generation,
            control,
            worker,
            landed_pts: None,
            trailing: None,
            decode_done: false,
            seeking,
            finished: false,
            fallback_used: false,
            decode_error: None,
            seek_diag: None,
        });
        Ok(())
    }

    /// Stops playback and drops the soundtrack. The decode thread stands
    /// down when it sees the stop flag; the generation bump retires sound.
    pub fn stop(&mut self) {
        if let Some(active) = self.active.take() {
            active.control.stop.store(true, Ordering::SeqCst);
            active.generation.fetch_add(1, Ordering::SeqCst);
            active.control.notify();
        }
    }

    /// Whether this file is the one playing right now.
    pub fn is_active(&self, path: &Path) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| active.path == path)
    }

    /// Why this file refused to play, if it already did.
    pub fn refusal(&self, path: &Path) -> Option<String> {
        self.refused
            .as_ref()
            .filter(|(known, _)| known == path)
            .map(|(_, why)| why.clone())
    }
    /// Remembers the output level for the next file and applies it to the
    /// one playing now. The view calls this every frame, so playback
    /// follows the slider without reopening anything.
    pub fn set_output(&mut self, volume: f32, muted: bool) {
        let volume = volume.clamp(0.0, 1.0);
        if self.volume == volume && self.muted == muted {
            return;
        }
        self.volume = volume;
        self.muted = muted;
        if let Some(active) = self.active.as_mut() {
            let vol = if muted { 0.0 } else { volume };
            active.control.set_volume(vol);
            if let Some((_, sink)) = &active.audio {
                sink.set_volume(vol);
            }
        }
    }

    /// Pumps decoded frames and answers what the viewer should paint.
    pub fn poll(&mut self, ctx: &egui::Context, path: &Path) -> State {
        let Some(active) = self.active.as_mut() else {
            // A refused file runs no decoder: say why instead of spinning on
            // a player that will never arrive.
            if let Some(why) = self.refusal(path) {
                return State::Unsupported(why);
            }
            return State::Loading;
        };
        if active.path != path {
            return State::Loading;
        }
        // A background extraction joins wherever playback stands. Grabbing
        // the level first keeps one mutable borrow.
        let (volume, muted) = (self.volume, self.muted);
        if active.control.has_native_audio() {
            active.audio = None;
            active.audio_rx = None;
            active.audio_task = None;
            active.pcm = None;
        } else {
            if active.audio.is_none() {
                let mut arrived = None;
                if let Some(rx) = active.audio_rx.as_ref() {
                    arrived = rx.try_recv().ok();
                }
                if let Some(pcm) = arrived {
                    active.audio_rx = None;
                    if !pcm.is_empty() {
                        active.pcm = Some(Arc::new(pcm));
                        let at = active.position();
                        attach_cached(active, volume, muted, at);
                        if active.seeking
                            && let Some(diag) = active.seek_diag.as_mut()
                        {
                            diag.audio_ready.get_or_insert(Instant::now());
                        }
                    }
                }
            }
            // A jump's soundtrack opens beside it. Only the newest jump may
            // answer; an older task's delivery dies with its generation.
            if active.audio.is_none() && active.audio_rx.is_none() {
                let mut outcome = None;
                if let Some((current, rx)) = active.audio_task.as_ref() {
                    let current = *current;
                    if let Ok(answer) = rx.try_recv() {
                        active.audio_task = None;
                        if current == active.generation.load(Ordering::SeqCst) {
                            outcome = Some(answer);
                        }
                    }
                }
                match outcome {
                    Some(SeekAudio::Stream((device, sink))) => {
                        sink.set_volume(if muted { 0.0 } else { volume });
                        let at = active.position();
                        active.audio = Some((device, sink));
                        active.base = at;
                        active.anchor = Duration::ZERO;
                        active.started = Instant::now();
                        // Joint seek transition: sound waits paused while the
                        // picture still catches up, even when playing. It joins
                        // on the landing frame below.
                        if !audio_may_play(active) {
                            if let Some((_, sink)) = &active.audio {
                                sink.pause();
                            }
                        } else if let Some((_, sink)) = &active.audio {
                            sink.play();
                        }
                        if active.seeking
                            && let Some(diag) = active.seek_diag.as_mut()
                        {
                            diag.audio_ready.get_or_insert(Instant::now());
                        }
                    }
                    Some(SeekAudio::Extracting(rx)) => {
                        active.audio_rx = Some(rx);
                    }
                    Some(SeekAudio::Silent) | None => {}
                }
            }
        }
        // The held arrival goes first: it never went back to the channel.
        if let Some(frame) = active.held.take() {
            active.place_frame(frame);
        }
        // Drain only while nothing is held: whatever does not fit stays
        // queued in the channel, and the decoder waits on it. Nothing is
        // ever taken just to be dropped.
        let mut decode_error = None;
        while active.held.is_none() {
            match active.frames.try_recv() {
                Ok(DecodeMsg::Frame(frame)) => {
                    let current = active.generation.load(Ordering::SeqCst);
                    if frame.generation != 0 && frame.generation != current {
                        continue;
                    }
                    if active.seeking
                        && let Some(diag) = &active.seek_diag
                        && !frame_reaches_seek(frame.pts, diag.target)
                    {
                        // Not live for this jump yet. Keep only the newest:
                        // when the stream ends on a low-frame-rate clip, no
                        // picture ever reaches a target aimed past the last
                        // sample, and this final pre-target picture is what
                        // settles the seek as the finished state.
                        active.trailing = Some(frame);
                        continue;
                    }
                    if active.seeking && active.landed_pts.is_none() {
                        active.landed_pts = Some(frame.pts);
                        active.trailing = None;
                    }
                    if active.seeking
                        && let Some(diag) = active.seek_diag.as_mut()
                    {
                        diag.first_frame.get_or_insert(Instant::now());
                    }
                    active.place_frame(frame);
                }
                Ok(DecodeMsg::End(pass)) => {
                    let current = active.generation.load(Ordering::SeqCst);
                    if pass != 0 && pass != current {
                        continue;
                    }
                    if active.seeking
                        && active.landed_pts.is_none()
                        && let Some(frame) = active.trailing.take()
                    {
                        active.place_frame(frame);
                    }
                    active.decode_done = true;
                    break;
                }
                Ok(DecodeMsg::Error(error, pass)) => {
                    let current = active.generation.load(Ordering::SeqCst);
                    if pass != 0 && pass != current {
                        continue;
                    }
                    active.decode_done = true;
                    decode_error = Some(error);
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    active.decode_done = true;
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
            }
        }
        if let Some(error) = decode_error {
            active.decode_error = Some(error.clone());
            // One controlled engine change per activation: position, pause
            // and volume survive, the old pictures stay until live ones
            // arrive, and a second failure refuses instead of looping.
            if !active.clip.ffmpeg && !active.fallback_used && ffmpeg_present() {
                log::warn!(
                    target: "vespera::video",
                    "decode fell back to ffmpeg: engine={} reason={} produced={} samples={}",
                    error.engine, error.reason, error.produced, error.samples,
                );
                active.clip.ffmpeg = true;
                active.fallback_used = true;
                active.decode_done = false;
                active.decode_error = None;
                let at = active.position();
                while active.frames.try_recv().is_ok() {}
                active.control.set_target(at);
                active.control.request_ffmpeg();
                active.generation.fetch_add(1, Ordering::SeqCst);
            }
        }
        let position = active.position();
        // Drop what is well behind, but keep the frame on screen. While a jump
        // catches up every frame stays, keyframe still included.
        if !active.seeking {
            while active.buffered.len() > 1 && active.buffered[1].pts + KEEP_BEHIND < position {
                active.buffered.pop_front();
            }
        }
        let total = active.clip.duration;
        // No picture yet: the clock is held, so a clip shorter than decoder
        // startup cannot be marked finished while the spinner is still up.
        if ready_to_finish(active.shown, position, total) {
            active.playing = false;
            active.finished = true;
            active.base = total;
            if let Some((_, sink)) = &active.audio {
                active.anchor = sink.get_pos();
                sink.pause();
            }
        }
        if soundtrack_ended(active, position, total) {
            // The soundtrack ended before the picture (a short track, a
            // capped extraction): the wall clock drives on instead of
            // freezing the picture on the silent sink.
            active.base = position;
            active.anchor = Duration::ZERO;
            active.started = Instant::now();
            active.audio = None;
        }
        if active.seeking {
            // A jump shows its keyframe still, never a scan.
            let live = active.base.saturating_sub(Duration::from_millis(80));
            let landed = active
                .buffered
                .iter()
                .find(|frame| frame.pts >= live)
                .map(|frame| frame.pts);
            if landed.is_none() && active.decode_done {
                // The decoder is exhausted without a live frame: the jump
                // ran past the last sample, or the file ends mid-jump. At
                // the very end that is the finished state on the last
                // picture; anywhere else the file cannot be played.
                active.playing = false;
                active.seeking = false;
                if let Some((_, sink)) = &active.audio {
                    sink.pause();
                }
                if active.base >= total.saturating_sub(Duration::from_millis(80)) {
                    active.base = total;
                    active.finished = true;
                    match active.buffered.back().map(|frame| frame.pts) {
                        Some(pts) => show_frame(active, ctx, path, pts, position, total),
                        None => State::Unsupported(undecodable(active)),
                    }
                } else {
                    State::Unsupported(undecodable(active))
                }
            } else {
                match landed {
                    Some(pts) => {
                        active.seeking = false;
                        // Live picture and clock resume together from the
                        // target: without this the sound would start ahead by
                        // however long the jump took to decode.
                        active.started = Instant::now();
                        if let Some((_, sink)) = &active.audio {
                            active.anchor = sink.get_pos();
                            // Sound waited paused through the jump: it joins
                            // here, never before the picture.
                            if active.playing {
                                sink.play();
                            }
                        }
                        if let Some(diag) = active.seek_diag.take() {
                            // Durations only, never paths: these lines ship in
                            // bug reports. A missing stage prints as "-".
                            let ms = |done: Option<Instant>| {
                                done.map(|at| (at - diag.requested).as_millis().to_string())
                                    .unwrap_or_else(|| "-".to_owned())
                            };
                            log::debug!(
                                target: "vespera::video",
                                "seek resumed: target={}ms resumed={}ms first_frame={}ms audio_ready={}ms",
                                diag.target.as_millis(),
                                diag.requested.elapsed().as_millis(),
                                ms(diag.first_frame),
                                ms(diag.audio_ready),
                            );
                        }
                        show_frame(active, ctx, path, pts, position, total)
                    }
                    None => {
                        // Work is in flight whether paused or playing: the
                        // window must wake when the jump lands, with no mouse
                        // needed.
                        ctx.request_repaint_after(Duration::from_millis(100));
                        match active.buffered.front().map(|frame| frame.pts) {
                            Some(pts) => show_frame(active, ctx, path, pts, position, total),
                            None => State::Loading,
                        }
                    }
                }
            }
        } else {
            // The first picture is due even when its timestamp is still ahead
            // of a clock that has not started.
            let pts = if active.shown == Duration::MAX {
                active.buffered.front().map(|frame| frame.pts)
            } else {
                choose_pts(
                    active.buffered.iter().map(|frame| frame.pts),
                    position,
                    active.shown,
                )
            };
            match pts {
                Some(pts) => show_frame(active, ctx, path, pts, position, total),
                None if active.decode_done => {
                    active.playing = false;
                    if let Some((_, sink)) = &active.audio {
                        sink.pause();
                    }
                    State::Unsupported(undecodable(active))
                }
                None => {
                    if active.playing {
                        ctx.request_repaint_after(Duration::from_millis(100));
                    }
                    State::Loading
                }
            }
        }
    }
}
/// Refusal text for an undecodable file: the stored decode failure names
/// its engine and reason instead of a generic message, so a bug report
/// can tell a broken file from a decoder that gave up.
fn undecodable(active: &Active) -> String {
    active
        .decode_error
        .as_ref()
        .map(|error| {
            // Without ffmpeg there is no second engine to try: say so
            // instead of leaving a bare engine name. The viewer already
            // offers opening the file in the default app beside this.
            if ffmpeg_present() {
                format!(
                    "This video could not be decoded ({}, {}).",
                    error.engine, error.reason
                )
            } else {
                format!(
                    "This video could not be decoded ({}, {}). Install ffmpeg and put it on PATH to play these files, or open it in the default app.",
                    error.engine, error.reason
                )
            }
        })
        .unwrap_or_else(|| "This video could not be decoded.".to_owned())
}
/// Uploads one frame and answers what the viewer paints for it.
fn show_frame(
    active: &mut Active,
    ctx: &egui::Context,
    path: &Path,
    pts: Duration,
    position: Duration,
    total: Duration,
) -> State {
    // The picture between two decode steps is the same one, so only a new
    // presentation time touches the GPU, and it lands in the clip's own
    // texture instead of a new one.
    if active.shown == Duration::MAX {
        active.started = Instant::now();
        if active.control.has_native_audio() {
            active.audio = None;
            active.audio_rx = None;
            active.audio_task = None;
            active.pcm = None;
        } else if let Some((_, sink)) = &active.audio {
            active.anchor = sink.get_pos();
            if active.playing && !active.seeking {
                sink.play();
            }
        }
    }
    if active.shown != pts
        && let Some(frame) = active.buffered.iter().find(|frame| frame.pts == pts)
    {
        let image = frame.image.clone();
        match active.texture.as_mut() {
            Some(handle) => handle.set(image, TextureOptions::LINEAR),
            None => {
                active.texture = Some(ctx.load_texture(
                    format!("video-{}", path.display()),
                    image,
                    TextureOptions::LINEAR,
                ));
            }
        }
        active.shown = pts;
    }
    let Some(handle) = active.texture.as_ref() else {
        return State::Loading;
    };
    let (texture, size) = (handle.clone(), handle.size_vec2());
    if active.playing {
        ctx.request_repaint_after(next_repaint(active, position));
    }
    State::Showing {
        texture,
        size,
        position: position.min(total),
        total,
        playing: active.playing,
        finished: active.finished,
        seeking: active.seeking,
    }
}

impl Drop for Active {
    fn drop(&mut self) {
        self.control.stop.store(true, Ordering::SeqCst);
        self.control.notify();
    }
}

impl Active {
    fn position(&self) -> Duration {
        if !self.playing || self.seeking || self.shown == Duration::MAX {
            return self.base;
        }
        if let Some(synced) = self.control.clock() {
            return synced;
        }
        match &self.audio {
            Some((_, sink)) => audio_position(self.base, sink.get_pos(), self.anchor),
            None => self.base + self.started.elapsed(),
        }
    }

    /// Files one arrival into the buffer, or holds it for the next tick
    /// when only future frames fill the buffer. The channel cannot take a
    /// frame back, so holding is what keeps a full queue lossless.
    fn place_frame(&mut self, frame: Frame) {
        let room = buffer_room(
            self.buffered.len(),
            self.buffered.front().map(|frame| frame.pts),
            self.position(),
            self.slots,
        );
        match room {
            BufferRoom::Push => self.buffered.push_back(frame),
            BufferRoom::EvictThenPush => {
                self.buffered.pop_front();
                self.buffered.push_back(frame);
            }
            BufferRoom::Hold => {
                self.held = Some(frame);
            }
        }
    }
}

/// Media position over a live soundtrack. The sink keeps its own count
/// across a pause, so the anchor taken when the base was established is
/// subtracted first: without it the stretch before the pause counts twice.
fn audio_position(base: Duration, sink_pos: Duration, anchor: Duration) -> Duration {
    base + sink_pos.saturating_sub(anchor)
}

/// True when the sink has run out of samples while the picture still has
/// time left. Some backends never report empty at the last sample, so the
/// extracted PCM length is also the end of the soundtrack.
fn soundtrack_ended(active: &Active, position: Duration, total: Duration) -> bool {
    if active.finished || position >= total {
        return false;
    }
    let Some((_, sink)) = &active.audio else {
        return false;
    };
    if sink.empty() {
        return true;
    }
    let Some(pcm) = &active.pcm else {
        return false;
    };
    let pcm_end = Duration::from_secs_f64(pcm.len() as f64 / f64::from(PCM_RATE));
    position + Duration::from_millis(40) >= pcm_end
}

/// Sound joins only outside a seek: starting it while the picture still
/// catches up is what played audio ahead of the image.
fn audio_may_play(active: &Active) -> bool {
    active.playing && !active.seeking && active.shown != Duration::MAX
}

/// A clip is finished only after a picture has been shown and the clock has
/// reached its end. Decoder startup must not consume a short clip first.
fn ready_to_finish(shown: Duration, position: Duration, total: Duration) -> bool {
    shown != Duration::MAX && position >= total
}

/// Presentation time to paint: the newest buffered frame due at position.
/// When none is due yet the current picture holds instead of flashing a
/// future frame early; None means nothing has shown and the poster or the
/// spinner stays up.
fn choose_pts(
    times: impl Iterator<Item = Duration> + Clone,
    position: Duration,
    shown: Duration,
) -> Option<Duration> {
    let mut due = None;
    for pts in times.clone() {
        if pts <= position {
            due = Some(pts);
        }
    }
    if due.is_some() {
        return due;
    }
    for pts in times {
        if pts == shown {
            return Some(shown);
        }
    }
    None
}

/// What a full frame buffer does with one more arrival. The buffer only
/// sheds frames already behind playback, so a buffer full of future frames
/// holds the arrival back (it stays queued, the decoder waits) instead of
/// dropping the future and freezing or jumping the picture later.
enum BufferRoom {
    Push,
    EvictThenPush,
    Hold,
}

fn buffer_room(
    len: usize,
    front: Option<Duration>,
    position: Duration,
    limit: usize,
) -> BufferRoom {
    if len < limit {
        return BufferRoom::Push;
    }
    if len > 1 && front.is_some_and(|pts| pts + KEEP_BEHIND < position) {
        return BufferRoom::EvictThenPush;
    }
    BufferRoom::Hold
}

/// How soon the viewer needs another frame.
fn next_repaint(active: &Active, position: Duration) -> Duration {
    active
        .buffered
        .iter()
        .find(|frame| frame.pts > position)
        .map(|frame| frame.pts - position)
        .unwrap_or(Duration::from_millis(100))
        .clamp(Duration::from_millis(10), Duration::from_millis(500))
}

/// The soundtrack from a position, or silence when the file has none.
/// What opening the soundtrack at a position gives.
enum Audio {
    /// Sound from the asked position.
    Sound((rodio::MixerDeviceSink, rodio::Player)),
    /// Sound on its way: ffmpeg decodes in the background while the picture
    /// plays muted, joining wherever playback stands when it arrives.
    Extracting(std::sync::mpsc::Receiver<Vec<f32>>),
    /// No soundtrack to play; the wall clock drives the picture.
    Silent,
}
/// What a jump's background soundtrack task answers. Only the newest jump
/// may apply it; older tasks die with their generation.
enum SeekAudio {
    /// Streaming sound opened at the target, still paused.
    Stream((rodio::MixerDeviceSink, rodio::Player)),
    /// A full extraction on its way; it joins at the live position.
    Extracting(std::sync::mpsc::Receiver<Vec<f32>>),
    /// No soundtrack to play; the wall clock drives the picture.
    Silent,
}
/// The soundtrack from a position, or why it starts elsewhere.
fn audio_at(path: &Path, at: Duration, generation: &Arc<AtomicU64>, current: u64) -> Audio {
    use rodio::Source;
    let Some(file) = std::fs::File::open(path).ok() else {
        return Audio::Silent;
    };
    let mut decoder = match rodio::Decoder::new(BufReader::new(file)) {
        Ok(decoder) => decoder,
        // Some layouts (trailing metadata, odd streams) defeat the streaming
        // decoder. Those fall back to a background extraction instead of
        // going quiet, and the reason is logged for the bug report.
        Err(error) => {
            log::warn!(
                "soundtrack not streaming from {}: {error:?}; extracting",
                path.display()
            );
            return extract_audio(path, generation.clone(), current);
        }
    };
    if !at.is_zero() && decoder.try_seek(at).is_err() {
        // The streaming decoder cannot land on the jump: the picture keeps
        // its target and the soundtrack joins from a background extraction
        // instead of silently restarting the clip from zero.
        log::warn!("soundtrack cannot seek in {}; extracting", path.display());
        return extract_audio(path, generation.clone(), current);
    }
    // The player opens a fresh sink on every seek, so rodio's drop notice
    // would print on each one; the sink is dropped on purpose here.
    let Some(mut device) = rodio::DeviceSinkBuilder::open_default_sink().ok() else {
        return Audio::Silent;
    };
    device.log_on_drop(false);
    let sink = rodio::Player::connect_new(device.mixer());
    sink.append(decoder);
    sink.pause();
    Audio::Sound((device, sink))
}
/// Opens streaming sound at a jump target off the interface thread. Only
/// the newest jump may apply the answer; anything older is refused before
/// it can move the clock.
fn open_seek_audio(
    path: &Path,
    at: Duration,
    generation: &Arc<AtomicU64>,
    current: u64,
) -> SeekAudio {
    use rodio::Source;
    let alive = || generation.load(Ordering::SeqCst) == current;
    let Some(file) = std::fs::File::open(path).ok() else {
        return SeekAudio::Silent;
    };
    let mut decoder = match rodio::Decoder::new(BufReader::new(file)) {
        Ok(decoder) => decoder,
        Err(error) => {
            log::warn!(
                "soundtrack not streaming from {}: {error:?}; extracting",
                path.display()
            );
            return SeekAudio::Extracting(extract_rx(path, generation.clone(), current));
        }
    };
    if !at.is_zero() && decoder.try_seek(at).is_err() {
        log::warn!("soundtrack cannot seek in {}; extracting", path.display());
        return SeekAudio::Extracting(extract_rx(path, generation.clone(), current));
    }
    let Some(mut device) = rodio::DeviceSinkBuilder::open_default_sink().ok() else {
        return SeekAudio::Silent;
    };
    device.log_on_drop(false);
    let sink = rodio::Player::connect_new(device.mixer());
    sink.append(decoder);
    sink.pause();
    if !alive() {
        return SeekAudio::Silent;
    }
    SeekAudio::Stream((device, sink))
}

/// Sidecar of decoded samples beside the video, reused while the file is unchanged.
pub(crate) fn soundtrack_sidecar(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_default();
    name.push(".pcm");
    path.with_file_name(name)
}

fn load_soundtrack(path: &Path) -> Option<Vec<f32>> {
    let bytes = std::fs::read(soundtrack_sidecar(path)).ok()?;
    if bytes.len() < 24 || &bytes[..4] != b"VPCM" {
        return None;
    }
    let len = u64::from_le_bytes(bytes[4..12].try_into().ok()?);
    let stored = u64::from_le_bytes(bytes[12..20].try_into().ok()?);
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta
        .modified()
        .ok()
        .and_then(|when| when.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|age| age.as_secs())
        .unwrap_or(0);
    if meta.len() != len || modified != stored {
        return None;
    }
    let count = u32::from_le_bytes(bytes[20..24].try_into().ok()?) as usize;
    if bytes.len() != 24 + count * 4 {
        return None;
    }
    let mut samples = Vec::with_capacity(count);
    let (chunks, _) = bytes[24..].as_chunks::<4>();
    for chunk in chunks {
        samples.push(f32::from_le_bytes(*chunk));
    }
    Some(samples)
}

fn store_soundtrack(path: &Path, samples: &[f32]) {
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    let modified = meta
        .modified()
        .ok()
        .and_then(|when| when.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|age| age.as_secs())
        .unwrap_or(0);
    let mut bytes = Vec::with_capacity(24 + samples.len() * 4);
    bytes.extend_from_slice(b"VPCM");
    bytes.extend_from_slice(&meta.len().to_le_bytes());
    bytes.extend_from_slice(&modified.to_le_bytes());
    bytes.extend_from_slice(&(samples.len() as u32).to_le_bytes());
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    let _ = std::fs::write(soundtrack_sidecar(path), bytes);
}

/// Decoded soundtrack, from the sidecar when the video file is unchanged.
fn soundtrack(path: &Path, alive: &dyn Fn() -> bool) -> Option<Vec<f32>> {
    if let Some(pcm) = load_soundtrack(path) {
        log::info!("soundtrack reused");
        return Some(pcm);
    }
    let pcm = symphonia_pcm(path, alive).or_else(|| ffmpeg_pcm(path, alive))?;
    if pcm.is_empty() || !alive() {
        return None;
    }
    store_soundtrack(path, &pcm);
    log::info!("soundtrack extracted");
    Some(pcm)
}

/// Starts a background extraction of the whole soundtrack, or silence.
/// The thread stands down as soon as a newer jump retires it, instead of
/// decoding a file nobody is watching anymore.
fn extract_rx(
    path: &Path,
    generation: Arc<AtomicU64>,
    current: u64,
) -> std::sync::mpsc::Receiver<Vec<f32>> {
    let (tx, rx) = std::sync::mpsc::channel();
    let path = path.to_path_buf();
    let _ = std::thread::Builder::new()
        .name("video-audio".into())
        .spawn(move || {
            let alive = || generation.load(Ordering::SeqCst) == current;
            // In-process first so sound works without any external binary;
            // ffmpeg stays only as a last resort for exotic codecs.
            if let Some(pcm) = soundtrack(&path, &alive)
                && alive()
            {
                let _ = tx.send(pcm);
            } else if !alive() {
                log::debug!("soundtrack extraction retired for {}", path.display());
            } else {
                log::warn!("soundtrack not decodable from {}", path.display());
            }
        });
    rx
}
/// Starts a background extraction of the whole soundtrack, or silence.
fn extract_audio(path: &Path, generation: Arc<AtomicU64>, current: u64) -> Audio {
    let (tx, rx) = std::sync::mpsc::channel();
    let path = path.to_path_buf();
    let _ = std::thread::Builder::new()
        .name("video-audio".into())
        .spawn(move || {
            let alive = || generation.load(Ordering::SeqCst) == current;
            // In-process first so sound works without any external binary;
            // ffmpeg stays only as a last resort for exotic codecs.
            if let Some(pcm) = soundtrack(&path, &alive)
                && alive()
            {
                let _ = tx.send(pcm);
            } else if !alive() {
                log::debug!("soundtrack extraction retired for {}", path.display());
            } else {
                log::warn!("soundtrack not decodable from {}", path.display());
            }
        });
    Audio::Extracting(rx)
}
/// Whether ffmpeg can extract what the streaming decoder cannot.
fn ffmpeg_present() -> bool {
    static KNOWN: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *KNOWN.get_or_init(|| {
        let mut check = std::process::Command::new("ffmpeg");
        quiet(&mut check)
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    })
}

/// A helper media process that never flashes a console window on Windows.
fn quiet(command: &mut std::process::Command) -> &mut std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: probing a chat video must not pop a console.
        command.creation_flags(0x0800_0000);
    }
    command
}
/// Decodes a soundtrack to mono 48 kHz samples, capped for chat videos.
/// Decodes a soundtrack in-process with symphonia, so sound works without
/// any external binary. Resamples to mono 48 kHz with a linear pass and
/// caps at five minutes for chat videos. Returns `None` when the file has
/// no decodable audio track.
/// The thread stands down as soon as a newer jump retires it.
fn symphonia_pcm(path: &Path, alive: &dyn Fn() -> bool) -> Option<Vec<f32>> {
    let file = std::fs::File::open(path).ok()?;
    let source = symphonia::core::io::MediaSourceStream::new(
        Box::new(file) as Box<dyn symphonia::core::io::MediaSource>,
        Default::default(),
    );
    let probe = symphonia::default::get_probe();
    let mut probed = probe
        .format(
            &Default::default(),
            source,
            &Default::default(),
            &Default::default(),
        )
        .ok()?;
    let track = probed
        .format
        .tracks()
        .iter()
        .find(|track| {
            track.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL
                && matches!(
                    track.codec_params.codec,
                    symphonia::core::codecs::CODEC_TYPE_AAC
                        | symphonia::core::codecs::CODEC_TYPE_MP3
                        | symphonia::core::codecs::CODEC_TYPE_VORBIS
                        | symphonia::core::codecs::CODEC_TYPE_PCM_S16LE
                        | symphonia::core::codecs::CODEC_TYPE_PCM_S24LE
                        | symphonia::core::codecs::CODEC_TYPE_PCM_S32LE
                        | symphonia::core::codecs::CODEC_TYPE_PCM_F32LE
                        | symphonia::core::codecs::CODEC_TYPE_PCM_F64LE
                )
        })
        .or_else(|| {
            probed.format.default_track().filter(|track| {
                track.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL
            })
        })?;
    let (track_id, rate) = (track.id, track.codec_params.sample_rate.unwrap_or(PCM_RATE));
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &Default::default())
        .ok()?;
    let cap = PCM_RATE as usize * PCM_CAP_SECS as usize;
    let mut mono = Vec::new();
    let mut capped = false;
    loop {
        if !alive() {
            return None;
        }
        if mono.len() >= cap {
            capped = true;
            break;
        }
        let packet = match probed.format.next_packet() {
            Ok(packet) => packet,
            Err(_) => break,
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            Err(_) => continue,
        };
        append_mono(&mut mono, &decoded, cap);
    }
    if mono.is_empty() {
        return None;
    }
    if capped {
        // Chat videos longer than the cap keep their picture; the clock
        // falls back to the wall clock once this sound runs out.
        log::warn!("soundtrack of {} capped at {PCM_CAP_SECS}s", path.display());
    }
    // Resample only when the source rate differs; a linear pass is plenty
    // for a chat soundtrack and keeps this dependency-free.
    if rate == PCM_RATE {
        return Some(mono);
    }
    Some(resample_linear(&mono, rate, PCM_RATE))
}
/// Appends one decoded audio buffer as mono f32 samples, capped.
fn append_mono(out: &mut Vec<f32>, decoded: &symphonia::core::audio::AudioBufferRef, cap: usize) {
    use symphonia::core::audio::{AudioBuffer, Signal};
    if out.len() >= cap || decoded.frames() == 0 {
        return;
    }
    let spec = *decoded.spec();
    let mut float = AudioBuffer::<f32>::new(decoded.frames() as u64, spec);
    decoded.convert(&mut float);
    let channels = spec.channels.count().max(1);
    for i in 0..decoded.frames() {
        if out.len() >= cap {
            break;
        }
        let mut sum = 0.0f32;
        for channel in 0..channels {
            sum += float.chan(channel)[i];
        }
        out.push(sum / channels as f32);
    }
}
/// Linear resampling between sample rates, for chat soundtracks.
fn resample_linear(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if input.is_empty() || from == 0 || to == 0 || from == to {
        return input.to_vec();
    }
    let duration = input.len() as f64 / f64::from(from);
    let mut output = Vec::with_capacity((duration * f64::from(to)) as usize);
    let cap = PCM_RATE as usize * PCM_CAP_SECS as usize;
    let len = ((duration * f64::from(to)) as usize).min(cap);
    for i in 0..len {
        let pos = i as f64 * f64::from(from) / f64::from(to);
        let base = pos.floor() as usize;
        let frac = (pos - base as f64) as f32;
        let first = *input.get(base).unwrap_or(&0.0);
        let second = *input.get(base + 1).unwrap_or(&first);
        output.push(first + (second - first) * frac);
    }
    output
}
fn ffmpeg_pcm(path: &Path, alive: &dyn Fn() -> bool) -> Option<Vec<f32>> {
    use std::io::Read;
    let mut launch = std::process::Command::new("ffmpeg");
    let mut child = quiet(&mut launch)
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-vn", "-ar", "48000", "-ac", "1", "-f", "f32le", "pipe:1"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        // No pipe to read: stop the helper instead of leaving it running.
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
    };
    // Streamed, so a long file never sits whole in RAM: past the cap the
    // helper is stopped instead of decoding the tail for nothing.
    let cap = PCM_RATE as usize * PCM_CAP_SECS as usize;
    let mut pcm = Vec::new();
    // A pipe read can split a four-byte sample anywhere; the tail waits
    // for the next read instead of being dropped and misaligning the rest.
    let mut pending: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 32_768];
    loop {
        if !alive() {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        if pcm.len() >= cap {
            break;
        }
        let read = match stdout.read(&mut chunk) {
            Ok(read) => read,
            Err(_) => break,
        };
        if read == 0 {
            break;
        }
        push_pcm(&mut pcm, &mut pending, &chunk[..read]);
        if pcm.len() >= cap {
            pcm.truncate(cap);
            break;
        }
    }
    let _ = child.kill();
    let status = child.wait().ok()?;
    if !status.success() && pcm.is_empty() {
        return None;
    }
    (!pcm.is_empty()).then_some(pcm)
}

/// Pushes stream bytes into samples, keeping one to three leftover bytes
/// for the next read. Pipe reads split samples anywhere; dropping the tail
/// would misalign every sample after it into noise.
fn push_pcm(pcm: &mut Vec<f32>, pending: &mut Vec<u8>, bytes: &[u8]) {
    pending.extend_from_slice(bytes);
    let samples = pending.len() / 4;
    for chunk in pending[..samples * 4].as_chunks::<4>().0 {
        pcm.push(f32::from_le_bytes(*chunk));
    }
    pending.drain(..samples * 4);
}
/// Plays cached samples from a position on a fresh output, if any.
fn attach_cached(active: &mut Active, volume: f32, muted: bool, from: Duration) {
    let Some(pcm) = active.pcm.clone() else {
        return;
    };
    let skip = (from.as_secs_f32() * PCM_RATE as f32) as usize;
    let Some(mut device) = rodio::DeviceSinkBuilder::open_default_sink().ok() else {
        return;
    };
    device.log_on_drop(false);
    let sink = rodio::Player::connect_new(device.mixer());
    sink.append(MemSamples::from_pcm(pcm, skip));
    sink.set_volume(if muted { 0.0 } else { volume.clamp(0.0, 1.0) });
    if !audio_may_play(active) {
        sink.pause();
    }
    active.audio = Some((device, sink));
    active.base = from;
    active.anchor = Duration::ZERO;
    active.started = Instant::now();
}

/// Points the open decoder at `target` and retires pictures from the old pass.
fn note_seek(active: &mut Active, target: Duration) -> u64 {
    active.control.set_target(target);
    let current = active.generation.fetch_add(1, Ordering::SeqCst) + 1;
    while active.frames.try_recv().is_ok() {}
    active.buffered.clear();
    active.held = None;
    active.shown = Duration::MAX;
    active.decode_done = false;
    active.finished = false;
    active.seeking = !target.is_zero();
    active.fallback_used = false;
    active.decode_error = None;
    active.landed_pts = None;
    active.trailing = None;
    active.seek_diag = if target.is_zero() {
        None
    } else {
        Some(SeekDiag {
            requested: Instant::now(),
            target,
            first_frame: None,
            audio_ready: None,
        })
    };
    active.control.set_paused(!active.playing);
    current
}

enum SessionEnd {
    Stopped,
    Switch,
}

/// One decode thread for the open video. Seeks reuse it. Returns this
/// player's worker number so tests can prove a player kept its own
/// decoder without reading the process-global counter other tests move.
#[allow(clippy::too_many_arguments)]
fn spawn_worker(
    path: PathBuf,
    total: Duration,
    width: u32,
    height: u32,
    generation: Arc<AtomicU64>,
    control: Arc<PlaybackControl>,
    out: SyncSender<DecodeMsg>,
) -> u64 {
    let worker = PLAYBACK_WORKERS.fetch_add(1, Ordering::Relaxed) + 1;
    let _ = std::thread::Builder::new()
        .name("video-decode".into())
        .spawn(move || worker_main(path, total, width, height, generation, control, out));
    worker
}

fn worker_main(
    path: PathBuf,
    total: Duration,
    width: u32,
    height: u32,
    generation: Arc<AtomicU64>,
    control: Arc<PlaybackControl>,
    out: SyncSender<DecodeMsg>,
) {
    if !control.ffmpeg.load(Ordering::SeqCst) {
        #[cfg(windows)]
        if let Some(mut decoder) = open_playback_decoder(&path) {
            let _engine = EngineGuard::enter();
            let end = mf_session(&mut decoder, total, &generation, &control, &out);
            drop(decoder);
            drop(_engine);
            if matches!(end, SessionEnd::Stopped) {
                return;
            }
        }
    }
    if !control.ffmpeg.load(Ordering::SeqCst)
        && matches!(
            openh264_session_loop(&path, total, width, height, &generation, &control, &out),
            SessionEnd::Stopped
        )
    {
        return;
    }
    ffmpeg_session_loop(&path, total, width, height, &generation, &control, &out);
}

#[cfg(windows)]
fn open_playback_decoder(path: &Path) -> Option<crate::native_video::Decoder> {
    let file = std::fs::File::open(path).ok()?;
    crate::native_video::Decoder::open(Box::new(file)).ok()
}

fn pass_alive(control: &PlaybackControl, generation: &AtomicU64, pass: u64) -> bool {
    !control.stopped()
        && !control.ffmpeg.load(Ordering::SeqCst)
        && generation.load(Ordering::SeqCst) == pass
}

#[cfg(windows)]
fn mf_session(
    decoder: &mut crate::native_video::Decoder,
    total: Duration,
    generation: &AtomicU64,
    control: &PlaybackControl,
    out: &SyncSender<DecodeMsg>,
) -> SessionEnd {
    loop {
        if control.stopped() {
            return SessionEnd::Stopped;
        }
        if control.ffmpeg.load(Ordering::SeqCst) {
            return SessionEnd::Switch;
        }
        let pass = generation.load(Ordering::SeqCst);
        FRAME_GENERATION.with(|cell| cell.set(pass));
        let target = control.target().min(total);
        if generation.load(Ordering::SeqCst) != pass {
            continue;
        }
        match drive_media_foundation(decoder, target, generation, pass, control, out) {
            SessionEnd::Switch => return SessionEnd::Switch,
            SessionEnd::Stopped => {
                if control.stopped() || control.ffmpeg.load(Ordering::SeqCst) {
                    return if control.stopped() {
                        SessionEnd::Stopped
                    } else {
                        SessionEnd::Switch
                    };
                }
                if generation.load(Ordering::SeqCst) != pass {
                    continue;
                }
                control.wait_for_work(generation, pass);
            }
        }
    }
}

#[cfg(windows)]
fn drive_media_foundation(
    decoder: &mut crate::native_video::Decoder,
    mut target: Duration,
    generation: &AtomicU64,
    pass: u64,
    control: &PlaybackControl,
    out: &SyncSender<DecodeMsg>,
) -> SessionEnd {
    use crate::native_video::Sample;
    use std::collections::VecDeque;

    struct NativeAudioGuard<'a>(&'a PlaybackControl);
    impl<'a> Drop for NativeAudioGuard<'a> {
        fn drop(&mut self) {
            self.0.set_native_audio(false);
            self.0.clear_clock();
        }
    }
    let _audio_guard = NativeAudioGuard(control);

    let alive = || pass_alive(control, generation, pass);
    let info = decoder.info();
    let seconds = target.as_secs_f64().clamp(0.0, info.duration);
    if decoder.seek(seconds).is_err() {
        control.request_ffmpeg();
        return SessionEnd::Switch;
    }
    if !alive() {
        return SessionEnd::Stopped;
    }

    let audio_position = Arc::new(AtomicU64::new(0));
    let eof = Arc::new(AtomicBool::new(false));
    let failed = Arc::new(AtomicBool::new(false));
    let mut output = if info.sample_rate > 0 {
        crate::video_output::open(
            info.sample_rate,
            crate::video_output::Controls {
                cancelled: Arc::new(AtomicBool::new(false)),
                paused: control.paused.clone(),
                seek: control.seek_ms.clone(),
                volume: control.volume.clone(),
                position: audio_position.clone(),
                eof: eof.clone(),
                failed: failed.clone(),
            },
        )
        .ok()
    } else {
        None
    };

    let has_audio = output.is_some() && info.sample_rate > 0;
    control.set_native_audio(has_audio);
    if has_audio {
        control.set_clock(target);
    } else {
        control.clear_clock();
    }

    let mut landed = false;
    let mut early: Option<Frame> = None;
    let mut produced = 0u64;
    let mut frames_queue: VecDeque<Frame> = VecDeque::new();
    let mut pending_audio: Option<(Vec<[f32; 2]>, usize)> = None;
    let mut video_done = false;
    let mut audio_done = output.is_none();
    let mut wall = target;
    let mut last_tick = Instant::now();

    loop {
        if !alive() {
            return SessionEnd::Stopped;
        }

        let seek_req = control.seek_ms.swap(u64::MAX, Ordering::AcqRel);
        if seek_req != u64::MAX {
            drop(output);
            target = Duration::from_millis(seek_req).min(Duration::from_secs_f64(info.duration));
            let sec = target.as_secs_f64().clamp(0.0, info.duration);
            if decoder.seek(sec).is_err() {
                control.request_ffmpeg();
                return SessionEnd::Switch;
            }
            audio_position.store(0, Ordering::Release);
            eof.store(false, Ordering::Release);
            failed.store(false, Ordering::Release);
            output = if info.sample_rate > 0 {
                crate::video_output::open(
                    info.sample_rate,
                    crate::video_output::Controls {
                        cancelled: Arc::new(AtomicBool::new(false)),
                        paused: control.paused.clone(),
                        seek: control.seek_ms.clone(),
                        volume: control.volume.clone(),
                        position: audio_position.clone(),
                        eof: eof.clone(),
                        failed: failed.clone(),
                    },
                )
                .ok()
            } else {
                None
            };
            landed = false;
            early = None;
            frames_queue.clear();
            pending_audio = None;
            video_done = false;
            audio_done = output.is_none();
            wall = target;
            last_tick = Instant::now();
            let has_audio = output.is_some() && info.sample_rate > 0;
            control.set_native_audio(has_audio);
            if has_audio {
                control.set_clock(target);
            } else {
                control.clear_clock();
            }
            continue;
        }

        if landed {
            control.block_while_paused(generation, pass);
            if !alive() {
                return SessionEnd::Stopped;
            }
        }

        let now = Instant::now();
        let elapsed = now.duration_since(last_tick);
        last_tick = now;

        let paused = control.paused.load(Ordering::Acquire);
        if !paused && landed {
            wall += elapsed;
        }

        if failed.load(Ordering::Acquire) && output.is_some() {
            output = None;
            audio_done = true;
            pending_audio = None;
            control.clear_clock();
            control.set_native_audio(false);
        }

        let clock_pos = if output.is_some() && !audio_done && info.sample_rate > 0 {
            let audio_secs = target.as_secs_f64()
                + audio_position.load(Ordering::Acquire) as f64 / f64::from(info.sample_rate);
            let pos = Duration::from_secs_f64(audio_secs);
            wall = pos;
            control.set_clock(pos);
            pos
        } else {
            control.clear_clock();
            wall
        };

        // Feed pending audio to output
        if let Some((samples, offset)) = &mut pending_audio
            && let Some(out_dev) = output.as_mut()
        {
            while *offset < samples.len() && out_dev.producer.push(samples[*offset]).is_ok() {
                *offset += 1;
            }
            if *offset == samples.len() {
                pending_audio = None;
            }
        }

        // Poll audio track if space in producer
        if !audio_done && !paused && pending_audio.is_none() {
            match decoder.read_audio() {
                Ok(Some(Sample::Audio { pts: _, frames })) => {
                    if let Some(out_dev) = output.as_mut() {
                        let mut offset = 0;
                        while offset < frames.len() && out_dev.producer.push(frames[offset]).is_ok()
                        {
                            offset += 1;
                        }
                        if offset < frames.len() {
                            pending_audio = Some((frames, offset));
                        }
                    }
                }
                Ok(Some(Sample::Video { .. })) => {}
                Ok(None) => {
                    audio_done = true;
                    eof.store(true, Ordering::Release);
                }
                Err(_) => {
                    audio_done = true;
                }
            }
        }

        // Poll video track if queue < 4
        if !video_done && frames_queue.len() < 4 {
            match decoder.read_video() {
                Ok(Some(Sample::Video {
                    pts,
                    width,
                    height,
                    rgba,
                })) => {
                    if let Some(frame) = rgba_frame(pts, width, height, &rgba) {
                        frames_queue.push_back(frame);
                    }
                }
                Ok(Some(Sample::Audio { .. })) => {}
                Ok(None) => {
                    video_done = true;
                }
                Err(reason) => {
                    let _ = send_decode(
                        out,
                        &alive,
                        DecodeMsg::Error(
                            DecodeError {
                                engine: "media-foundation",
                                reason: reason.to_owned(),
                                produced,
                                samples: produced,
                            },
                            terminal_generation(),
                        ),
                    );
                    control.request_ffmpeg();
                    return SessionEnd::Switch;
                }
            }
        }

        // Deliver ready frame
        let mut frame_to_send = None;
        while let Some(front) = frames_queue.front() {
            let pts = front.pts;
            if !landed {
                if frame_reaches_seek(pts, target) {
                    landed = true;
                    frame_to_send = frames_queue.pop_front();
                    break;
                } else {
                    early = frames_queue.pop_front();
                }
            } else if pts <= clock_pos + Duration::from_millis(20) {
                // If there's a subsequent frame also past clock_pos, drop this late frame
                let is_late = frames_queue
                    .get(1)
                    .is_some_and(|next| next.pts <= clock_pos);
                let popped = frames_queue.pop_front();
                if !is_late {
                    frame_to_send = popped;
                    break;
                }
            } else {
                break;
            }
        }

        if let Some(frame) = frame_to_send {
            produced += 1;
            if send_frame(out, &alive, frame).is_err() {
                return SessionEnd::Stopped;
            }
        }

        if video_done && frames_queue.is_empty() && (audio_done || output.is_none()) {
            if !landed
                && let Some(frame) = early.take()
                && send_frame(out, &alive, frame).is_err()
            {
                return SessionEnd::Stopped;
            }
            let _ = send_decode(out, &alive, DecodeMsg::End(terminal_generation()));
            return SessionEnd::Stopped;
        }

        if paused {
            std::thread::sleep(Duration::from_millis(15));
        } else if frames_queue.len() >= 2 {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

fn openh264_session_loop(
    path: &Path,
    total: Duration,
    _width: u32,
    _height: u32,
    generation: &AtomicU64,
    control: &PlaybackControl,
    out: &SyncSender<DecodeMsg>,
) -> SessionEnd {
    loop {
        if control.stopped() {
            return SessionEnd::Stopped;
        }
        if control.ffmpeg.load(Ordering::SeqCst) {
            return SessionEnd::Switch;
        }
        let pass = generation.load(Ordering::SeqCst);
        FRAME_GENERATION.with(|cell| cell.set(pass));
        let target = control.target().min(total);
        if generation.load(Ordering::SeqCst) != pass {
            continue;
        }
        let alive = || pass_alive(control, generation, pass);
        decode(path, target, total, &alive, out, control, generation, pass);
        if control.stopped() {
            return SessionEnd::Stopped;
        }
        if control.ffmpeg.load(Ordering::SeqCst) {
            return SessionEnd::Switch;
        }
        if generation.load(Ordering::SeqCst) != pass {
            continue;
        }
        control.wait_for_work(generation, pass);
    }
}

fn ffmpeg_session_loop(
    path: &Path,
    total: Duration,
    width: u32,
    height: u32,
    generation: &AtomicU64,
    control: &PlaybackControl,
    out: &SyncSender<DecodeMsg>,
) {
    loop {
        if control.stopped() {
            return;
        }
        let pass = generation.load(Ordering::SeqCst);
        FRAME_GENERATION.with(|cell| cell.set(pass));
        let target = control.target().min(total);
        let alive = || !control.stopped() && generation.load(Ordering::SeqCst) == pass;
        decode_ffmpeg(
            path, target, width, height, &alive, out, control, generation, pass,
        );
        if control.stopped() {
            return;
        }
        if generation.load(Ordering::SeqCst) != pass {
            continue;
        }
        control.wait_for_work(generation, pass);
    }
}
/// How far before the target the input `-ss` may jump. Five seconds of
/// decode after a keyframe is enough for B-frame reordering, and keeps a
/// long jump from scanning the whole file.
const FFMPEG_INPUT_PREROLL: Duration = Duration::from_secs(5);

/// Coarse input seek and fine output seek that together land on `at`.
fn ffmpeg_seek_offsets(at: Duration) -> (Duration, Duration) {
    let coarse = at.saturating_sub(FFMPEG_INPUT_PREROLL);
    (coarse, at - coarse)
}

fn detect_fps(path: &Path) -> f64 {
    if let Ok(rate) = ffprobe(
        path,
        [
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=r_frame_rate",
            "-of",
            "csv=p=0",
        ],
    ) {
        let parts: Vec<&str> = rate.trim().split('/').collect();
        if parts.len() == 2 {
            if let (Ok(num), Ok(den)) = (parts[0].parse::<f64>(), parts[1].parse::<f64>())
                && den > 0.0
                && num > 0.0
            {
                let fps = num / den;
                if fps.is_finite() && fps > 1.0 && fps <= 240.0 {
                    return fps;
                }
            }
        } else if let Ok(fps) = rate.trim().parse::<f64>()
            && fps.is_finite()
            && fps > 1.0
            && fps <= 240.0
        {
            return fps;
        }
    }
    30.0
}

/// Pulls frames through ffmpeg for files the in-process decoder cannot read.
///
/// A coarse `-ss` before `-i` jumps to a nearby keyframe; a fine `-ss`
/// after `-i` then decodes to the requested presentation time. The first
/// piped frame is stamped `at`, so the viewer clock does not depend on
/// ffmpeg's output timestamps.
#[allow(clippy::too_many_arguments)]
fn decode_ffmpeg(
    path: &Path,
    at: Duration,
    width: u32,
    height: u32,
    alive: &dyn Fn() -> bool,
    out: &SyncSender<DecodeMsg>,
    control: &PlaybackControl,
    generation: &AtomicU64,
    pass: u64,
) {
    let (out_width, out_height) = playback_limit(width, height);
    let (coarse, fine) = ffmpeg_seek_offsets(at);
    let mut launch = std::process::Command::new("ffmpeg");
    let launch = quiet(&mut launch).args(["-v", "error"]);
    if !coarse.is_zero() {
        launch.args(["-ss", &format!("{:.6}", coarse.as_secs_f64())]);
    }
    launch.args(["-i"]).arg(path);
    if !fine.is_zero() {
        launch.args(["-ss", &format!("{:.6}", fine.as_secs_f64())]);
    }
    let fps = detect_fps(path);
    let mut args: Vec<std::ffi::OsString> = Vec::new();
    if out_width < width || out_height < height {
        args.push("-vf".into());
        args.push(format!("scale={out_width}:{out_height}:flags=lanczos").into());
    }
    args.extend([
        "-an".into(),
        "-f".into(),
        "rawvideo".into(),
        "-pix_fmt".into(),
        "rgba".into(),
        "pipe:1".into(),
    ]);
    let mut child = match launch
        .args(&args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => {
            fail_decode(out, "ffmpeg", "could not start", 0, 0);
            return;
        }
    };
    let mut stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            fail_decode(out, "ffmpeg", "could not start", 0, 0);
            return;
        }
    };
    use std::io::Read;
    let frame_bytes = (out_width * out_height * 4) as usize;
    let mut buffer = vec![0u8; frame_bytes];
    let mut index = 0u64;
    let mut produced = false;
    loop {
        if !alive() {
            break;
        }
        if produced {
            control.block_while_paused(generation, pass);
            if !alive() {
                break;
            }
        }
        if stdout.read_exact(&mut buffer).is_err() {
            // Pipe dry: normal end of stream, not a failure.
            send_control(out, DecodeMsg::End(terminal_generation()));
            break;
        }
        if index == 0 && !at.is_zero() {
            log::debug!(
                target: "vespera::video",
                "ffmpeg decode first frame: start={}ms",
                at.as_millis()
            );
        }
        let pts = at + Duration::from_secs_f64(index as f64 / fps);
        index += 1;
        produced = true;
        let image =
            ColorImage::from_rgba_unmultiplied([out_width as usize, out_height as usize], &buffer);
        if send_frame(
            out,
            alive,
            Frame {
                pts,
                image,
                generation: 0,
            },
        )
        .is_err()
        {
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(windows)]
fn rgba_frame(pts: f64, width: u32, height: u32, rgba: &[u8]) -> Option<Frame> {
    if width == 0 || height == 0 || rgba.len() < width as usize * height as usize * 4 {
        return None;
    }
    let (out_width, out_height) = playback_limit(width, height);
    let pts = if pts.is_finite() && pts >= 0.0 {
        Duration::from_secs_f64(pts)
    } else {
        Duration::ZERO
    };
    // Media Foundation already negotiated this size. Building the image
    // straight from the sample skips the extra buffer the resizer needed.
    if out_width == width && out_height == height {
        return Some(Frame {
            pts,
            generation: 0,
            image: ColorImage::from_rgba_unmultiplied([width as usize, height as usize], rgba),
        });
    }
    let image = image::RgbaImage::from_raw(width, height, rgba.to_vec())?;
    let scaled = image::imageops::resize(
        &image,
        out_width,
        out_height,
        image::imageops::FilterType::Triangle,
    );
    Some(Frame {
        pts,
        generation: 0,
        image: ColorImage::from_rgba_unmultiplied(
            [scaled.width() as usize, scaled.height() as usize],
            scaled.as_raw(),
        ),
    })
}

#[allow(clippy::too_many_arguments)]
fn decode(
    path: &Path,
    at: Duration,
    total: Duration,
    alive: &dyn Fn() -> bool,
    out: &SyncSender<DecodeMsg>,
    control: &PlaybackControl,
    generation: &AtomicU64,
    pass: u64,
) {
    // Media Foundation, when it can open the file, owns the reader for the
    // whole playback. This pass is the OpenH264 fallback on that same thread.
    // The session stays held until the pass returns, so a scrub that still
    // needs OpenH264 waits instead of stalling the next picture.
    let _session = openh264_session();
    let _engine = EngineGuard::enter();
    let Ok(file) = std::fs::File::open(path) else {
        return;
    };
    let Ok(size) = file.metadata().map(|meta| meta.len()) else {
        return;
    };
    let Ok(mut mp4) = mp4::Mp4Reader::read_header(BufReader::new(file), size) else {
        return;
    };
    let Some(track) = mp4
        .tracks()
        .values()
        .find(|track| track.track_type().ok() == Some(mp4::TrackType::Video))
    else {
        return;
    };
    let (track_id, timescale) = (track.track_id(), u64::from(track.timescale().max(1)));
    // Fragmented files list their samples per fragment; the header count stays empty.
    let count = mp4.sample_count(track_id).unwrap_or(0);
    let Ok(sps) = track.sequence_parameter_set().map(|bytes| bytes.to_vec()) else {
        // Media Foundation declined the file and OpenH264 has no setup
        // either. Say so, so the viewer can hand the clip to ffmpeg.
        fail_decode(out, "in-process", "no decoder setup", 0, 0);
        return;
    };
    let Ok(pps) = track.picture_parameter_set().map(|bytes| bytes.to_vec()) else {
        fail_decode(out, "in-process", "no decoder setup", 0, 0);
        return;
    };
    let target = at.min(total);
    let start_sample = first_sample_at(&mut mp4, track_id, timescale, target, total);
    let mut decoder = match openh264::decoder::Decoder::new() {
        Ok(decoder) => decoder,
        Err(_) => return,
    };
    let mut parameters = Vec::new();
    push_annex_b(&mut parameters, &sps);
    push_annex_b(&mut parameters, &pps);
    let _ = decoder.decode(&parameters);
    let mut pending: VecDeque<Duration> = VecDeque::new();
    // The last timestamp handed to the viewer. Flushed frames carry no time
    // of their own, so they inherit this instead of falling back to zero.
    let mut sent = target;
    // Presentation times queue in decode order, which matches the decoder's
    // output order for the baseline encodes WhatsApp sends (no B-frames).
    // Pictures before the target stay in the decoder (the keyframe is
    // earlier) and are not sent. The final sample still passes: a jump at
    // the very end would otherwise decode zero frames.
    // Consecutive undecodable samples before the file reads as broken
    // instead of corrupt in one spot; samples read with zero pictures out
    // before a stall reads the same way.
    let mut consecutive_errors: u64 = 0;
    let mut produced: u64 = 0;
    let mut samples: u64 = 0;
    // Length prefix size audited against the first sample, never assumed.
    let mut length_size: Option<usize> = None;
    for sample_id in start_sample..=count {
        if !alive() {
            return;
        }
        if produced > 0 {
            control.block_while_paused(generation, pass);
            if !alive() {
                return;
            }
        }
        let Ok(Some(sample)) = mp4.read_sample(track_id, sample_id) else {
            break;
        };
        samples += 1;
        let size = match length_size {
            Some(size) => size,
            None => match avcc_length_size(&sample.bytes) {
                Some(size) => {
                    length_size = Some(size);
                    size
                }
                // Not length-prefixed units we understand: fail loudly so
                // the fallback path takes over instead of feeding garbage.
                None => {
                    fail_decode(
                        out,
                        "in-process",
                        "unreadable sample units",
                        produced,
                        samples,
                    );
                    return;
                }
            },
        };
        let pts = stamp(sample.start_time, sample.rendering_offset, timescale);
        pending.push_back(pts);
        let mut annex_b = Vec::with_capacity(sample.bytes.len() + 16);
        let converted = avcc_to_annex_b(&mut annex_b, &sample.bytes, size);
        let decoded = converted
            .ok()
            .and_then(|()| decoder.decode(&annex_b).ok().flatten());
        let Some(yuv) = decoded else {
            // A failed sample loses its timestamp: keeping it would stamp
            // every later picture with an older time.
            pending.pop_front();
            consecutive_errors += 1;
            if consecutive_errors > MAX_CONSECUTIVE_DECODE_ERRORS
                || (produced == 0 && samples >= MAX_SAMPLES_WITHOUT_PICTURE)
            {
                fail_decode(
                    out,
                    "in-process",
                    "decoder stopped producing pictures",
                    produced,
                    samples,
                );
                return;
            }
            continue;
        };
        consecutive_errors = 0;
        // Frames delayed by the decoder keep their presentation order.
        if let Some(delay) = pending.pop_front()
            && let Some(frame) = frame_of(&yuv, delay)
            && (frame_reaches_seek(delay, target) || sample_id == count)
        {
            sent = sent.max(delay);
            produced += 1;
            if send_frame(out, alive, frame).is_err() {
                return;
            }
        }
    }
    if !alive() {
        return;
    }
    if let Ok(rest) = decoder.flush_remaining() {
        let mut rest = rest.iter().peekable();
        while let Some(yuv) = rest.next() {
            if !alive() {
                return;
            }
            let delay = pending.pop_front().unwrap_or(sent);
            // The very last picture still goes out when the jump aimed
            // past it; the viewer settles it as the finished state.
            let last = rest.peek().is_none() && pending.is_empty();
            if let Some(frame) = frame_of(yuv, delay)
                && (frame_reaches_seek(delay, target) || last)
                && send_frame(out, alive, frame).is_err()
            {
                return;
            }
        }
    }
    if produced == 0 && samples > 0 {
        // Every sample read, zero pictures out: the file is valid enough
        // to list samples but undecodable here (seen on a VFR re-encode
        // openh264 silently misses). Fail loudly so the single controlled
        // ffmpeg fallback runs instead of refusing a playable file.
        fail_decode(
            out,
            "in-process",
            "decoder produced no pictures",
            produced,
            samples,
        );
        return;
    }
    send_control(out, DecodeMsg::End(terminal_generation()));
}

/// Samples a seek may search, either side of its estimate. A chat encode
/// puts a key frame every couple of seconds, so a few hundred samples cover
/// it several times over.
const SEEK_WINDOW: u32 = 400;
/// The 1-based key frame a seek should start from, in a table of start time
/// and key-frame flag pairs in time order: the last one at or before the
/// target. `None` when the table holds none at or before it.
///
/// A decode has to begin on a key frame: starting on a delta frame feeds the
/// decoder pictures it cannot reconstruct, and the timestamps after it line
/// up wrong, which is what made a jump play as a stutter.
fn pick_keyframe(table: &[(u64, bool)], target_units: u64) -> Option<u32> {
    let mut before = None;
    for (index, entry) in table.iter().enumerate() {
        if !entry.1 {
            continue;
        }
        if entry.0 <= target_units {
            before = Some(index as u32 + 1);
        } else {
            // Past the target already, and time order means nothing later
            // can sit behind it: widen the search instead of opening ahead
            // of the target, where the frames to show could never decode.
            return before;
        }
    }
    before
}
/// Reads the samples of one search window, in time order.
///
/// Reads stop at the first key frame past the target: nothing later can
/// change the answer, and every sample read loads its bytes from the file.
fn sample_window(
    mp4: &mut mp4::Mp4Reader<BufReader<std::fs::File>>,
    track_id: u32,
    from: u32,
    span: u32,
    count: u32,
    target_units: u64,
) -> Vec<(u64, bool)> {
    let mut table = Vec::with_capacity(span as usize);
    let last = from.saturating_add(span).min(count);
    let mut sample_id = from.max(1);
    while sample_id <= last {
        let Ok(Some(sample)) = mp4.read_sample(track_id, sample_id) else {
            break;
        };
        table.push((sample.start_time, sample.is_sync));
        if sample.is_sync && sample.start_time > target_units {
            break;
        }
        sample_id += 1;
    }
    table
}
/// The sample to start decoding at: a real key frame at or before the target
/// when one is near, so the decoder enters cleanly without replaying the
/// whole file.
///
/// The estimate comes from where the target sits in the clip, because
/// walking the sample table from the start loads every sample's bytes and
/// made a jump feel like a fast forward. The window widens only for files
/// with an unusually long key-frame interval.
fn first_sample_at(
    mp4: &mut mp4::Mp4Reader<BufReader<std::fs::File>>,
    track_id: u32,
    timescale: u64,
    target: Duration,
    total: Duration,
) -> u32 {
    if target.is_zero() {
        return 1;
    }
    let count = mp4.sample_count(track_id).unwrap_or(0);
    if count == 0 {
        return 1;
    }
    let target_units = (target.as_secs_f64() * timescale.max(1) as f64) as u64;
    let fraction = if total.is_zero() {
        0.0
    } else {
        (target.as_secs_f64() / total.as_secs_f64()).clamp(0.0, 1.0)
    };
    let guess = ((fraction * count as f64) as u32).clamp(1, count);
    for span in [SEEK_WINDOW, SEEK_WINDOW * 3, SEEK_WINDOW * 9] {
        let from = guess.saturating_sub(span).max(1);
        let table = sample_window(mp4, track_id, from, span * 2, count, target_units);
        if let Some(picked) = pick_keyframe(&table, target_units) {
            return from + picked - 1;
        }
    }
    // A track with no key frame anywhere decodes from the top.
    1
}

/// Presentation time of a sample, honouring the composition offset that
/// reorders frames.
fn stamp(start_time: u64, rendering_offset: i32, timescale: u64) -> Duration {
    let units = start_time as i64 + i64::from(rendering_offset);
    Duration::from_secs_f64((units.max(0) as f64) / timescale.max(1) as f64)
}

/// The playback pass a terminal message belongs to. Every decode session
/// stamps its current pass in `FRAME_GENERATION` before sending, so a
/// seek that retires the old pass can tell its `End`/`Error` apart from
/// the new pass still decoding.
fn terminal_generation() -> u64 {
    FRAME_GENERATION.with(|cell| cell.get())
}

/// Sends a frame, waiting briefly when the viewer fell behind, and giving up
/// when a newer playback replaced this one.
fn send_frame(
    out: &SyncSender<DecodeMsg>,
    alive: &dyn Fn() -> bool,
    mut frame: Frame,
) -> Result<(), ()> {
    frame.generation = FRAME_GENERATION.with(|cell| cell.get());
    send_decode(out, alive, DecodeMsg::Frame(frame))
}

/// Sends one decode message, waiting briefly when the viewer fell behind,
/// and giving up when a newer playback replaced this one.
fn send_decode(
    out: &SyncSender<DecodeMsg>,
    alive: &dyn Fn() -> bool,
    mut message: DecodeMsg,
) -> Result<(), ()> {
    loop {
        if !alive() {
            return Err(());
        }
        match out.try_send(message) {
            Ok(()) => return Ok(()),
            Err(std::sync::mpsc::TrySendError::Full(taken)) => {
                // Only pictures wait: a terminal state behind a full queue
                // still drains through the frames ahead of it.
                let DecodeMsg::Frame(frame) = taken else {
                    return Err(());
                };
                message = DecodeMsg::Frame(frame);
                std::thread::sleep(Duration::from_millis(30));
            }
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => return Err(()),
        }
    }
}

/// Sends a terminal state without waiting: frames still queued drain
/// first, and a gone viewer ends the task through the dropped channel.
fn send_control(out: &SyncSender<DecodeMsg>, message: DecodeMsg) {
    let _ = out.try_send(message);
}

/// Reports a loud decode failure: the viewer falls back to ffmpeg or
/// refuses with this reason instead of mistaking silence for loading.
fn fail_decode(
    out: &SyncSender<DecodeMsg>,
    engine: &'static str,
    reason: &str,
    produced: u64,
    samples: u64,
) {
    send_control(
        out,
        DecodeMsg::Error(
            DecodeError {
                engine,
                reason: reason.to_owned(),
                produced,
                samples,
            },
            terminal_generation(),
        ),
    );
}

/// Consecutive undecodable samples before the file reads as broken instead
/// of corrupt in one spot.
const MAX_CONSECUTIVE_DECODE_ERRORS: u64 = 32;
/// Samples read with zero pictures out before a stall reads as failure.
const MAX_SAMPLES_WITHOUT_PICTURE: u64 = 240;

fn push_annex_b(out: &mut Vec<u8>, nal: &[u8]) {
    out.extend_from_slice(&[0, 0, 0, 1]);
    out.extend_from_slice(nal);
}

/// NAL length sizes AVCC tracks may declare. Four bytes dominate; one and
/// two exist in the wild and must not misparse as four.
const AVCC_LENGTH_SIZES: [usize; 3] = [4, 2, 1];

/// Whether `size`-prefixed NAL units parse cleanly: every length positive,
/// inside the rest, consuming the sample exactly.
fn avcc_parses(sample: &[u8], size: usize) -> bool {
    if sample.is_empty() || size == 0 || size > 4 {
        return false;
    }
    let mut rest = sample;
    let mut units = 0;
    while !rest.is_empty() {
        if rest.len() < size {
            return false;
        }
        let mut length = 0usize;
        for byte in &rest[..size] {
            length = (length << 8) | usize::from(*byte);
        }
        rest = &rest[size..];
        if length == 0 || length > rest.len() {
            return false;
        }
        rest = &rest[length..];
        units += 1;
    }
    units > 0
}

/// The declared length size, audited against the first sample instead of
/// assumed: the first size that parses it exactly wins, four first.
fn avcc_length_size(sample: &[u8]) -> Option<usize> {
    AVCC_LENGTH_SIZES
        .into_iter()
        .find(|size| avcc_parses(sample, *size))
}

/// Converts length-prefixed AVCC NAL units to Annex B start codes with the
/// audited length size. Malformed units are an explicit error, never a
/// silent partial picture.
fn avcc_to_annex_b(out: &mut Vec<u8>, sample: &[u8], length_size: usize) -> Result<(), ()> {
    if sample.is_empty() {
        return Ok(());
    }
    let mut rest = sample;
    while !rest.is_empty() {
        if rest.len() < length_size {
            return Err(());
        }
        let mut length = 0usize;
        for byte in &rest[..length_size] {
            length = (length << 8) | usize::from(*byte);
        }
        rest = &rest[length_size..];
        if length == 0 || length > rest.len() {
            return Err(());
        }
        push_annex_b(out, &rest[..length]);
        rest = &rest[length..];
    }
    Ok(())
}

/// Converts and scales one decoded preview frame.
/// Nearest keeps a drag thumbnail responsive: Triangle on a 1080p source
/// dominates preview latency, while the definitive jump still uses
/// frame_of with Triangle at full playback width.
fn frame_of_preview(yuv: &openh264::decoder::DecodedYUV<'_>, pts: Duration) -> Option<Frame> {
    use openh264::formats::YUVSource;
    let (width, height) = yuv.dimensions();
    if width == 0 || height == 0 {
        return None;
    }
    let mut rgba = vec![0u8; width * height * 4];
    yuv.write_rgba8(&mut rgba);
    let image = image::RgbaImage::from_raw(width as u32, height as u32, rgba)?;
    let out_width = (width as u32).min(PREVIEW_WIDTH);
    let out_height = ((height as u64 * u64::from(out_width) / width as u64) as u32).max(1);
    let scaled = if out_width == width as u32 {
        image
    } else {
        image::imageops::resize(
            &image,
            out_width,
            out_height,
            image::imageops::FilterType::Nearest,
        )
    };
    Some(Frame {
        pts,
        generation: 0,
        image: ColorImage::from_rgba_unmultiplied(
            [scaled.width() as usize, scaled.height() as usize],
            scaled.as_raw(),
        ),
    })
}

/// Converts and scales one decoded frame.
fn frame_of(yuv: &openh264::decoder::DecodedYUV<'_>, pts: Duration) -> Option<Frame> {
    use openh264::formats::YUVSource;
    let (width, height) = yuv.dimensions();
    if width == 0 || height == 0 {
        return None;
    }
    let mut rgba = vec![0u8; width * height * 4];
    yuv.write_rgba8(&mut rgba);
    let image = image::RgbaImage::from_raw(width as u32, height as u32, rgba)?;
    let (out_width, out_height) = playback_limit(width as u32, height as u32);
    let scaled = if out_width == width as u32 && out_height == height as u32 {
        image
    } else {
        image::imageops::resize(
            &image,
            out_width,
            out_height,
            image::imageops::FilterType::Triangle,
        )
    };
    Some(Frame {
        pts,
        generation: 0,
        image: ColorImage::from_rgba_unmultiplied(
            [scaled.width() as usize, scaled.height() as usize],
            scaled.as_raw(),
        ),
    })
}

/// How long a drag waits before asking for another preview while one is
/// already flying. Motion between two answers replaces the pending
/// target instead of queueing behind it.
const PREVIEW_THROTTLE: Duration = Duration::from_millis(60);
/// Preview pictures remembered per video: enough for scrubbing back and
/// forth without decoding twice, small enough to stay out of the way.
const PREVIEW_CACHE_ENTRIES: usize = 8;
/// Byte budget for cached previews: eight 320-wide pictures fit several
/// times over, leaving headroom for 720p/1080p thumbnails without crowding
/// playback textures.
const PREVIEW_CACHE_BYTES: usize = 6 * 1024 * 1024;
/// Samples decoded per preview at most: a keyframe search plus margin,
/// never the whole file however far the keyframes spread.
const PREVIEW_MAX_SAMPLES: u32 = 1500;

/// One decoded preview picture with the presentation time it shows.
/// In-process decode only, so B-frame pictures are the nearest the fast
/// path produces rather than ffmpeg-exact; the definitive jump still
/// lands through the full player path.
/// Stage timings for one preview decode, in milliseconds.
/// open covers file open plus MP4 header and track setup; index covers the
/// keyframe search; decode covers sample reads plus H.264 decode; resize
/// covers RGBA conversion plus downscale to PREVIEW_WIDTH. Total spans open
/// through the exact picture. Approx marks when the first picture was ready.
#[derive(Clone, Debug, Default)]
struct PreviewTimings {
    open_ms: u128,
    index_ms: u128,
    decode_ms: u128,
    resize_ms: u128,
    total_ms: u128,
    approx_ms: Option<u128>,
    samples: u32,
    width: u32,
    height: u32,
}
/// One staged preview: an early approximate picture plus the exact picture
/// near the drag target. The approximate is the first decoded picture after
/// the keyframe; the exact is the first picture at or past the target. When
/// the first picture already meets the target, approximate is None.
#[derive(Debug)]
struct StagedPreview {
    approx: Option<PreviewImage>,
    exact: PreviewImage,
    timings: PreviewTimings,
}
#[derive(Debug)]
struct PreviewImage {
    pts: Duration,
    image: ColorImage,
    samples: u32,
}

/// Decodes one picture near the target without touching the player:
/// real timestamps from the container, keyframe-aware start, bounded
/// sample count. Never spawns ffmpeg; failure keeps the last picture.
/// Staged preview decode with timings, an early approximate, and abort checks.
/// Never spawns ffmpeg; failure keeps the last picture. A fresh decoder per
/// call keeps reference frames correct: reusing a decoder across targets of
/// the same file would carry the old position refs into the new keyframe.
/// One thumbnail from a Media Foundation reader that is not the playback
/// decoder and does not take the OpenH264 lock.
///
/// The reader seeks to the keyframe behind the target, which on a chat encode
/// can be seconds away, so it reads forward under the shared sample budget
/// and only claims to be exact once it reaches the target; otherwise it hands
/// the question to the in-process path instead of painting a picture that
/// reads as the chosen one.
#[cfg(windows)]
fn preview_media_foundation(
    path: &Path,
    target: Duration,
    should_abort: &dyn Fn() -> bool,
    aborted_approx: &mut Option<PreviewImage>,
) -> Result<StagedPreview, String> {
    use crate::native_video::Sample;
    let started = Instant::now();
    let file = std::fs::File::open(path).map_err(|_| "Could not open the video".to_owned())?;
    let mut decoder = crate::native_video::Decoder::open(Box::new(file))
        .map_err(|_| "Media Foundation could not open the video".to_owned())?;
    if !target.is_zero() {
        decoder
            .seek(target.as_secs_f64())
            .map_err(|_| "This video cannot seek to that position.".to_owned())?;
    }
    let mut early: Option<PreviewImage> = None;
    let mut early_ms: Option<u128> = None;
    let mut samples = 0u32;
    loop {
        if samples >= PREVIEW_MAX_SAMPLES {
            break;
        }
        // Same handover rule as the in-process path: a newer drag target may
        // take over only once the early picture exists, and that picture goes
        // out with the abort. Aborting before it would answer nothing at all
        // and leave the next identical request to abort this one too.
        if early.is_some() && samples.is_multiple_of(8) && should_abort() {
            *aborted_approx = early;
            return Err("aborted".to_owned());
        }
        match decoder.read_video() {
            Ok(Some(Sample::Video {
                pts,
                width,
                height,
                rgba,
            })) => {
                samples += 1;
                let stamp = if pts.is_finite() && pts >= 0.0 {
                    Duration::from_secs_f64(pts)
                } else {
                    Duration::ZERO
                };
                let reached = frame_reaches_seek(stamp, target);
                // Only the first picture and the one at the target pay the
                // conversion; the deltas between them just advance the reader.
                if early.is_some() && !reached {
                    continue;
                }
                let Some(image) = scale_preview(width, height, &rgba) else {
                    continue;
                };
                let picture = PreviewImage {
                    pts: stamp,
                    image,
                    samples,
                };
                if reached {
                    let approx = early.take().filter(|early| early.pts != stamp);
                    let approx_ms = early_ms.filter(|_| approx.is_some());
                    return Ok(StagedPreview {
                        approx,
                        exact: picture,
                        timings: PreviewTimings {
                            total_ms: started.elapsed().as_millis(),
                            samples,
                            approx_ms,
                            ..PreviewTimings::default()
                        },
                    });
                }
                early_ms = Some(started.elapsed().as_millis());
                early = Some(picture);
            }
            Ok(Some(Sample::Audio { .. })) => {}
            Ok(None) => break,
            Err(reason) => return Err(reason.to_owned()),
        }
    }
    let Some(exact) = early else {
        return Err("The video has no picture".to_owned());
    };
    if !frame_reaches_seek(exact.pts, target) {
        return Err("Media Foundation stopped short of the target".to_owned());
    }
    Ok(StagedPreview {
        approx: None,
        exact,
        timings: PreviewTimings {
            total_ms: started.elapsed().as_millis(),
            samples,
            ..PreviewTimings::default()
        },
    })
}

#[cfg(windows)]
fn scale_preview(width: u32, height: u32, rgba: &[u8]) -> Option<ColorImage> {
    if width == 0 || height == 0 || rgba.len() < width as usize * height as usize * 4 {
        return None;
    }
    let image = image::RgbaImage::from_raw(width, height, rgba.to_vec())?;
    if width <= PREVIEW_WIDTH {
        return Some(ColorImage::from_rgba_unmultiplied(
            [width as usize, height as usize],
            image.as_raw(),
        ));
    }
    let out_width = PREVIEW_WIDTH;
    let out_height = ((u64::from(height) * u64::from(out_width) / u64::from(width)) as u32).max(1);
    let scaled = image::imageops::resize(
        &image,
        out_width,
        out_height,
        image::imageops::FilterType::Triangle,
    );
    Some(ColorImage::from_rgba_unmultiplied(
        [scaled.width() as usize, scaled.height() as usize],
        scaled.as_raw(),
    ))
}

fn preview_staged(
    path: &Path,
    target: Duration,
    total: Duration,
    should_abort: &dyn Fn() -> bool,
    aborted_approx: &mut Option<PreviewImage>,
) -> Result<StagedPreview, String> {
    #[cfg(windows)]
    match preview_media_foundation(path, target, should_abort, aborted_approx) {
        Ok(staged) => return Ok(staged),
        Err(reason) if reason == "aborted" => return Err(reason),
        Err(_) => {}
    }
    let _session = openh264_session();
    let total_start = Instant::now();
    let open_start = Instant::now();
    let file =
        std::fs::File::open(path).map_err(|error| format!("Could not open the video: {error}"))?;
    let size = file
        .metadata()
        .map_err(|error| format!("Could not read the video: {error}"))?
        .len();
    let mut mp4 = mp4::Mp4Reader::read_header(BufReader::new(file), size)
        .map_err(|error| format!("Could not read the video: {error}"))?;
    let Some(track) = mp4
        .tracks()
        .values()
        .find(|track| track.track_type().ok() == Some(mp4::TrackType::Video))
    else {
        return Err("The video has no picture track".to_owned());
    };
    let (track_id, timescale) = (track.track_id(), u64::from(track.timescale().max(1)));
    let count = mp4.sample_count(track_id).unwrap_or(0);
    if count == 0 {
        return Err("The video has no samples".to_owned());
    }
    let Ok(sps) = track.sequence_parameter_set().map(|bytes| bytes.to_vec()) else {
        return Err("The video has no decoder setup".to_owned());
    };
    let Ok(pps) = track.picture_parameter_set().map(|bytes| bytes.to_vec()) else {
        return Err("The video has no decoder setup".to_owned());
    };
    let open_ms = open_start.elapsed().as_millis();
    let index_start = Instant::now();
    let start_sample = first_sample_at(&mut mp4, track_id, timescale, target, total);
    let index_ms = index_start.elapsed().as_millis();
    let mut decoder =
        openh264::decoder::Decoder::new().map_err(|_| "The decoder would not start".to_owned())?;
    let mut parameters = Vec::new();
    push_annex_b(&mut parameters, &sps);
    push_annex_b(&mut parameters, &pps);
    let _ = decoder.decode(&parameters);
    let mut pending: VecDeque<Duration> = VecDeque::new();
    let mut length_size: Option<usize> = None;
    let mut approx: Option<PreviewImage> = None;
    let mut approx_ms: Option<u128> = None;
    let mut best: Option<Frame> = None;
    let mut samples: u32 = 0;
    let mut decode_ms: u128 = 0;
    let mut resize_ms: u128 = 0;
    for sample_id in start_sample..=count {
        if samples >= PREVIEW_MAX_SAMPLES {
            break;
        }
        // Newer drag targets abort here, but only after the early approximate
        // exists: aborting before the first picture would discard the very
        // feedback the skip path is required to hand over, and the keyframe
        // this decode starts from yields that picture quickly.
        if approx.is_some() && samples.is_multiple_of(8) && should_abort() {
            *aborted_approx = approx;
            return Err("aborted".to_owned());
        }
        let Ok(Some(sample)) = mp4.read_sample(track_id, sample_id) else {
            break;
        };
        samples += 1;
        let size = match length_size {
            Some(size) => size,
            None => match avcc_length_size(&sample.bytes) {
                Some(size) => {
                    length_size = Some(size);
                    size
                }
                None => return Err("The video uses unknown sample units".to_owned()),
            },
        };
        let pts = stamp(sample.start_time, sample.rendering_offset, timescale);
        pending.push_back(pts);
        let mut annex_b = Vec::with_capacity(sample.bytes.len() + 16);
        if avcc_to_annex_b(&mut annex_b, &sample.bytes, size).is_err() {
            pending.pop_front();
            continue;
        }
        let decode_start = Instant::now();
        let decoded = decoder.decode(&annex_b).ok().flatten();
        decode_ms += decode_start.elapsed().as_millis();
        let Some(yuv) = decoded else {
            pending.pop_front();
            continue;
        };
        let Some(delay) = pending.pop_front() else {
            continue;
        };
        // Only the first picture (approximate) and the target picture (exact)
        // pay RGBA conversion plus downscale: intermediate deltas only advance
        // the decoder refs and never paint. Resizing every delta dominated
        // preview latency, especially on 720p/1080p sources.
        let is_first = approx.is_none();
        let is_target = delay >= target;
        if !is_first && !is_target {
            continue;
        }
        let resize_start = Instant::now();
        let framed = frame_of_preview(&yuv, delay);
        resize_ms += resize_start.elapsed().as_millis();
        let Some(frame) = framed else {
            continue;
        };
        if is_first {
            approx_ms = Some(total_start.elapsed().as_millis());
            approx = Some(PreviewImage {
                pts: frame.pts,
                image: frame.image.clone(),
                samples,
            });
        }
        best = Some(frame);
        if delay >= target {
            break;
        }
    }
    let Some(frame) = best else {
        return Err("No picture near the target".to_owned());
    };
    let total_ms = total_start.elapsed().as_millis();
    let width = frame.image.width() as u32;
    let height = frame.image.height() as u32;
    let exact = PreviewImage {
        pts: frame.pts,
        image: frame.image,
        samples,
    };
    // When the first picture already meets the target, there is no earlier
    // approximate to show: send only the exact to avoid a duplicate paint.
    if let Some(first) = approx.as_ref()
        && first.pts == exact.pts
    {
        approx = None;
        approx_ms = None;
    }
    let timings = PreviewTimings {
        open_ms,
        index_ms,
        decode_ms,
        resize_ms,
        total_ms,
        approx_ms,
        samples,
        width,
        height,
    };
    Ok(StagedPreview {
        approx,
        exact,
        timings,
    })
}
// staged helper end
#[cfg(test)]
fn preview_frame(path: &Path, target: Duration, total: Duration) -> Result<PreviewImage, String> {
    let _session = openh264_session();
    let file =
        std::fs::File::open(path).map_err(|error| format!("Could not open the video: {error}"))?;
    let size = file
        .metadata()
        .map_err(|error| format!("Could not read the video: {error}"))?
        .len();
    let mut mp4 = mp4::Mp4Reader::read_header(BufReader::new(file), size)
        .map_err(|error| format!("Could not read the video: {error}"))?;
    let Some(track) = mp4
        .tracks()
        .values()
        .find(|track| track.track_type().ok() == Some(mp4::TrackType::Video))
    else {
        return Err("The video has no picture track".to_owned());
    };
    let (track_id, timescale) = (track.track_id(), u64::from(track.timescale().max(1)));
    let count = mp4.sample_count(track_id).unwrap_or(0);
    if count == 0 {
        return Err("The video has no samples".to_owned());
    }
    let Ok(sps) = track.sequence_parameter_set().map(|bytes| bytes.to_vec()) else {
        return Err("The video has no decoder setup".to_owned());
    };
    let Ok(pps) = track.picture_parameter_set().map(|bytes| bytes.to_vec()) else {
        return Err("The video has no decoder setup".to_owned());
    };
    let start_sample = first_sample_at(&mut mp4, track_id, timescale, target, total);
    let mut decoder =
        openh264::decoder::Decoder::new().map_err(|_| "The decoder would not start".to_owned())?;
    let mut parameters = Vec::new();
    push_annex_b(&mut parameters, &sps);
    push_annex_b(&mut parameters, &pps);
    let _ = decoder.decode(&parameters);
    let mut pending: VecDeque<Duration> = VecDeque::new();
    let mut length_size: Option<usize> = None;
    let mut best: Option<Frame> = None;
    let mut samples: u32 = 0;
    for sample_id in start_sample..=count {
        if samples >= PREVIEW_MAX_SAMPLES {
            break;
        }
        let Ok(Some(sample)) = mp4.read_sample(track_id, sample_id) else {
            break;
        };
        samples += 1;
        let size = match length_size {
            Some(size) => size,
            None => match avcc_length_size(&sample.bytes) {
                Some(size) => {
                    length_size = Some(size);
                    size
                }
                None => return Err("The video uses unknown sample units".to_owned()),
            },
        };
        let pts = stamp(sample.start_time, sample.rendering_offset, timescale);
        pending.push_back(pts);
        let mut annex_b = Vec::with_capacity(sample.bytes.len() + 16);
        if avcc_to_annex_b(&mut annex_b, &sample.bytes, size).is_err() {
            pending.pop_front();
            continue;
        }
        let decoded = decoder.decode(&annex_b).ok().flatten();
        let Some(yuv) = decoded else {
            pending.pop_front();
            continue;
        };
        let Some(delay) = pending.pop_front() else {
            continue;
        };
        let Some(frame) = frame_of_preview(&yuv, delay) else {
            continue;
        };
        best = Some(frame);
        if delay >= target {
            break;
        }
    }
    best.map(|frame| PreviewImage {
        pts: frame.pts,
        image: frame.image,
        samples,
    })
    .ok_or_else(|| "No picture near the target".to_owned())
}

/// One scrub request: the newest queued request always wins, older ones
/// never decode.
struct PreviewRequest {
    path: PathBuf,
    generation: u64,
    seq: u64,
    fraction: f32,
    total: Duration,
}

/// One answered preview, identified by the video, generation and
/// request it was made for. Anything older never paints.
pub struct PreviewReady {
    pub path: PathBuf,
    pub generation: u64,
    pub seq: u64,
    pub fraction: f32,
    pub pts: Duration,
    pub image: ColorImage,
    pub samples: u32,
    /// True for the early keyframe picture; false for the exact picture near
    /// the drag target. Approximate paints fast while the exact travels.
    pub approximate: bool,
}

/// Scrub preview decoder: one background thread, latest request wins,
/// small byte-budgeted cache. Decode runs off the interface thread and
/// in-process only, so a drag never spawns an ffmpeg process per move.
/// The definitive jump still goes through the full player path.
#[derive(Default)]
pub struct Previewer {
    tx: Option<std::sync::mpsc::Sender<PreviewRequest>>,
    rx: Option<std::sync::mpsc::Receiver<PreviewReady>>,
    seq: u64,
    generations: std::collections::HashMap<PathBuf, u64>,
    in_flight: Option<u64>,
    pending: Option<PreviewRequest>,
    /// Exact picture that arrived in the same drain as its approximate.
    /// The next poll delivers it so the approximate still paints first.
    deferred: Option<PreviewReady>,
    last_send: Option<Instant>,
    cache: std::collections::HashMap<(PathBuf, u64, u32), (PreviewReady, usize)>,
    order: VecDeque<(PathBuf, u64, u32)>,
    cached_bytes: usize,
}

impl Previewer {
    /// Starts a drag generation for one video: older answers die on
    /// arrival from here on.
    pub fn begin(&mut self, path: &Path) -> u64 {
        let generation = self.generations.get(path).copied().unwrap_or(0) + 1;
        self.generations.insert(path.to_path_buf(), generation);
        self.pending = None;
        self.in_flight = None;
        self.deferred = None;
        generation
    }

    /// Cancels everything pending for one video: the next answer
    /// carries a generation nobody waits for anymore.
    pub fn cancel(&mut self, path: &Path) {
        self.begin(path);
        if let Some(rx) = &self.rx {
            while rx.try_recv().is_ok() {}
        }
    }

    /// Asks for the picture at a drag fraction, throttled while one
    /// flies. A cache hit answers at once without touching the thread.
    /// Returns the cached picture when the target was decoded before.
    pub fn request(
        &mut self,
        path: &Path,
        generation: u64,
        fraction: f32,
        total: Duration,
    ) -> Option<PreviewReady> {
        let bucket = (fraction.clamp(0.0, 1.0) * 100.0) as u32;
        let hit = self
            .cache
            .get(&(path.to_path_buf(), generation, bucket))
            .map(|(ready, _)| PreviewReady {
                path: ready.path.clone(),
                generation: ready.generation,
                seq: ready.seq,
                fraction: ready.fraction,
                pts: ready.pts,
                image: ready.image.clone(),
                samples: ready.samples,
                approximate: ready.approximate,
            });
        if let Some(ready) = hit {
            self.touch(path, generation, bucket);
            return Some(ready);
        }
        self.seq += 1;
        self.pending = Some(PreviewRequest {
            path: path.to_path_buf(),
            generation,
            seq: self.seq,
            fraction: fraction.clamp(0.0, 1.0),
            total,
        });
        self.maybe_send();
        None
    }

    /// Collects the newest answer for this video and generation,
    /// dropping anything older, and launches the pending target.
    pub fn poll(&mut self, path: &Path, generation: u64) -> Option<PreviewReady> {
        if let Some(ready) = self.deferred.take() {
            let ours = ready.path == path && ready.generation == generation && !ready.approximate;
            if ours {
                return self.take_exact(path, generation, ready);
            }
            self.deferred = Some(ready);
        }
        let mut approx: Option<PreviewReady> = None;
        let mut exact: Option<PreviewReady> = None;
        if let Some(rx) = &self.rx {
            while let Ok(ready) = rx.try_recv() {
                if ready.path != path || ready.generation != generation {
                    continue;
                }
                if ready.approximate {
                    if approx
                        .as_ref()
                        .is_none_or(|known: &PreviewReady| ready.seq >= known.seq)
                    {
                        approx = Some(ready);
                    }
                } else if exact
                    .as_ref()
                    .is_none_or(|known: &PreviewReady| ready.seq >= known.seq)
                {
                    exact = Some(ready);
                }
            }
        }
        if let (Some(a), Some(e)) = (&approx, &exact)
            && a.seq < e.seq
        {
            approx = None;
        }
        if let Some(approx) = approx {
            match exact {
                Some(exact) if exact.seq == approx.seq => self.deferred = Some(exact),
                Some(exact) if exact.seq > approx.seq => {
                    return self.take_exact(path, generation, exact);
                }
                _ => {}
            }
            self.maybe_send();
            return Some(approx);
        }
        if let Some(ready) = exact {
            return self.take_exact(path, generation, ready);
        }
        self.maybe_send();
        None
    }

    fn take_exact(
        &mut self,
        path: &Path,
        generation: u64,
        ready: PreviewReady,
    ) -> Option<PreviewReady> {
        if self.in_flight == Some(ready.seq) {
            self.in_flight = None;
        }
        self.remember(ready);
        self.maybe_send();
        self.latest(path, generation)
    }

    /// Byte size of one cached answer.
    fn answer_bytes(ready: &PreviewReady) -> usize {
        ready.image.pixels.len() * 4
    }

    /// Files one answer under its request bucket, evicting oldest
    /// first while entries or bytes exceed budget.
    fn remember(&mut self, ready: PreviewReady) {
        if ready.approximate {
            return;
        }
        let bucket = (ready.fraction.clamp(0.0, 1.0) * 100.0) as u32;
        let key = (ready.path.clone(), ready.generation, bucket);
        let bytes = Self::answer_bytes(&ready);
        if let Some((_, old)) = self.cache.insert(key.clone(), (ready, bytes)) {
            self.cached_bytes = self.cached_bytes.saturating_sub(old);
            self.order.retain(|known| known != &key);
        }
        self.order.push_back(key);
        self.cached_bytes += bytes;
        while self.order.len() > PREVIEW_CACHE_ENTRIES || self.cached_bytes > PREVIEW_CACHE_BYTES {
            let Some(old) = self.order.pop_front() else {
                break;
            };
            if let Some((_, bytes)) = self.cache.remove(&old) {
                self.cached_bytes = self.cached_bytes.saturating_sub(bytes);
            }
        }
    }

    /// Newest cached answer for this video and generation, if any.
    fn latest(&mut self, path: &Path, generation: u64) -> Option<PreviewReady> {
        let key = self
            .order
            .iter()
            .rev()
            .find(|(known, generation_no, _)| known == path && *generation_no == generation)
            .cloned();
        key.and_then(|key| {
            self.touch(path, generation, key.2);
            self.cache.get(&key).map(|(ready, _)| PreviewReady {
                path: ready.path.clone(),
                generation: ready.generation,
                seq: ready.seq,
                fraction: ready.fraction,
                pts: ready.pts,
                image: ready.image.clone(),
                samples: ready.samples,
                approximate: ready.approximate,
            })
        })
    }

    /// Marks a cached bucket newest without holding two borrows.
    fn touch(&mut self, path: &Path, generation: u64, bucket: u32) {
        let key = (path.to_path_buf(), generation, bucket);
        self.order.retain(|known| known != &key);
        self.order.push_back(key);
    }

    /// Sends the pending target when the worker is free or the last
    /// send aged past the throttle. The worker always drains to the
    /// newest queued request before decoding.
    fn maybe_send(&mut self) {
        let due = self
            .last_send
            .is_none_or(|at| at.elapsed() >= PREVIEW_THROTTLE);
        if self.pending.is_none() || (self.in_flight.is_some() && !due) {
            return;
        }
        if self.tx.is_none() {
            let (tx, rx) = std::sync::mpsc::channel();
            let (back, fore) = std::sync::mpsc::channel();
            self.tx = Some(tx);
            self.rx = Some(fore);
            std::thread::Builder::new()
                .name("video-preview".into())
                .spawn(move || preview_worker(rx, back))
                .ok();
        }
        let Some(request) = self.pending.take() else {
            return;
        };
        let seq = request.seq;
        let stale_sender = self.tx.take();
        if let Some(tx) = stale_sender {
            match tx.send(request) {
                Ok(()) => {
                    self.tx = Some(tx);
                    self.in_flight = Some(seq);
                    self.last_send = Some(Instant::now());
                }
                Err(_) => {
                    self.tx = None;
                    self.in_flight = None;
                }
            }
        }
    }
}

/// Decodes the newest queued request only: motion between two answers
/// replaces the pending target instead of queueing behind it. Staged
/// decode sends an early keyframe approximate while the exact travels;
/// a newer drag target aborts the stale decode so a long GOP never
/// starves the drag. Approximate never touches the exact-picture cache.
fn preview_worker(
    rx: std::sync::mpsc::Receiver<PreviewRequest>,
    tx: std::sync::mpsc::Sender<PreviewReady>,
) {
    use std::cell::RefCell;
    while let Ok(first) = rx.recv() {
        let mut request = first;
        while let Ok(newer) = rx.try_recv() {
            request = newer;
        }
        loop {
            let stash: RefCell<Option<PreviewRequest>> = RefCell::new(None);
            let should_abort = || match rx.try_recv() {
                Ok(newer) => {
                    *stash.borrow_mut() = Some(newer);
                    true
                }
                Err(_) => false,
            };
            let target = request.total.mul_f32(request.fraction);
            let mut aborted_approx = None;
            let staged = preview_staged(
                &request.path,
                target,
                request.total,
                &should_abort,
                &mut aborted_approx,
            );
            let mut newest = stash.take();
            while let Ok(newer) = rx.try_recv() {
                newest = Some(newer);
            }
            let mut staged = staged.ok();
            if let Some(next) = newest {
                // A superseded decode still hands over its early approximate:
                // it was decoded first and stays useful as directional feedback
                // while the exact it belongs to is stale. Only the stale exact
                // is skipped, never painted and never cached.
                let approx = staged
                    .take()
                    .and_then(|staged| staged.approx)
                    .or(aborted_approx);
                if let Some(approx) = approx {
                    let _ = tx.send(PreviewReady {
                        path: request.path.clone(),
                        generation: request.generation,
                        seq: request.seq,
                        fraction: request.fraction,
                        pts: approx.pts,
                        image: approx.image,
                        samples: approx.samples,
                        approximate: true,
                    });
                }
                request = next;
                continue;
            }
            let Some(staged) = staged else {
                break;
            };

            // Stage timings are measured for benches and reports; the worker
            // itself only needs total ordering, so the breakdown is read here
            // to keep the instrumentation live without affecting the send path.
            let timings = &staged.timings;
            let _ = (
                timings.open_ms,
                timings.index_ms,
                timings.decode_ms,
                timings.resize_ms,
                timings.total_ms,
                timings.approx_ms,
                timings.samples,
                timings.width,
                timings.height,
            );
            if let Some(approx) = staged.approx {
                let _ = tx.send(PreviewReady {
                    path: request.path.clone(),
                    generation: request.generation,
                    seq: request.seq,
                    fraction: request.fraction,
                    pts: approx.pts,
                    image: approx.image,
                    samples: approx.samples,
                    approximate: true,
                });
            }
            let exact = staged.exact;
            let _ = tx.send(PreviewReady {
                path: request.path.clone(),
                generation: request.generation,
                seq: request.seq,
                fraction: request.fraction,
                pts: exact.pts,
                image: exact.image,
                samples: exact.samples,
                approximate: false,
            });
            break;
        }
    }
}
// worker end

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_soundtrack_sidecar_reloads_and_misses_when_the_file_changes() {
        let dir = std::env::temp_dir().join(format!("vespera-pcm-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let video = dir.join("clip.mp4");
        std::fs::write(&video, b"source-a").expect("writes");
        let samples = vec![0.0f32, 1.0, -0.5, 0.5];
        store_soundtrack(&video, &samples);
        assert_eq!(load_soundtrack(&video).as_deref(), Some(samples.as_slice()));
        std::fs::write(&video, b"source-b-longer").expect("rewrites");
        assert!(
            load_soundtrack(&video).is_none(),
            "a changed file is a miss"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn playback_stays_inside_the_source_and_1080p() {
        assert_eq!(playback_limit(1920, 1080), (1920, 1080));
        assert_eq!(playback_limit(1280, 720), (1280, 720));
        assert_eq!(playback_limit(640, 360), (640, 360));
        let (width, height) = playback_limit(3840, 2160);
        assert_eq!(width, 3840);
        assert_eq!(height, 2160);
        assert_eq!(width % 2, 0);
        assert_eq!(height % 2, 0);
        assert_eq!(frame_budget(64, 64), BUFFER_FRAMES);
        let slots = frame_budget(1920, 1080);
        assert!(slots < BUFFER_FRAMES);
        assert!(slots >= 2);
        let queued = slots * 1920 * 1080 * 4;
        assert!(queued <= 32 * 1024 * 1024);
        #[cfg(windows)]
        assert_eq!(crate::native_video::engine_name(), "Media Foundation");
    }

    #[test]
    fn the_clock_does_not_finish_a_clip_before_its_first_picture() {
        let total = Duration::from_secs(6);
        assert!(
            !ready_to_finish(Duration::MAX, total, total),
            "startup is not the end of the clip"
        );
        assert!(ready_to_finish(Duration::ZERO, total, total));
    }

    /// A chat clip's decode thread and the next clip must not both sit
    /// inside OpenH264. On Windows the second create access-violates.
    #[test]
    fn a_second_openh264_decoder_starts_after_the_first_releases() {
        let gate = std::sync::Arc::new(std::sync::Barrier::new(2));
        let entered = gate.clone();
        let worker = std::thread::spawn(move || {
            let _session = openh264_session();
            let decoder = openh264::decoder::Decoder::new();
            entered.wait();
            std::thread::sleep(std::time::Duration::from_millis(150));
            drop(decoder);
        });
        gate.wait();
        let _session = openh264_session();
        assert!(
            openh264::decoder::Decoder::new().is_ok(),
            "the next decoder starts once the first has dropped"
        );
        worker.join().expect("the first decoder thread finishes");
    }

    #[test]
    fn presentation_time_honours_the_composition_offset() {
        // A frame stored early but shown late (B-frame reordering).
        assert_eq!(stamp(3000, 2000, 1000), Duration::from_secs(5));
        // A negative offset never goes below zero.
        assert_eq!(stamp(100, -5000, 1000), Duration::ZERO);
        // A zero timescale never divides by zero.
        assert_eq!(stamp(100, 0, 0), Duration::from_secs(100));
    }

    #[test]
    fn ffmpeg_seek_keeps_five_seconds_of_output_decode() {
        assert_eq!(
            ffmpeg_seek_offsets(Duration::ZERO),
            (Duration::ZERO, Duration::ZERO)
        );
        assert_eq!(
            ffmpeg_seek_offsets(Duration::from_secs(3)),
            (Duration::ZERO, Duration::from_secs(3))
        );
        assert_eq!(
            ffmpeg_seek_offsets(Duration::from_secs(540)),
            (Duration::from_secs(535), Duration::from_secs(5))
        );
    }

    #[test]
    fn avcc_length_sizes_parse_and_malformed_fails() {
        // Four-byte prefix, one unit.
        let four = [0, 0, 0, 3, 1, 2, 3];
        assert_eq!(avcc_length_size(&four), Some(4));
        let mut out = Vec::new();
        avcc_to_annex_b(&mut out, &four, 4).expect("converts");
        assert_eq!(out, [0, 0, 0, 1, 1, 2, 3]);
        // Two-byte prefix, two units.
        let two = [0, 2, 9, 9, 0, 1, 7];
        assert_eq!(avcc_length_size(&two), Some(2));
        let mut out = Vec::new();
        avcc_to_annex_b(&mut out, &two, 2).expect("converts");
        assert_eq!(out, [0, 0, 0, 1, 9, 9, 0, 0, 0, 1, 7]);
        // One-byte prefix, two units.
        let one = [2, 5, 5, 1, 6];
        assert_eq!(avcc_length_size(&one), Some(1));
        // Malformed: zero length, overrun, truncated prefix, emptiness.
        assert_eq!(avcc_length_size(&[0, 0, 0, 0]), None);
        assert_eq!(avcc_length_size(&[]), None);
        assert!(avcc_to_annex_b(&mut Vec::new(), &[0, 0, 0, 9, 1, 2], 4).is_err());
        assert!(avcc_to_annex_b(&mut Vec::new(), &[0, 0], 4).is_err());
        // An empty sample converts to nothing instead of failing.
        assert!(avcc_to_annex_b(&mut Vec::new(), &[], 4).is_ok());
    }

    #[test]
    fn pausing_and_resuming_never_counts_a_stretch_twice() {
        // Ten seconds in, the pause bakes the position into the base and
        // freezes the anchor on the sink. Five more output seconds resume
        // from twelve, not from nineteen.
        let paused = audio_position(Duration::ZERO, Duration::from_secs(7), Duration::ZERO);
        assert_eq!(paused, Duration::from_secs(7));
        let anchor = Duration::from_secs(7);
        let resumed = audio_position(paused, Duration::from_secs(12), anchor);
        assert_eq!(resumed, Duration::from_secs(12));
        // Ten quick pause cycles drift by nothing at all.
        let mut base = Duration::ZERO;
        let mut anchor = Duration::ZERO;
        let mut sink = Duration::ZERO;
        for _ in 0..10 {
            sink += Duration::from_secs(1);
            base = audio_position(base, sink, anchor);
            anchor = sink;
        }
        assert_eq!(base, Duration::from_secs(10));
    }

    #[test]
    fn the_picture_holds_instead_of_showing_the_future() {
        fn times(seconds: &[u64]) -> Vec<Duration> {
            seconds.iter().map(|s| Duration::from_secs(*s)).collect()
        }
        // Future frames wait their turn; with nothing shown the poster stays.
        assert_eq!(
            choose_pts(
                times(&[5, 6]).into_iter(),
                Duration::from_secs(3),
                Duration::MAX
            ),
            None
        );
        // Due frames show the newest one at or behind the position.
        assert_eq!(
            choose_pts(
                times(&[5, 6]).into_iter(),
                Duration::from_secs(5),
                Duration::MAX
            ),
            Some(Duration::from_secs(5))
        );
        assert_eq!(
            choose_pts(
                times(&[5, 6]).into_iter(),
                Duration::from_secs(7),
                Duration::MAX
            ),
            Some(Duration::from_secs(6))
        );
        // With no frame due, the picture on screen holds.
        assert_eq!(
            choose_pts(
                times(&[5, 6]).into_iter(),
                Duration::from_secs(4),
                Duration::from_secs(5)
            ),
            Some(Duration::from_secs(5))
        );
        // A picture no longer buffered cannot hold: back to the poster.
        assert_eq!(
            choose_pts(
                times(&[5, 6]).into_iter(),
                Duration::from_secs(4),
                Duration::from_secs(2)
            ),
            None
        );
    }

    #[test]
    fn a_full_buffer_of_future_frames_holds_instead_of_dropping() {
        assert!(matches!(
            buffer_room(10, None, Duration::ZERO, BUFFER_FRAMES),
            BufferRoom::Push
        ));
        assert!(matches!(
            buffer_room(BUFFER_FRAMES - 1, None, Duration::ZERO, BUFFER_FRAMES),
            BufferRoom::Push
        ));
        // Full of future frames: hold, so the queued frames survive and
        // the decoder waits on its channel.
        assert!(matches!(
            buffer_room(
                BUFFER_FRAMES,
                Some(Duration::from_secs(5)),
                Duration::ZERO,
                BUFFER_FRAMES
            ),
            BufferRoom::Hold
        ));
        // A frame well behind playback makes room for the arrival.
        assert!(matches!(
            buffer_room(
                BUFFER_FRAMES,
                Some(Duration::ZERO),
                Duration::from_secs(5),
                BUFFER_FRAMES
            ),
            BufferRoom::EvictThenPush
        ));
    }

    #[test]
    fn a_full_queue_loses_no_frame_across_ticks() {
        // A full buffer of future frames plus a loaded channel: every
        // numbered frame must survive repeated ticks, none silently dropped.
        let path = std::path::PathBuf::from("queue.mp4");
        let (tx, rx) = sync_channel::<DecodeMsg>(BUFFER_FRAMES);
        let frame = |secs: u64| Frame {
            pts: Duration::from_secs(secs),
            generation: 0,
            image: ColorImage::new([2, 2], vec![egui::Color32::BLACK; 4]),
        };
        let mut buffered = VecDeque::new();
        for secs in 10..10 + BUFFER_FRAMES as u64 {
            buffered.push_back(frame(secs));
        }
        for secs in 100..105 {
            tx.send(DecodeMsg::Frame(frame(secs)))
                .expect("room in the channel");
        }
        let mut player = Player {
            active: Some(Active {
                path: path.clone(),
                clip: Clip {
                    duration: Duration::from_secs(200),
                    width: 64,
                    height: 64,
                    has_audio: false,
                    ffmpeg: false,
                },
                audio: None,
                pcm: None,
                audio_rx: None,
                audio_task: None,
                playing: true,
                base: Duration::ZERO,
                anchor: Duration::ZERO,
                started: Instant::now(),
                frames: rx,
                slots: BUFFER_FRAMES,
                buffered,
                held: None,
                shown: Duration::MAX,
                texture: None,
                generation: Arc::new(AtomicU64::new(1)),
                control: PlaybackControl::new(Duration::ZERO, false, 1.0),
                worker: 0,
                landed_pts: None,
                trailing: None,
                decode_done: false,
                seeking: false,
                finished: false,
                fallback_used: false,
                decode_error: None,
                seek_diag: None,
            }),
            ..Player::default()
        };
        let ctx = egui::Context::default();
        for _ in 0..3 {
            player.poll(&ctx, &path);
        }
        // Reap everything: the buffer, the held slot and the channel.
        let mut seen: Vec<u64> = player
            .active
            .as_ref()
            .expect("still open")
            .buffered
            .iter()
            .map(|frame| frame.pts.as_secs())
            .collect();
        let active = player.active.as_mut().expect("still open");
        assert!(active.held.is_some(), "the overflow parks in the held slot");
        if let Some(frame) = active.held.take() {
            seen.push(frame.pts.as_secs());
        }
        while let Ok(DecodeMsg::Frame(frame)) = active.frames.try_recv() {
            seen.push(frame.pts.as_secs());
        }
        seen.sort_unstable();
        let mut expected: Vec<u64> = (10..10 + BUFFER_FRAMES as u64).collect();
        expected.extend(100..105);
        expected.sort_unstable();
        assert_eq!(seen, expected, "every queued frame survives");
        player.stop();
    }

    #[test]
    fn split_pipe_reads_keep_every_sample_aligned() {
        // One hundred samples as raw bytes, fed whole and in hostile splits:
        // identical samples out, nothing dropped, nothing misaligned.
        let samples: Vec<f32> = (0..100).map(|i| i as f32 * 0.25 - 12.0).collect();
        let mut bytes = Vec::new();
        for sample in &samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        let whole = {
            let mut pcm = Vec::new();
            let mut pending = Vec::new();
            push_pcm(&mut pcm, &mut pending, &bytes);
            assert!(pending.is_empty());
            pcm
        };
        assert_eq!(whole, samples);
        for width in [1usize, 3, 5, 7] {
            let mut pcm = Vec::new();
            let mut pending = Vec::new();
            for chunk in bytes.chunks(width) {
                push_pcm(&mut pcm, &mut pending, chunk);
            }
            assert!(pending.is_empty(), "width {width} leaves no tail");
            assert_eq!(pcm, samples, "width {width} aligns");
        }
    }

    #[test]
    fn replaying_a_finished_video_starts_playing() {
        let dir = std::env::temp_dir().join(format!("vespera-replay-{}", std::process::id()));
        let Some(path) = sample_clip(&dir) else {
            return;
        };
        let mut player = Player::default();
        let mut stop = || {};
        player.toggle(&path, &mut stop).expect("opens");
        assert!(
            player.active.as_ref().is_some_and(|active| active.playing),
            "opening plays"
        );
        let total = player
            .active
            .as_ref()
            .map(|active| active.clip.duration)
            .expect("clip");
        // Watch it to the end: base reaches the length, playback stops.
        {
            let active = player.active.as_mut().expect("open");
            active.base = total;
            active.finished = true;
            active.playing = false;
        }
        player.toggle(&path, &mut stop).expect("replays");
        let active = player.active.as_ref().expect("still open");
        assert!(
            active.playing,
            "one click replays instead of parking at zero"
        );
        assert!(active.base < total, "back at the start");
        player.stop();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn seeking_starts_at_the_nearest_earlier_key_frame() {
        // Samples 1 and 4 are key frames; the target sits past sample 5.
        let table = vec![
            (0, true),
            (33, false),
            (66, false),
            (99, true),
            (132, false),
        ];
        assert_eq!(pick_keyframe(&table, 150), Some(4));
        // Before the second key frame, the first one still opens.
        assert_eq!(pick_keyframe(&table, 90), Some(1));
        // Past the end, the last key frame opens.
        assert_eq!(pick_keyframe(&table, 10_000), Some(4));
        // A target before the first key frame answers nothing, so the
        // search widens instead of opening ahead of the target, where the
        // frames to show could never decode.
        let late = vec![(99, false), (132, true), (165, false)];
        assert_eq!(pick_keyframe(&late, 0), None);
        // With no key frame at all there is nothing to open on.
        let plain = vec![(0, false), (33, false)];
        assert_eq!(pick_keyframe(&plain, 100), None);
        assert_eq!(pick_keyframe(&[], 100), None);
    }
    #[test]
    fn a_wide_gap_still_opens_behind_the_target() {
        // A long interval between key frames must not open ahead: with a
        // key frame every six seconds, a jump to fifteen seconds starts
        // from twelve, never from eighteen.
        let mut table = Vec::new();
        for secs in [0u64, 6, 12, 18] {
            table.push((secs * 1000, true));
            table.push((secs * 1000 + 100, false));
        }
        assert_eq!(pick_keyframe(&table, 15_000), Some(5));
        assert_eq!(pick_keyframe(&table, 12_000), Some(5));
        // Nothing behind the target widens the search instead of taking
        // the key frame ahead.
        assert_eq!(pick_keyframe(&table[6..], 15_000), None);
    }
    #[test]
    fn a_seek_lands_on_a_real_key_frame() {
        // The decoder can only enter on a key frame, so a jump has to answer
        // one no matter where the target falls.
        let dir = std::env::temp_dir().join(format!("vespera-seek-{}", std::process::id()));
        let Some(path) = sample_clip_seconds(&dir, 6) else {
            return;
        };
        let file = std::fs::File::open(&path).expect("opens");
        let size = file.metadata().expect("stats").len();
        let mut mp4 = mp4::Mp4Reader::read_header(BufReader::new(file), size).expect("header");
        let track = mp4
            .tracks()
            .values()
            .find(|track| track.track_type().ok() == Some(mp4::TrackType::Video))
            .expect("video track");
        let (track_id, timescale) = (track.track_id(), u64::from(track.timescale().max(1)));
        let total = mp4.duration();
        assert!(!total.is_zero(), "the sample clip has a length");
        for fraction in [0.05f32, 0.25, 0.5, 0.75, 0.95, 1.0] {
            let at = total.mul_f32(fraction);
            if at.is_zero() {
                continue;
            }
            let start = first_sample_at(&mut mp4, track_id, timescale, at, total);
            let sample = mp4
                .read_sample(track_id, start)
                .expect("reads")
                .expect("a sample");
            assert!(
                sample.is_sync,
                "a seek to {fraction} starts on sample {start}, a key frame"
            );
            let pts = stamp(sample.start_time, sample.rendering_offset, timescale);
            assert!(
                pts <= at,
                "the key frame at {pts:?} sits at or before the target {at:?}"
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_long_gap_opens_behind_the_target() {
        // Key frames six seconds apart: jumps to 7, 13, 15 and 19 seconds
        // must all start behind their target, never on the key frame ahead.
        let dir = std::env::temp_dir().join(format!("vespera-longgap-{}", std::process::id()));
        let Some(path) = sample_clip_gop(&dir, 20, 60) else {
            return;
        };
        let file = std::fs::File::open(&path).expect("opens");
        let size = file.metadata().expect("stats").len();
        let mut mp4 = mp4::Mp4Reader::read_header(BufReader::new(file), size).expect("header");
        let track = mp4
            .tracks()
            .values()
            .find(|track| track.track_type().ok() == Some(mp4::TrackType::Video))
            .expect("video track");
        let (track_id, timescale) = (track.track_id(), u64::from(track.timescale().max(1)));
        let total = mp4.duration();
        assert!(
            total >= Duration::from_secs(19),
            "twenty seconds of clip: {total:?}"
        );
        for at in [7, 13, 15, 19].map(Duration::from_secs) {
            let start = first_sample_at(&mut mp4, track_id, timescale, at, total);
            let sample = mp4
                .read_sample(track_id, start)
                .expect("reads")
                .expect("a sample");
            assert!(sample.is_sync, "a jump to {at:?} opens on a key frame");
            let pts = stamp(sample.start_time, sample.rendering_offset, timescale);
            assert!(
                pts <= at,
                "the key frame at {pts:?} sits behind the target {at:?}"
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Drives one open clip until the viewer shows a frame, or nothing
    /// past the deadline.
    fn drive_until(
        player: &mut Player,
        ctx: &egui::Context,
        path: &std::path::Path,
        deadline: std::time::Instant,
        mut done: impl FnMut(Duration, bool) -> bool,
    ) -> (Duration, bool) {
        loop {
            let (position, playing) = match player.poll(ctx, path) {
                State::Showing {
                    position, playing, ..
                } => (position, playing),
                State::Loading => (Duration::ZERO, true),
                State::Unsupported(why) => panic!("the sample clip plays: {why}"),
            };
            if done(position, playing) {
                return (position, playing);
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the jump lands: {position:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn a_jump_lands_and_stays_in_sync() {
        // A twenty-second clip with a tone, played for real: jump to
        // fifteen seconds while playing, then to five while paused. The
        // picture must arrive at the target and stay with the sound.
        let dir = std::env::temp_dir().join(format!("vespera-jump-{}", std::process::id()));
        let Some(path) = sample_clip_seconds(&dir, 20) else {
            return;
        };
        let clip = probe(&path).expect("the header reads");
        let total = clip.duration;
        let ctx = egui::Context::default();
        let mut player = Player::default();
        let mut stop = || {};
        player.toggle(&path, &mut stop).expect("opens");
        // Playing jump to three quarters: the picture lands near fifteen
        // seconds and keeps playing.
        player.seek(&path, 0.75).expect("jumps");
        let target = total.mul_f32(0.75);
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        let (position, playing) = drive_until(&mut player, &ctx, &path, deadline, |position, _| {
            position >= target.saturating_sub(Duration::from_secs(2))
        });
        assert!(playing, "a playing jump keeps playing");
        assert!(
            position <= target + Duration::from_secs(5),
            "the picture lands near its target: {position:?} for {target:?}"
        );
        // Paused jump to one quarter: the frame shows on its own, still
        // paused, with no mouse needed.
        player.toggle(&path, &mut stop).expect("pauses");
        player.seek(&path, 0.25).expect("jumps paused");
        let target = total.mul_f32(0.25);
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        let (position, playing) = drive_until(&mut player, &ctx, &path, deadline, |position, _| {
            position >= target.saturating_sub(Duration::from_millis(500))
        });
        assert!(!playing, "a paused jump stays paused");
        assert!(
            (position.as_secs_f32() - target.as_secs_f32()).abs() < 1.5,
            "the paused frame is the target: {position:?} for {target:?}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rapid_jumps_keep_only_the_newest_target() {
        // Three jumps with no paint between them: the retired decodes and
        // extractions die, and playback settles at the last target.
        let dir = std::env::temp_dir().join(format!("vespera-rapid-{}", std::process::id()));
        let Some(path) = sample_clip_seconds(&dir, 20) else {
            return;
        };
        let clip = probe(&path).expect("the header reads");
        let total = clip.duration;
        let ctx = egui::Context::default();
        let mut player = Player::default();
        let mut stop = || {};
        player.toggle(&path, &mut stop).expect("opens");
        player.seek(&path, 0.9).expect("first");
        player.seek(&path, 0.1).expect("second");
        player.seek(&path, 0.8).expect("third");
        let target = total.mul_f32(0.8);
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        let (position, _) = drive_until(&mut player, &ctx, &path, deadline, |position, _| {
            position >= target.saturating_sub(Duration::from_secs(2))
        });
        assert!(
            position <= target + Duration::from_secs(5),
            "only the newest jump survives: {position:?} for {target:?}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn analyze_reports_length_and_a_real_poster() {
        // A two-second clip: the analysis answers about two seconds and a
        // JPEG poster, both decoded from the file itself.
        let dir = std::env::temp_dir().join(format!("vespera-analyze-{}", std::process::id()));
        let Some(path) = sample_clip_seconds(&dir, 2) else {
            return;
        };
        let analysis = analyze(&path);
        assert!(
            analysis
                .seconds
                .is_some_and(|seconds| (1..=3).contains(&seconds)),
            "about two seconds: {:?}",
            analysis.seconds
        );
        let poster = analysis.poster.expect("a poster");
        assert!(
            poster.len() > 2 && poster[0] == 0xFF && poster[1] == 0xD8,
            "a JPEG poster, {} bytes",
            poster.len()
        );
        // A file cut to nothing stays unknown instead of answering zero.
        std::fs::write(&path, b"not a video").expect("truncates");
        let analysis = analyze(&path);
        assert!(analysis.seconds.is_none(), "no length is no answer");
        assert!(analysis.poster.is_none(), "no frames is no poster");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn seek_fractions_stay_inside_the_clip() {
        let total = Duration::from_secs(64);
        assert_eq!(total.mul_f32(0.0), Duration::ZERO);
        assert_eq!(total.mul_f32(1.0), total);
        assert_eq!(total.mul_f32(2.0_f32.clamp(0.0, 1.0)), total);
        assert_eq!(total.mul_f32((-1.0_f32).clamp(0.0, 1.0)), Duration::ZERO);
    }
    #[test]
    fn linear_resampling_keeps_duration() {
        // Two seconds at 44.1 kHz become two seconds at 48 kHz.
        let input = vec![0.0f32; 44_100 * 2];
        let output = resample_linear(&input, 44_100, PCM_RATE);
        assert_eq!(output.len(), PCM_RATE as usize * 2);
        // Silence in, silence out.
        assert!(output.iter().all(|sample| *sample == 0.0));
        // Identity rates copy.
        let input = vec![0.5f32; 8];
        assert_eq!(resample_linear(&input, PCM_RATE, PCM_RATE), input);
    }
    #[test]
    fn chat_video_sound_lasts_the_whole_clip() {
        // Interleaved MP4s carry picture packets among the sound ones. rodio
        // 0.22.2 stops the sound at the first packet of another track (about
        // a tenth of a second), which the listener hears as silence; the fix
        // skips packets of every track but the selected one (upstream
        // RustAudio/rodio#833).
        use rodio::Source;
        let dir = std::env::temp_dir().join(format!("vespera-video-sound-{}", std::process::id()));
        let Some(path) =
            sample_clip_sized(&dir, "sound.mp4", "64x64", 3, "baseline", "0", "10", true)
        else {
            eprintln!("skipped: ffmpeg unavailable for fixtures");
            return;
        };
        let file = std::fs::File::open(&path).expect("opens");
        let mut decoder = rodio::Decoder::new(std::io::BufReader::new(file)).expect("decodes");
        let rate = decoder.sample_rate().get();
        let channels = usize::from(decoder.channels().get());
        let frames = decoder.by_ref().count() / channels;
        let sound = Duration::from_secs_f64(frames as f64 / f64::from(rate));
        assert!(
            sound > Duration::from_millis(2900),
            "a three second clip plays about three seconds of sound, not {sound:?}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn soundtrack_decodes_in_process_without_ffmpeg() {
        let dir = std::env::temp_dir().join(format!("vespera-symphonia-{}", std::process::id()));
        let Some(path) = sample_clip(&dir) else {
            return;
        };
        let pcm = symphonia_pcm(&path, &|| true).expect("AAC decodes in-process");
        assert!(pcm.len() > PCM_RATE as usize, "about two seconds of sound");
        assert!(
            pcm.iter().any(|sample| sample.abs() > 0.01),
            "the tone is audible, not silence"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
    /// Makes a two-second H.264 video with a tone, or nothing without ffmpeg.
    fn sample_clip(dir: &std::path::Path) -> Option<std::path::PathBuf> {
        sample_clip_seconds(dir, 2)
    }
    /// Makes a clip of that many seconds with a key frame every second, so a
    /// seek has something to land on, or nothing without ffmpeg.
    fn sample_clip_seconds(dir: &std::path::Path, secs: u32) -> Option<std::path::PathBuf> {
        sample_clip_gop(dir, secs, 10)
    }
    /// Makes a clip with a key frame every `gop` frames (ten frames per
    /// second), or nothing without ffmpeg.
    fn sample_clip_gop(dir: &std::path::Path, secs: u32, gop: u32) -> Option<std::path::PathBuf> {
        std::fs::create_dir_all(dir).ok()?;
        let path = dir.join(format!("clip-{secs}-{gop}.mp4"));
        let gop = gop.to_string();
        let made = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y"])
            .args([
                "-f",
                "lavfi",
                "-i",
                &format!("color=c=blue:s=64x64:d={secs}:r=10"),
            ])
            .args([
                "-f",
                "lavfi",
                "-i",
                &format!("sine=frequency=440:duration={secs}"),
            ])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            // WhatsApp sends baseline video, so the clip has no B-frames either.
            .args(["-profile:v", "baseline", "-bf", "0"])
            .args(["-g", &gop, "-keyint_min", &gop, "-sc_threshold", "0"])
            .args(["-c:a", "aac", "-shortest", "-movflags", "+faststart"])
            .arg(&path)
            .status()
            .is_ok_and(|status| status.success());
        made.then_some(path)
    }
    /// Bigger synthetic clip at a representative resolution, with
    /// profile, B-frames, keyframe interval and sound of its own.
    /// All ffmpeg arguments avoid commas for shell safety.
    /// Eight knobs mirror the ffmpeg command line below one by one, so the
    /// argument count stays even though clippy caps plain helpers at seven.
    #[allow(clippy::too_many_arguments)]
    fn sample_clip_sized(
        dir: &std::path::Path,
        name: &str,
        size: &str,
        secs: u32,
        profile: &str,
        bf: &str,
        gop: &str,
        audio: bool,
    ) -> Option<std::path::PathBuf> {
        std::fs::create_dir_all(dir).ok()?;
        let path = dir.join(name);
        let mut command = std::process::Command::new("ffmpeg");
        command.args(["-v", "error", "-y"]);
        command.args([
            "-f",
            "lavfi",
            "-i",
            &format!("color=c=blue:s={size}:d={secs}:r=10"),
        ]);
        if audio {
            command.args([
                "-f",
                "lavfi",
                "-i",
                &format!("sine=frequency=440:duration={secs}"),
            ]);
        }
        command.args(["-c:v", "libx264", "-pix_fmt", "yuv420p"]);
        command.args(["-profile:v", profile, "-bf", bf]);
        command.args(["-g", gop, "-keyint_min", gop, "-sc_threshold", "0"]);
        if audio {
            command.args(["-c:a", "aac", "-shortest"]);
        } else {
            command.args(["-an"]);
        }
        command.args(["-movflags", "+faststart"]);
        command.arg(&path);
        let made = command.status().is_ok_and(|status| status.success());
        if !made {
            eprintln!("skipped: ffmpeg could not encode {name}");
            return None;
        }
        Some(path)
    }
    /// Makes a variant of the sample clip: fast metadata, trailing metadata, or fragments.
    fn sample_variant(
        dir: &std::path::Path,
        name: &str,
        flags: &[&str],
    ) -> Option<std::path::PathBuf> {
        let path = dir.join(name);
        let mut command = std::process::Command::new("ffmpeg");
        command.args(["-v", "error", "-y"]);
        command.args(["-f", "lavfi", "-i", "color=c=blue:s=64x64:d=2:r=10"]);
        command.args(["-f", "lavfi", "-i", "sine=frequency=440:duration=2"]);
        command.args(["-c:v", "libx264", "-pix_fmt", "yuv420p"]);
        command.args(["-profile:v", "baseline", "-bf", "0"]);
        command.args(["-c:a", "aac", "-shortest"]);
        for flag in flags {
            command.args(["-movflags", flag]);
        }
        command.arg(&path);
        let made = command.status().is_ok_and(|status| status.success());
        made.then_some(path)
    }
    /// Makes a clip with the given H.264 profile and B-frame count, or
    /// nothing without ffmpeg. Skips print instead of vanishing silently.
    fn sample_clip_profile(
        dir: &std::path::Path,
        name: &str,
        secs: u32,
        profile: &str,
        bf: &str,
    ) -> Option<std::path::PathBuf> {
        std::fs::create_dir_all(dir).ok()?;
        let path = dir.join(name);
        let made = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y"])
            .args([
                "-f",
                "lavfi",
                "-i",
                &format!("color=c=red:s=64x64:d={secs}:r=10"),
            ])
            .args([
                "-f",
                "lavfi",
                "-i",
                &format!("sine=frequency=660:duration={secs}"),
            ])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .args(["-profile:v", profile, "-bf", bf])
            .args(["-g", "10", "-keyint_min", "10", "-sc_threshold", "0"])
            .args(["-c:a", "aac", "-shortest", "-movflags", "+faststart"])
            .arg(&path)
            .status()
            .is_ok_and(|status| status.success());
        if !made {
            eprintln!("skipped: ffmpeg could not encode {profile} bf={bf}");
            return None;
        }
        Some(path)
    }
    /// Tiny HEVC clip: the in-process decoder cannot read it, so a
    /// preview must fail clean instead of hanging or spawning work.
    fn sample_hevc(dir: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
        std::fs::create_dir_all(dir).ok()?;
        let path = dir.join(name);
        let made = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y"])
            .args(["-f", "lavfi", "-i", "color=c=green:s=64x64:d=2:r=10"])
            .args(["-c:v", "libx265", "-pix_fmt", "yuv420p"])
            .args(["-movflags", "+faststart"])
            .arg(&path)
            .status()
            .is_ok_and(|status| status.success());
        if !made {
            eprintln!("skipped: ffmpeg could not encode hevc");
            return None;
        }
        Some(path)
    }
    #[test]
    fn reordered_video_plays_through_ffmpeg() {
        // Main profile with B-frames: composition offsets prove the
        // reorder, so the probe must route to ffmpeg instead of stamping
        // decode-order pictures with presentation times.
        let dir = std::env::temp_dir().join(format!("vespera-reorder-{}", std::process::id()));
        let Some(path) = sample_clip_profile(&dir, "main-bf3.mp4", 4, "main", "3") else {
            return;
        };
        let clip = probe(&path).expect("the header reads");
        if !ffmpeg_present() {
            assert!(probe(&path).is_err(), "no silent fallback without ffmpeg");
            let _ = std::fs::remove_dir_all(dir);
            return;
        }
        if cfg!(windows) {
            assert!(
                !clip.ffmpeg,
                "Windows tries Media Foundation before the external player"
            );
        } else {
            assert!(clip.ffmpeg, "reordered tracks present through ffmpeg");
        }
        let ctx = egui::Context::default();
        let mut player = Player::default();
        let mut stop = || {};
        player.toggle(&path, &mut stop).expect("opens");
        // ffmpeg on a busy runner can take most of a minute to present two
        // seconds. The test cap is 60 seconds, so the wait stays inside it.
        let deadline = std::time::Instant::now() + Duration::from_secs(45);
        let (position, _) = drive_until(&mut player, &ctx, &path, deadline, |position, _| {
            position >= Duration::from_secs(2)
        });
        assert!(
            position >= Duration::from_secs(2),
            "ffmpeg presents the reordered clip: {position:?}"
        );
        let ffmpeg = player
            .active
            .as_ref()
            .is_some_and(|active| active.clip.ffmpeg);
        assert!(ffmpeg || cfg!(windows), "the fallback engine stayed on");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Two-second 10fps segment joined to two-second 5fps segment with
    /// fps_mode vfr, reencoded baseline without B-frames: r_frame_rate
    /// stays 10 while avg_frame_rate drops, so gaps are uneven with no
    /// reorder. All ffmpeg args avoid commas for shell safety.
    fn sample_clip_vfr(dir: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
        sample_clip_vfr_sized(dir, name, "64x64")
    }

    fn sample_clip_vfr_sized(
        dir: &std::path::Path,
        name: &str,
        size: &str,
    ) -> Option<std::path::PathBuf> {
        std::fs::create_dir_all(dir).ok()?;
        let seg_a = dir.join("vfr-seg-a.mp4");
        let seg_b = dir.join("vfr-seg-b.mp4");
        let out = dir.join(name);
        let seg = |path: &std::path::Path, color: &str, rate: &str, tone: &str| {
            std::process::Command::new("ffmpeg")
                .args(["-v", "error", "-y"])
                .args([
                    "-f",
                    "lavfi",
                    "-i",
                    &format!("color=c={color}:s={size}:d=2:r={rate}"),
                ])
                .args([
                    "-f",
                    "lavfi",
                    "-i",
                    &format!("sine=frequency={tone}:duration=2"),
                ])
                .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
                .args(["-profile:v", "baseline", "-bf", "0"])
                .args(["-g", "10", "-keyint_min", "10", "-sc_threshold", "0"])
                .args(["-c:a", "aac", "-shortest"])
                .arg(path)
                .status()
                .is_ok_and(|status| status.success())
        };
        if !seg(&seg_a, "blue", "10", "440") {
            return None;
        }
        if !seg(&seg_b, "red", "5", "660") {
            return None;
        }
        // Join with the concat demuxer and stream copy: the slices stay
        // byte-identical to proven single encodes, only the cadence turns
        // uneven (10fps then 5fps). A filter re-encode is avoided on
        // purpose after a re-encoded join decoded to zero pictures here.
        // Bare file names with the join running inside the directory:
        // absolute Windows paths would need escaping in the list.
        let list = dir.join("vfr-list.txt");
        if std::fs::write(&list, "file vfr-seg-a.mp4\nfile vfr-seg-b.mp4\n").is_err() {
            return None;
        }
        let joined = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y"])
            .args(["-f", "concat", "-safe", "0", "-i", "vfr-list.txt"])
            .args(["-c", "copy", "-movflags", "+faststart"])
            .arg(&out)
            .current_dir(dir)
            .status()
            .is_ok_and(|status| status.success());
        if !joined {
            return None;
        }
        Some(out)
    }

    /// VFR join through a filter re-encode instead of stream copy.
    fn sample_clip_vfr_reencode(dir: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
        std::fs::create_dir_all(dir).ok()?;
        let seg_a = dir.join("vfre-seg-a.mp4");
        let seg_b = dir.join("vfre-seg-b.mp4");
        let out = dir.join(name);
        let seg = |path: &std::path::Path, color: &str, rate: &str, tone: &str| {
            std::process::Command::new("ffmpeg")
                .args(["-v", "error", "-y"])
                .args([
                    "-f",
                    "lavfi",
                    "-i",
                    &format!("color=c={color}:s=64x64:d=2:r={rate}"),
                ])
                .args([
                    "-f",
                    "lavfi",
                    "-i",
                    &format!("sine=frequency={tone}:duration=2"),
                ])
                .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
                .args(["-profile:v", "baseline", "-bf", "0"])
                .args(["-c:a", "aac", "-shortest"])
                .arg(path)
                .status()
                .is_ok_and(|status| status.success())
        };
        if !seg(&seg_a, "blue", "10", "440") {
            return None;
        }
        if !seg(&seg_b, "red", "5", "660") {
            return None;
        }
        let joined = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y"])
            .args(["-i", &seg_a.to_string_lossy()])
            .args(["-i", &seg_b.to_string_lossy()])
            .args(["-filter_complex", "concat=n=2:v=1:a=1"])
            .args(["-fps_mode", "vfr"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .args(["-profile:v", "baseline", "-bf", "0"])
            .args(["-c:a", "aac", "-movflags", "+faststart"])
            .arg(&out)
            .status()
            .is_ok_and(|status| status.success());
        if !joined {
            return None;
        }
        Some(out)
    }

    #[test]
    fn vfr_clip_plays_and_seeks() {
        let dir = std::env::temp_dir().join(format!("vespera-vfr-{}", std::process::id()));
        let Some(path) = sample_clip_vfr(&dir, "vfr.mp4") else {
            eprintln!("skipped: ffmpeg unavailable for fixtures");
            return;
        };
        let clip = probe(&path).expect("the header reads");
        assert!(!clip.ffmpeg, "uneven gaps without offsets stay in process");
        let total = clip.duration;
        assert!(
            total >= Duration::from_secs(3),
            "two segments joined: {total:?}"
        );
        let ctx = egui::Context::default();
        let mut player = Player::default();
        let mut stop = || {};
        player.toggle(&path, &mut stop).expect("opens");
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let (position, _) = drive_until(&mut player, &ctx, &path, deadline, |position, _| {
            position >= Duration::from_secs(1)
        });
        assert!(
            position >= Duration::from_secs(1),
            "frames cross the rate change: {position:?}"
        );
        for fraction in [0.75, 0.25] {
            player.seek(&path, fraction).expect("jumps");
            let target = total.mul_f32(fraction);
            let deadline = std::time::Instant::now() + Duration::from_secs(20);
            let (landed, _) = drive_until(&mut player, &ctx, &path, deadline, |position, _| {
                position >= target.saturating_sub(Duration::from_millis(400))
            });
            assert!(
                landed <= target + Duration::from_secs(2),
                "VFR jump lands: {landed:?} for {target:?}"
            );
        }
        player.toggle(&path, &mut stop).expect("pauses");
        player.seek(&path, 0.5).expect("jumps paused");
        let target = total.mul_f32(0.5);
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            match player.poll(&ctx, &path) {
                State::Showing {
                    position,
                    seeking,
                    playing,
                    ..
                } => {
                    assert!(!playing, "paused stays paused");
                    if seeking {
                        assert_eq!(position, target, "paused clock holds the target");
                    } else {
                        break;
                    }
                }
                State::Loading => {}
                State::Unsupported(why) => panic!("VFR clip plays: {why}"),
            }
            assert!(std::time::Instant::now() < deadline, "paused jump lands");
        }
        player.toggle(&path, &mut stop).expect("resumes");
        for fraction in [0.1, 0.9, 0.5] {
            player.seek(&path, fraction).expect("rapid jump");
        }
        let target = total.mul_f32(0.5);
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let (landed, _) = drive_until(&mut player, &ctx, &path, deadline, |position, _| {
            position >= target.saturating_sub(Duration::from_millis(400))
        });
        assert!(
            landed <= target + Duration::from_secs(2),
            "only the newest rapid jump lands: {landed:?}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn vfr_silent_miss_falls_back_and_plays() {
        // Fallback-after-failure on the real player path: the re-encoded
        // VFR join decodes to zero pictures in process, so one controlled
        // engine change must present it instead of refusing.
        let dir = std::env::temp_dir().join(format!("vespera-vfrfb-{}", std::process::id()));
        let Some(path) = sample_clip_vfr_reencode(&dir, "vfr-fb.mp4") else {
            eprintln!("skipped: ffmpeg unavailable for fixtures");
            return;
        };
        let clip = probe(&path).expect("the header reads");
        assert!(!clip.ffmpeg, "the header alone does not route to ffmpeg");
        if !ffmpeg_present() {
            eprintln!("skipped: fallback needs ffmpeg");
            let _ = std::fs::remove_dir_all(dir);
            return;
        }
        let total = clip.duration;
        let ctx = egui::Context::default();
        let mut player = Player::default();
        let mut stop = || {};
        player.toggle(&path, &mut stop).expect("opens");
        let deadline = std::time::Instant::now() + Duration::from_secs(25);
        let (position, _) = drive_until(&mut player, &ctx, &path, deadline, |position, _| {
            position >= Duration::from_secs(1)
        });
        assert!(
            position >= Duration::from_secs(1),
            "fallback presents pictures: {position:?}"
        );
        // Windows may present this join through Media Foundation, so the
        // external engine is required only where that decoder is absent.
        if cfg!(not(windows)) {
            assert!(
                player
                    .active
                    .as_ref()
                    .is_some_and(|active| active.clip.ffmpeg),
                "the engine changed after the silent miss"
            );
            assert!(
                player
                    .active
                    .as_ref()
                    .is_some_and(|active| active.fallback_used),
                "exactly the single controlled fallback ran"
            );
        }
        player.seek(&path, 0.5).expect("jumps");
        let target = total.mul_f32(0.5);
        let deadline = std::time::Instant::now() + Duration::from_secs(25);
        let (landed, _) = drive_until(&mut player, &ctx, &path, deadline, |position, _| {
            position >= target.saturating_sub(Duration::from_millis(500))
        });
        assert!(
            landed <= target + Duration::from_secs(3),
            "seek lands after fallback: {landed:?}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn fallback_switch_and_close_retire_cleanly() {
        // Switch and close during a fallback: the old engine retires by
        // generation, the new file plays, and stop leaves no active clip.
        // State-level proof; OS thread exit races past channel close.
        let dir = std::env::temp_dir().join(format!("vespera-vfrswitch-{}", std::process::id()));
        let Some(first) = sample_clip_vfr_reencode(&dir, "first.mp4") else {
            eprintln!("skipped: ffmpeg unavailable for fixtures");
            return;
        };
        if !ffmpeg_present() {
            eprintln!("skipped: fallback needs ffmpeg");
            let _ = std::fs::remove_dir_all(dir);
            return;
        }
        let Some(second) = sample_clip_seconds(&dir, 4) else {
            eprintln!("skipped: ffmpeg unavailable for fixtures");
            let _ = std::fs::remove_dir_all(dir);
            return;
        };
        let ctx = egui::Context::default();
        let mut player = Player::default();
        let mut stop = || {};
        player.toggle(&first, &mut stop).expect("opens");
        let deadline = std::time::Instant::now() + Duration::from_secs(25);
        let (position, _) = drive_until(&mut player, &ctx, &first, deadline, |position, _| {
            position >= Duration::from_secs(1)
        });
        let ffmpeg = player
            .active
            .as_ref()
            .is_some_and(|active| active.clip.ffmpeg);
        // Windows may play this clip through Media Foundation, so the
        // external ffmpeg flag stays off. Either engine has to move.
        assert!(
            ffmpeg || position >= Duration::from_secs(1),
            "fallback engine on, or the system decoder played it: {position:?}"
        );
        let retired_arc = player
            .active
            .as_ref()
            .map(|active| active.generation.clone());
        let retired = retired_arc.as_ref().map(std::sync::Arc::as_ptr);
        player.toggle(&second, &mut stop).expect("switches");
        assert!(
            player
                .active
                .as_ref()
                .is_some_and(|active| active.path == second),
            "the new file owns the player"
        );
        // Generations restart per activation, so Arc identity (not the
        // counter value) proves the switch: stop() retires the old Arc
        // while the new activation owns a fresh one.
        let current = player
            .active
            .as_ref()
            .map(|active| std::sync::Arc::as_ptr(&active.generation));
        assert!(
            retired.is_some() && current.is_some() && retired != current,
            "the old engine retired"
        );
        // Hold the retired counter past the comparison: without this clone
        // the old allocation could be freed by `stop()` and reused by the
        // new activation, making two different engines share one address.
        drop(retired_arc);
        assert!(
            matches!(player.poll(&ctx, &first), State::Loading),
            "no old picture lands on the new file"
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        drive_until(&mut player, &ctx, &second, deadline, |position, _| {
            position >= Duration::from_secs(1)
        });
        player.stop();
        assert!(player.active.is_none(), "close drops the clip");
        assert!(
            matches!(player.poll(&ctx, &second), State::Loading),
            "nothing plays after close"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn seek_readiness_reports_frame_and_audio() {
        // Seek latency split: frame-available vs audio-ready vs landed.
        // Clip 64x64 baseline 20s with tone; fractions listed below mix
        // forward, back and rapid jumps. First drive to 1s warms the file
        // cache, so samples are hot-cache steady state. Start is stamped
        // before seek(); frame and audio come from seek_diag instants,
        // landed when the loop first sees seeking false. p50 is the
        // sorted middle, p95 the second largest; raw arrays print below
        // so p50/p95 recompute by hand. Frame time here proves picture
        // readiness only, never synced sound or on-screen fluidity.
        // Debug runs 4 samples for speed; release runs 22 for p50/p95.
        let dir = std::env::temp_dir().join(format!("vespera-seekbench-{}", std::process::id()));
        let Some(path) = sample_clip_seconds(&dir, 20) else {
            eprintln!("skipped: ffmpeg unavailable for fixtures");
            return;
        };
        let clip = probe(&path).expect("the header reads");
        let total = clip.duration;
        let engine = if clip.ffmpeg { "ffmpeg" } else { "in-process" };
        let ctx = egui::Context::default();
        let mut player = Player::default();
        let mut stop = || {};
        player.toggle(&path, &mut stop).expect("opens");
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        drive_until(&mut player, &ctx, &path, deadline, |position, _| {
            position >= Duration::from_secs(1)
        });
        #[cfg(debug_assertions)]
        let fractions: Vec<f32> = vec![0.75, 0.25, 0.6, 0.4];
        #[cfg(not(debug_assertions))]
        let fractions: Vec<f32> = vec![
            0.1, 0.8, 0.3, 0.7, 0.2, 0.9, 0.4, 0.6, 0.15, 0.85, 0.35, 0.65, 0.05, 0.95, 0.45, 0.55,
            0.25, 0.75, 0.5, 0.33, 0.66, 0.44,
        ];
        let mut frames_ms: Vec<u128> = Vec::new();
        let mut landed_ms: Vec<u128> = Vec::new();
        let mut audio_ms: Vec<u128> = Vec::new();
        let mut audio_missing = 0;
        for fraction in &fractions {
            player.seek(&path, *fraction).expect("jumps");
            let target = total.mul_f32(*fraction);
            let start = std::time::Instant::now();
            let mut first: Option<u128> = None;
            let mut audio: Option<u128> = None;
            let landed = loop {
                match player.poll(&ctx, &path) {
                    State::Showing {
                        position, seeking, ..
                    } => {
                        if let Some(active) = player.active.as_ref()
                            && let Some(diag) = active.seek_diag.as_ref()
                        {
                            if first.is_none()
                                && let Some(at) = diag.first_frame
                            {
                                first = Some(at.duration_since(start).as_millis());
                            }
                            if audio.is_none()
                                && let Some(at) = diag.audio_ready
                            {
                                audio = Some(at.duration_since(start).as_millis());
                            }
                        }
                        if !seeking {
                            assert!(
                                position >= target.saturating_sub(Duration::from_millis(500)),
                                "seek lands near target"
                            );
                            assert!(
                                position <= target + Duration::from_secs(3),
                                "seek does not overshoot"
                            );
                            break start.elapsed().as_millis();
                        }
                    }
                    State::Loading => {}
                    State::Unsupported(why) => panic!("clip plays: {why}"),
                }
                assert!(start.elapsed() < Duration::from_secs(25), "seek lands");
            };
            // Landing itself proves a live frame showed, so a missed
            // observation still bounds frame availability by landing.
            frames_ms.push(first.unwrap_or(landed));
            landed_ms.push(landed);
            match audio {
                Some(ms) => audio_ms.push(ms),
                None => audio_missing += 1,
            }
        }
        fn pct(mut values: Vec<u128>, q: f64) -> u128 {
            values.sort_unstable();
            values[((values.len() as f64 * q).floor() as usize).min(values.len() - 1)]
        }
        let mut raw_frames = frames_ms.clone();
        raw_frames.sort_unstable();
        let mut raw_landed = landed_ms.clone();
        raw_landed.sort_unstable();
        eprintln!(
            "seek-bench engine={engine} samples={} frame_p50={}ms frame_p95={}ms audio_samples={} audio_p50={}ms audio_p95={}ms audio_missing={} landed_p50={}ms landed_p95={}ms",
            fractions.len(),
            pct(raw_frames.clone(), 0.5),
            pct(raw_frames.clone(), 0.95),
            audio_ms.len(),
            if audio_ms.is_empty() {
                0
            } else {
                pct(audio_ms.clone(), 0.5)
            },
            if audio_ms.is_empty() {
                0
            } else {
                pct(audio_ms.clone(), 0.95)
            },
            audio_missing,
            pct(raw_landed.clone(), 0.5),
            pct(raw_landed.clone(), 0.95)
        );
        eprintln!("seek-raw frames_ms={raw_frames:?} landed_ms={raw_landed:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn high_profile_without_offsets_stays_in_process() {
        // High profile alone proves nothing: no composition offsets means
        // no reorder, so the fast path stays instead of demanding ffmpeg.
        let dir = std::env::temp_dir().join(format!("vespera-highprof-{}", std::process::id()));
        let Some(path) = sample_clip_profile(&dir, "high-bf0.mp4", 2, "high", "0") else {
            return;
        };
        let clip = probe(&path).expect("the header reads");
        assert!(!clip.ffmpeg, "offsets, not profile, decide the engine");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn distant_keyframes_still_land() {
        // Twenty seconds with a key frame every fifteen: the widening
        // windows must still find the one behind the target.
        let dir = std::env::temp_dir().join(format!("vespera-fargop-{}", std::process::id()));
        let Some(path) = sample_clip_gop(&dir, 20, 150) else {
            eprintln!("skipped: ffmpeg unavailable for fixtures");
            return;
        };
        let clip = probe(&path).expect("the header reads");
        let total = clip.duration;
        let ctx = egui::Context::default();
        let mut player = Player::default();
        let mut stop = || {};
        player.toggle(&path, &mut stop).expect("opens");
        player.seek(&path, 0.5).expect("jumps");
        let target = total.mul_f32(0.5);
        let deadline = std::time::Instant::now() + Duration::from_secs(25);
        let (position, _) = drive_until(&mut player, &ctx, &path, deadline, |position, _| {
            position >= target.saturating_sub(Duration::from_secs(2))
        });
        assert!(
            position <= target + Duration::from_secs(5),
            "the far jump lands: {position:?} for {target:?}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Ten seconds of picture with two seconds of tone, or nothing.
    fn sample_short_audio(dir: &std::path::Path) -> Option<std::path::PathBuf> {
        std::fs::create_dir_all(dir).ok()?;
        let path = dir.join("short-audio.mp4");
        let made = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y"])
            .args(["-f", "lavfi", "-i", "color=c=green:s=64x64:d=10:r=10"])
            .args(["-f", "lavfi", "-i", "sine=frequency=520:duration=2"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .args(["-profile:v", "baseline", "-bf", "0"])
            .args(["-g", "10", "-keyint_min", "10", "-sc_threshold", "0"])
            .args(["-c:a", "aac", "-movflags", "+faststart"])
            .arg(&path)
            .status()
            .is_ok_and(|status| status.success());
        if !made {
            eprintln!("skipped: ffmpeg could not mix a short soundtrack");
            return None;
        }
        Some(path)
    }

    #[test]
    fn short_soundtrack_finishes_with_the_picture() {
        // The tone ends at two seconds, the picture at ten: reaching past
        // the soundtrack must finish on the last picture, never freeze.
        let dir = std::env::temp_dir().join(format!("vespera-shortaac-{}", std::process::id()));
        let Some(path) = sample_short_audio(&dir) else {
            return;
        };
        let clip = probe(&path).expect("the header reads");
        assert!(clip.has_audio, "the short tone is seen");
        let total = clip.duration;
        assert!(total >= Duration::from_secs(9), "ten seconds: {total:?}");
        let ctx = egui::Context::default();
        let mut player = Player::default();
        let mut stop = || {};
        player.toggle(&path, &mut stop).expect("opens");
        player.seek(&path, 0.9).expect("jumps past the soundtrack");
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let (position, playing) =
            drive_until(&mut player, &ctx, &path, deadline, |position, playing| {
                !playing && position >= total.saturating_sub(Duration::from_millis(200))
            });
        assert!(!playing, "past-audio playback finishes");
        assert!(position >= total.saturating_sub(Duration::from_millis(200)));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn hevc_falls_back_with_ffmpeg() {
        // iPhone-style codec: no parameter sets for openh264, so the
        // header refuses and ffmpeg gets its chance.
        let dir = std::env::temp_dir().join(format!("vespera-hevc-{}", std::process::id()));
        if std::fs::create_dir_all(&dir).is_err() {
            eprintln!("skipped: cannot stage an HEVC fixture");
            return;
        }
        let path = dir.join("hevc.mp4");
        let made = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y"])
            .args(["-f", "lavfi", "-i", "color=c=yellow:s=64x64:d=2:r=10"])
            .args(["-f", "lavfi", "-i", "sine=frequency=480:duration=2"])
            .args(["-c:v", "libx265", "-c:a", "aac", "-shortest"])
            .arg(&path)
            .status()
            .is_ok_and(|status| status.success());
        if !made {
            eprintln!("skipped: ffmpeg without libx265 for fixtures");
            return;
        }
        if cfg!(windows) {
            let clip = probe(&path).expect("Windows keeps HEVC for the system decoder");
            assert!(!clip.ffmpeg);
        } else {
            assert!(probe(&path).is_err(), "no in-process header for HEVC");
        }
        if !ffmpeg_present() {
            assert!(probe_ffmpeg(&path).is_err());
            let _ = std::fs::remove_dir_all(dir);
            return;
        }
        let clip = probe_ffmpeg(&path).expect("ffmpeg reads HEVC");
        assert!(clip.ffmpeg);
        let ctx = egui::Context::default();
        let mut player = Player::default();
        let mut stop = || {};
        player.toggle(&path, &mut stop).expect("opens");
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let (position, _) = drive_until(&mut player, &ctx, &path, deadline, |position, _| {
            position >= Duration::from_secs(1)
        });
        assert!(
            position >= Duration::from_secs(1),
            "HEVC plays: {position:?}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn corrupt_samples_fail_loud_then_fall_back() {
        // A valid baseline clip with a zeroed stretch of picture data:
        // faststart keeps metadata first, so the middle is sample bytes.
        let dir = std::env::temp_dir().join(format!("vespera-corrupt-{}", std::process::id()));
        let Some(path) = sample_clip_seconds(&dir, 6) else {
            eprintln!("skipped: ffmpeg unavailable for fixtures");
            return;
        };
        let mut bytes = std::fs::read(&path).expect("reads");
        // Most of the picture bytes: past the consecutive-failure budget
        // with room to spare, while the headers stay intact.
        let start = bytes.len() * 30 / 100;
        let end = bytes.len() * 95 / 100;
        for byte in &mut bytes[start..end] {
            *byte = 0;
        }
        std::fs::write(&path, &bytes).expect("writes");
        // The decode task says Error with counts, never silent starvation.
        let (tx, rx) = sync_channel::<DecodeMsg>(16);
        let generation = Arc::new(AtomicU64::new(1));
        let watch = Arc::clone(&generation);
        let alive = move || watch.load(Ordering::SeqCst) == 1;
        let control = PlaybackControl::new(Duration::ZERO, false, 1.0);
        let total = probe(&path)
            .map(|clip| clip.duration)
            .unwrap_or(Duration::from_secs(6));
        let mut system_played = false;
        std::thread::scope(|scope| {
            let held = tx.clone();
            let direct = path.clone();
            scope.spawn(move || {
                decode(
                    &direct,
                    Duration::ZERO,
                    total,
                    &alive,
                    &held,
                    &control,
                    &generation,
                    1,
                )
            });
            drop(tx);
            let mut frames = 0;
            let mut saw_error = false;
            while let Ok(message) = rx.recv() {
                match message {
                    // Four pictures answer "can this engine play the file at
                    // all"; reading a whole damaged file to the end would
                    // make the test slow without telling it anything new.
                    DecodeMsg::Frame(_) => {
                        frames += 1;
                        if frames >= 4 {
                            break;
                        }
                    }
                    DecodeMsg::End(_) => break,
                    DecodeMsg::Error(error, _) => {
                        assert!(
                            error.engine == "in-process" || error.engine == "media-foundation",
                            "the decoder that failed names itself"
                        );
                        if error.engine == "in-process" {
                            assert!(
                                error.samples > error.produced,
                                "counts tell stall from bad luck"
                            );
                        }
                        saw_error = true;
                        break;
                    }
                }
            }
            // The in-process decoder must say so loudly instead of going
            // quiet: a damaged file that produced no picture is how the
            // viewer learns to try another engine.
            system_played = frames > 0 && !saw_error;
            if !system_played {
                assert!(saw_error, "loud failure, {frames} frames first");
            }
        });
        if system_played {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        // And the player falls back instead of refusing a playable file.
        if !ffmpeg_present() {
            eprintln!("skipped: no ffmpeg for the fallback leg");
            let _ = std::fs::remove_dir_all(dir);
            return;
        }
        let ctx = egui::Context::default();
        let mut player = Player::default();
        let mut stop = || {};
        player.toggle(&path, &mut stop).expect("opens");
        let deadline = std::time::Instant::now() + Duration::from_secs(25);
        let (position, _) = drive_until(&mut player, &ctx, &path, deadline, |position, _| {
            position >= Duration::from_secs(1)
        });
        assert!(
            position >= Duration::from_secs(1),
            "fallback plays: {position:?}"
        );
        // The player must reach pictures for this file: either it fell back
        // to ffmpeg, or the system decoder played the damaged stretch, which
        // is a correct outcome on Windows and leaves nothing to fall back
        // from. What may never happen is silence: `drive_until` already
        // refuses a file that reports itself unplayable, and a carried decode
        // error here would mean the viewer was left with no engine.
        let active = player.active.as_ref().expect("still open");
        assert!(
            active.decode_error.is_none(),
            "a damaged file keeps an engine playing: {:?}",
            active
                .decode_error
                .as_ref()
                .map(|error| error.reason.as_str())
        );
        assert!(
            active.shown != Duration::MAX,
            "a picture reached the screen"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn seek_while_playing_holds_position_until_landed() {
        // The joint transition, with or without an audio device: while
        // seeking, the clock holds the target instead of running ahead;
        // only the landing frame releases picture and sound together.
        let dir = std::env::temp_dir().join(format!("vespera-freeze-{}", std::process::id()));
        let Some(path) = sample_clip_seconds(&dir, 20) else {
            eprintln!("skipped: ffmpeg unavailable for fixtures");
            return;
        };
        let clip = probe(&path).expect("the header reads");
        let total = clip.duration;
        let ctx = egui::Context::default();
        let mut player = Player::default();
        let mut stop = || {};
        player.toggle(&path, &mut stop).expect("opens");
        player.seek(&path, 0.75).expect("jumps while playing");
        let target = total.mul_f32(0.75);
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            match player.poll(&ctx, &path) {
                State::Showing {
                    position,
                    seeking,
                    playing,
                    ..
                } => {
                    assert!(playing, "a playing jump keeps playing");
                    if seeking {
                        assert_eq!(position, target, "the clock holds the target mid-seek");
                    } else {
                        break;
                    }
                }
                State::Loading => {}
                State::Unsupported(why) => panic!("the sample clip plays: {why}"),
            }
            assert!(std::time::Instant::now() < deadline, "the jump lands");
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn stopping_really_stops_playback() {
        // Cessation, not just hiding: after stop no decoder, task or sink
        // may answer with a picture anymore.
        let dir = std::env::temp_dir().join(format!("vespera-stop-{}", std::process::id()));
        let Some(path) = sample_clip_seconds(&dir, 4) else {
            eprintln!("skipped: ffmpeg unavailable for fixtures");
            return;
        };
        let ctx = egui::Context::default();
        let mut player = Player::default();
        let mut stop = || {};
        player.toggle(&path, &mut stop).expect("opens");
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        drive_until(&mut player, &ctx, &path, deadline, |position, _| {
            position >= Duration::from_secs(1)
        });
        assert!(player.is_active(&path), "really playing first");
        player.stop();
        assert!(!player.is_active(&path));
        for _ in 0..5 {
            match player.poll(&ctx, &path) {
                State::Loading => {}
                _ => panic!("nothing answers after stop"),
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn a_chat_video_probes_and_decodes_frames_in_order() {
        // Its own folder: the audio test uses a similar name in this process.
        let dir = std::env::temp_dir().join(format!("vespera-playback-{}", std::process::id()));
        let Some(path) = sample_clip(&dir) else {
            return;
        };
        let clip = probe(&path).expect("the header reads");
        assert!(clip.has_audio, "the tone is seen");
        assert!(
            clip.duration >= Duration::from_secs(1),
            "about two seconds: {:?}",
            clip.duration
        );
        assert_eq!((clip.width, clip.height), (64, 64));
        let (tx, rx) = sync_channel::<DecodeMsg>(16);
        let generation = Arc::new(AtomicU64::new(1));
        let watch = Arc::clone(&generation);
        let alive = move || watch.load(Ordering::SeqCst) == 1;
        let control = PlaybackControl::new(Duration::ZERO, false, 1.0);
        // Decoding runs beside the test, like the viewer does: the channel
        // holds 16 frames and the test drains it.
        let total = clip.duration;
        std::thread::scope(|scope| {
            let held = tx.clone();
            scope.spawn(move || {
                decode(
                    &path,
                    Duration::ZERO,
                    total,
                    &alive,
                    &held,
                    &control,
                    &generation,
                    1,
                )
            });
            drop(tx);
            let mut pts: Vec<Duration> = Vec::new();
            let mut sizes = 0;
            while let Ok(message) = rx.recv() {
                let DecodeMsg::Frame(frame) = message else {
                    break;
                };
                if let Some(last) = pts.last() {
                    assert!(
                        frame.pts >= *last,
                        "frames stay in presentation order: {pts:?} then {:?}",
                        frame.pts
                    );
                }
                sizes += 1;
                pts.push(frame.pts);
            }
            assert!(sizes >= 10, "most of the twenty frames arrive");
        });
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn every_layout_probes_and_sounds() {
        // Metadata first, metadata last, and fragments: the soundtrack must
        // decode on all of them, or the player goes quiet without saying why.
        // Runners without a sound card have no output device, so `audio_at`
        // answers Silent there even when the file decodes; Silent only
        // passes when no device exists.
        let no_sink = rodio::DeviceSinkBuilder::open_default_sink().is_err();
        let dir = std::env::temp_dir().join(format!("vespera-layouts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("creates");
        let mut made = 0;
        for (name, flags) in [
            ("fast.mp4", vec!["+faststart"]),
            ("slow.mp4", vec![]),
            ("frag.mp4", vec!["+frag_keyframe", "+empty_moov"]),
        ] {
            let Some(path) = sample_variant(&dir, name, &flags) else {
                continue;
            };
            made += 1;
            // The player tries the in-process header first, then ffmpeg.
            let clip = probe(&path)
                .or_else(|_| probe_ffmpeg(&path))
                .unwrap_or_else(|error| panic!("{name}: the header reads: {error}"));
            let expect_ffmpeg = name == "frag.mp4";
            assert_eq!(clip.ffmpeg, expect_ffmpeg, "{name} picks its engine");
            assert!(clip.has_audio, "{name} carries sound");
            // Streaming or background extraction: every layout must sound.
            let generation = Arc::new(AtomicU64::new(1));
            match audio_at(&path, Duration::ZERO, &generation, 1) {
                Audio::Sound(_) | Audio::Extracting(_) => {}
                Audio::Silent => assert!(no_sink, "{name} sounds"),
            }
        }
        if made == 0 {
            return;
        }
        assert_eq!(made, 3, "all three layouts are made");
        let _ = std::fs::remove_dir_all(dir);
    }
    /// Makes a two-second video-only clip, or nothing without ffmpeg.
    fn sample_silent(dir: &std::path::Path) -> Option<std::path::PathBuf> {
        std::fs::create_dir_all(dir).ok()?;
        let path = dir.join("clip-silent.mp4");
        let made = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y"])
            .args(["-f", "lavfi", "-i", "color=c=green:s=64x64:d=2:r=10"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .args(["-profile:v", "baseline", "-bf", "0", "-an"])
            .arg(&path)
            .status()
            .is_ok_and(|status| status.success());
        made.then_some(path)
    }

    fn player_position(player: &Player) -> Option<Duration> {
        player.active.as_ref().map(|active| active.position())
    }

    #[test]
    fn pause_freezes_and_resume_continues() {
        let dir = std::env::temp_dir().join(format!("vespera-pause-{}", std::process::id()));
        let Some(path) = sample_silent(&dir) else {
            eprintln!("skipped: ffmpeg unavailable for fixtures");
            return;
        };
        let ctx = egui::Context::default();
        let mut player = Player::default();
        player.toggle(&path, &mut || {}).expect("opens");
        player.seek(&path, 0.5).expect("seeks");
        // Let the jump land first: the clock only runs once live frames
        // arrive and clear the seeking flag, like in the viewer.
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            let landed = player.active.as_ref().is_none_or(|active| !active.seeking);
            if landed {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "the jump lands");
            player.poll(&ctx, &path);
            std::thread::sleep(Duration::from_millis(20));
        }
        player.toggle(&path, &mut || {}).expect("pauses");
        let frozen = player_position(&player).expect("a clock");
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(player_position(&player), Some(frozen), "paused holds base");
        player.toggle(&path, &mut || {}).expect("resumes");
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            player_position(&player).expect("a clock") > frozen,
            "resumed advances"
        );
        player.stop();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn replay_from_end_restarts_playing() {
        // Its own folder: replaying_a_finished_video_starts_playing owns
        // vespera-replay in this process and both remove their folder.
        let dir = std::env::temp_dir().join(format!("vespera-replay-end-{}", std::process::id()));
        let Some(path) = sample_silent(&dir) else {
            eprintln!("skipped: ffmpeg unavailable for fixtures");
            return;
        };
        let ctx = egui::Context::default();
        let mut player = Player::default();
        player.toggle(&path, &mut || {}).expect("opens");
        player.seek(&path, 1.0).expect("seeks to the end");
        let mut finished = false;
        for _ in 0..100 {
            if let State::Showing { finished: done, .. } = player.poll(&ctx, &path) {
                finished = done;
                if done {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(finished, "the end state arrives");
        player.toggle(&path, &mut || {}).expect("replays");
        let position = player_position(&player).expect("a clock");
        assert!(
            position < Duration::from_secs(2),
            "replay restarts from the beginning: {position:?}"
        );
        player.stop();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn seek_drops_the_old_soundtrack_without_doubling() {
        let dir = std::env::temp_dir().join(format!("vespera-seeksound-{}", std::process::id()));
        let Some(path) = sample_clip(&dir) else {
            eprintln!("skipped: ffmpeg unavailable for fixtures");
            return;
        };
        let mut player = Player::default();
        player.toggle(&path, &mut || {}).expect("opens");
        player.seek(&path, 0.5).expect("seeks");
        // One soundtrack at most: the jump either attached the cached one
        // or dropped the old one while the new opens beside it.
        let sounding = player
            .active
            .as_ref()
            .map(|active| active.audio.is_some())
            .unwrap_or(false);
        let extracting = player
            .active
            .as_ref()
            .map(|active| active.audio_rx.is_some() || active.audio_task.is_some())
            .unwrap_or(false);
        assert!(!sounding || !extracting, "no old sink beside a new opening");
        // Switching files retires the whole previous playback.
        let other = dir.join("other.mp4");
        std::fs::copy(&path, &other).expect("copies");
        player.toggle(&other, &mut || {}).expect("switches");
        assert!(!player.is_active(&path), "the old clip is gone");
        assert!(player.is_active(&other));
        player.stop();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn silent_clip_plays_without_soundtrack() {
        let dir = std::env::temp_dir().join(format!("vespera-silent-{}", std::process::id()));
        let Some(path) = sample_silent(&dir) else {
            eprintln!("skipped: ffmpeg unavailable for fixtures");
            return;
        };
        let clip = probe(&path).expect("the header reads");
        assert!(!clip.has_audio, "video only");
        let ctx = egui::Context::default();
        let mut player = Player::default();
        player.toggle(&path, &mut || {}).expect("opens");
        assert!(
            player
                .active
                .as_ref()
                .is_some_and(|active| active.audio.is_none()),
            "nothing to attach"
        );
        let mut first = None;
        for _ in 0..50 {
            if let State::Showing { position, .. } = player.poll(&ctx, &path) {
                first = Some(position);
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let first = first.expect("frames show");
        std::thread::sleep(Duration::from_millis(300));
        let later = player_position(&player).expect("a clock");
        assert!(
            later >= first,
            "the wall clock drives on: {first:?} then {later:?}"
        );
        player.stop();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn invalid_file_refuses_once() {
        let dir = std::env::temp_dir().join(format!("vespera-badclip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("creates");
        let path = dir.join("bad.mp4");
        std::fs::write(&path, b"not a video").expect("writes");
        let ctx = egui::Context::default();
        let mut player = Player::default();
        assert!(player.toggle(&path, &mut || {}).is_err());
        assert!(player.refusal(&path).is_some(), "complains once");
        assert!(matches!(player.poll(&ctx, &path), State::Unsupported(_)));
        assert!(player.toggle(&path, &mut || {}).is_err(), "still refused");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn switching_files_never_shows_the_old_clip() {
        let dir = std::env::temp_dir().join(format!("vespera-switch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("creates");
        let first = dir.join("first.mp4");
        let second = dir.join("second.mp4");
        std::fs::write(&first, b"not a video").expect("writes");
        std::fs::write(&second, b"also not a video").expect("writes");
        let ctx = egui::Context::default();
        let mut player = Player::default();
        assert!(player.toggle(&first, &mut || {}).is_err());
        assert!(player.refusal(&first).is_some());
        assert!(player.refusal(&second).is_none());
        assert!(matches!(player.poll(&ctx, &second), State::Loading));
        let _ = std::fs::remove_dir_all(dir);
    }

    // Drives one preview round trip to the matching answer.
    fn await_preview(
        previewer: &mut Previewer,
        path: &std::path::Path,
        generation: u64,
        fraction: f32,
        total: Duration,
    ) -> PreviewReady {
        previewer.request(path, generation, fraction, total);
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(ready) = previewer.poll(path, generation)
                && (ready.fraction - fraction).abs() < 0.001
                && !ready.approximate
            {
                return ready;
            }
            assert!(Instant::now() < deadline, "preview answers");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn preview_returns_nearby_frame() {
        let dir = std::env::temp_dir().join(format!("vespera-preview-{}", std::process::id()));
        let Some(path) =
            sample_clip_sized(&dir, "base.mp4", "640x360", 6, "baseline", "0", "10", true)
        else {
            eprintln!("skipped: ffmpeg unavailable for fixtures");
            return;
        };
        let total = probe(&path).expect("the header reads").duration;
        let mut previewer = Previewer::default();
        let generation = previewer.begin(&path);
        for fraction in [0.1f32, 0.5, 0.9] {
            let ready = await_preview(&mut previewer, &path, generation, fraction, total);
            let target = total.mul_f32(fraction);
            let gap = ready.pts.abs_diff(target);
            assert!(
                gap <= Duration::from_millis(1500),
                "preview near {fraction}"
            );
            assert!(ready.samples <= PREVIEW_MAX_SAMPLES);
            assert!(ready.image.width() <= 320 && ready.image.height() > 0);
            assert!(!ready.image.pixels.is_empty());
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn preview_covers_bframes_vfr_gaps_silence_and_refusals() {
        let dir = std::env::temp_dir().join(format!("vespera-preview-mix-{}", std::process::id()));
        let mut previewer = Previewer::default();
        // B-frames: the fast path answers an approximate neighbor.
        if let Some(path) = sample_clip_sized(&dir, "bf.mp4", "640x360", 4, "main", "3", "10", true)
        {
            let total = probe(&path).expect("the header reads").duration;
            let generation = previewer.begin(&path);
            let ready = await_preview(&mut previewer, &path, generation, 0.5, total);
            assert!(ready.pts <= total);
        }
        // Uneven gaps: any real timestamp answers.
        if let Some(path) = sample_clip_vfr_sized(&dir, "vfrm.mp4", "640x360") {
            let total = probe(&path).expect("the header reads").duration;
            let generation = previewer.begin(&path);
            let ready = await_preview(&mut previewer, &path, generation, 0.6, total);
            assert!(ready.pts <= total);
        }
        // Fifteen-second keyframe gaps stay within the sample budget.
        if let Some(path) =
            sample_clip_sized(&dir, "gop.mp4", "640x360", 20, "baseline", "0", "150", true)
        {
            let total = probe(&path).expect("the header reads").duration;
            let generation = previewer.begin(&path);
            let ready = await_preview(&mut previewer, &path, generation, 0.5, total);
            assert!(ready.samples <= PREVIEW_MAX_SAMPLES);
        }
        // No soundtrack: the preview ignores audio by construction.
        if let Some(path) =
            sample_clip_sized(&dir, "mute.mp4", "640x360", 3, "baseline", "0", "10", false)
        {
            let total = probe(&path).expect("the header reads").duration;
            let generation = previewer.begin(&path);
            await_preview(&mut previewer, &path, generation, 0.5, total);
        }
        // HEVC: clean failure, no ffmpeg process from a preview.
        if let Some(path) = sample_hevc(&dir, "hevc.mp4") {
            let total = probe_ffmpeg(&path)
                .map(|clip| clip.duration)
                .unwrap_or(Duration::from_secs(2));
            assert!(preview_frame(&path, total.mul_f32(0.5), total).is_err());
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn preview_approximate_arrives_before_exact() {
        // Distant keyframes force several deltas after the keyframe: the staged
        // path must hand an early keyframe approximate before the exact near
        // the target, with timings proving the approximate led the exact.
        let dir =
            std::env::temp_dir().join(format!("vespera-preview-approx-{}", std::process::id()));
        let Some(path) = sample_clip_sized(
            &dir,
            "approx.mp4",
            "640x360",
            20,
            "baseline",
            "0",
            "150",
            true,
        ) else {
            eprintln!("skipped: ffmpeg unavailable for fixtures");
            return;
        };
        let total = probe(&path).expect("the header reads").duration;
        let target = total.mul_f32(0.5);
        let staged = preview_staged(&path, target, total, &|| false, &mut None).expect("decodes");
        let gap = staged.exact.pts.abs_diff(target);
        assert!(
            gap <= Duration::from_millis(1500),
            "exact near target: {gap:?}"
        );
        assert!(staged.exact.image.width() <= 320, "preview stays narrow");
        assert!(
            staged.timings.samples <= PREVIEW_MAX_SAMPLES,
            "bounded decode"
        );
        if let Some(approx) = staged.approx.as_ref() {
            assert!(approx.pts <= staged.exact.pts, "approximate leads exact");
            let approx_ms = staged.timings.approx_ms.expect("approximate timed");
            assert!(
                approx_ms <= staged.timings.total_ms,
                "approximate leads total"
            );
            eprintln!(
                "preview-staged approx_ms={}ms total_ms={}ms open={}ms index={}ms decode={}ms resize={}ms samples={}",
                approx_ms,
                staged.timings.total_ms,
                staged.timings.open_ms,
                staged.timings.index_ms,
                staged.timings.decode_ms,
                staged.timings.resize_ms,
                staged.timings.samples
            );
        } else {
            eprintln!(
                "preview-staged single total_ms={}ms samples={}",
                staged.timings.total_ms, staged.timings.samples
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn superseded_decode_still_hands_over_its_approximate() {
        // Two stacked drag targets on a slow GOP-distante clip: the first
        // decode loses the race, but its early approximate must still paint
        // instead of vanishing with the stale exact. Fails on the old skip
        // path, which discarded the whole staged result.
        let dir =
            std::env::temp_dir().join(format!("vespera-preview-supersede-{}", std::process::id()));
        let Some(path) = sample_clip_sized(
            &dir,
            "supersede.mp4",
            "640x360",
            20,
            "baseline",
            "0",
            "150",
            true,
        ) else {
            eprintln!("skipped: ffmpeg unavailable for fixtures");
            return;
        };
        let total = probe(&path).expect("the header reads").duration;
        let mut previewer = Previewer::default();
        let generation = previewer.begin(&path);
        // Warm the worker up first: without this, thread-spawn latency under
        // parallel load can let both requests sit in the channel, so the first
        // is drained instead of decoded and no abort ever fires. After warmup
        // the worker idles in recv and grabs the next request at once.
        previewer.request(&path, generation, 0.0, total);
        let deadline = Instant::now() + Duration::from_secs(25);
        loop {
            if let Some(ready) = previewer.poll(&path, generation)
                && (ready.fraction - 0.0).abs() < 0.001
                && !ready.approximate
            {
                break;
            }
            assert!(Instant::now() < deadline, "worker warms up");
            std::thread::sleep(Duration::from_millis(10));
        }
        previewer.request(&path, generation, 0.5, total);
        previewer.request(&path, generation, 0.9, total);
        let mut saw_first_approx = false;
        loop {
            if let Some(ready) = previewer.poll(&path, generation) {
                if ready.approximate && (ready.fraction - 0.5).abs() < 0.001 {
                    saw_first_approx = true;
                }
                if !ready.approximate && (ready.fraction - 0.9).abs() < 0.001 {
                    break;
                }
            }
            assert!(
                Instant::now() < deadline,
                "the superseded approximate paints and the exact lands"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(saw_first_approx, "stale approximate still paints");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn poll_hands_approximate_before_exact_of_the_same_seq() {
        let mut previewer = Previewer::default();
        let (tx, rx) = std::sync::mpsc::channel();
        previewer.rx = Some(rx);
        let path = std::path::Path::new("clip.mp4");
        let generation = 1;
        let pixels = [255u8; 16];
        let image = ColorImage::from_rgba_unmultiplied([2, 2], &pixels);
        let ready = |approximate: bool| PreviewReady {
            path: path.to_path_buf(),
            generation,
            seq: 7,
            fraction: 0.4,
            pts: Duration::from_millis(400),
            image: image.clone(),
            samples: 1,
            approximate,
        };
        tx.send(ready(true)).unwrap();
        tx.send(ready(false)).unwrap();
        let first = previewer
            .poll(path, generation)
            .expect("approximate paints");
        assert!(first.approximate, "poll 1 is the approximate");
        assert!(previewer.cache.is_empty(), "approximate is not cached");
        let second = previewer.poll(path, generation).expect("exact follows");
        assert!(!second.approximate, "poll 2 is the exact");
        assert!(
            previewer
                .cache
                .values()
                .all(|(ready, _)| !ready.approximate),
            "only the exact is cached"
        );
        assert!(previewer.poll(path, generation).is_none());
    }

    #[test]
    fn preview_abort_yields_to_newer_target() {
        // A newer drag target must abort a stale decode instead of queueing
        // behind it: an abort flag that is already set fails fast with
        // an explicit marker the worker treats as silent, never as a picture.
        let dir =
            std::env::temp_dir().join(format!("vespera-preview-abort-{}", std::process::id()));
        let Some(path) =
            // Distant keyframes force dozens of samples before the target, so the
            // abort below fires mid-decode with the approximate already in hand.
            sample_clip_sized(&dir, "abort.mp4", "640x360", 20, "baseline", "0", "150", true)
        else {
            eprintln!("skipped: ffmpeg unavailable for fixtures");
            return;
        };
        let total = probe(&path).expect("the header reads").duration;
        let target = total.mul_f32(0.5);
        let mut slot = None;
        let err = preview_staged(&path, target, total, &|| true, &mut slot).expect_err("aborts");
        assert_eq!(err, "aborted", "abort marker stays explicit");
        assert!(slot.is_some(), "abort hands over its approximate");
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn preview_latency_by_resolution_reports_stages() {
        // Resolution ladder with stage splits: open, keyframe index, decode,
        // resize, plus approximate lead time. Cold is the first decode of a
        // file, warm repeats it with the OS cache hot, hit repeats the same
        // bucket through Previewer. Debug runs one fixture for speed; release
        // runs the ladder for p50/p95. Prints raw arrays so p50/p95 recompute.
        fn pct(mut values: Vec<u128>, q: f64) -> u128 {
            values.sort_unstable();
            values[((values.len() as f64 * q).floor() as usize).min(values.len() - 1)]
        }
        let dir =
            std::env::temp_dir().join(format!("vespera-preview-ladder-{}", std::process::id()));
        let ladder: Vec<(&str, &str, u32, &str, &str, &str, bool)> = if cfg!(debug_assertions) {
            vec![("base-480p.mp4", "854x480", 8, "baseline", "0", "30", true)]
        } else {
            vec![
                ("base-480p.mp4", "854x480", 8, "baseline", "0", "30", true),
                ("base-720p.mp4", "1280x720", 6, "baseline", "0", "30", true),
                (
                    "base-1080p.mp4",
                    "1920x1080",
                    4,
                    "baseline",
                    "0",
                    "30",
                    true,
                ),
                ("gop-720p.mp4", "1280x720", 8, "baseline", "0", "150", true),
                ("bf-720p.mp4", "1280x720", 4, "main", "2", "30", true),
                ("vfr-480p.mp4", "854x480", 0, "", "", "", true),
                ("long-480p.mp4", "854x480", 60, "baseline", "0", "150", true),
            ]
        };
        let fractions: Vec<f32> = if cfg!(debug_assertions) {
            vec![0.5]
        } else {
            vec![0.1, 0.5, 0.9]
        };
        for (name, size, secs, profile, bf, gop, audio) in ladder {
            if name == "vfr-480p.mp4" {
                let Some(path) = sample_clip_vfr_sized(&dir, name, size) else {
                    eprintln!("skipped: ffmpeg could not encode {name}");
                    continue;
                };
                let total = probe(&path).expect("the header reads").duration;
                let mut totals: Vec<u128> = Vec::new();
                for fraction in &fractions {
                    let target = total.mul_f32(*fraction);
                    let staged = preview_staged(&path, target, total, &|| false, &mut None)
                        .expect("decodes");
                    let gap = staged.exact.pts.abs_diff(target);
                    assert!(
                        gap <= Duration::from_millis(1500),
                        "{name} exact near {fraction}: {gap:?}"
                    );
                    totals.push(staged.timings.total_ms);
                }
                eprintln!("preview-ladder {name} size={size} raw_totals={totals:?}");
                continue;
            }
            let Some(path) = sample_clip_sized(&dir, name, size, secs, profile, bf, gop, audio)
            else {
                eprintln!("skipped: ffmpeg could not encode {name}");
                continue;
            };
            let total = probe(&path).expect("the header reads").duration;
            let mut totals: Vec<u128> = Vec::new();
            let mut opens: Vec<u128> = Vec::new();
            let mut indexes: Vec<u128> = Vec::new();
            let mut decodes: Vec<u128> = Vec::new();
            let mut resizes: Vec<u128> = Vec::new();
            let mut approx_lead: Vec<u128> = Vec::new();
            for fraction in &fractions {
                for round in 0..2 {
                    let target = total.mul_f32(*fraction);
                    let staged = preview_staged(&path, target, total, &|| false, &mut None)
                        .expect("decodes");
                    let is_bframes = bf != "0";
                    if is_bframes {
                        assert!(staged.exact.pts <= total, "{name} neighbor inside clip");
                    } else {
                        let gap = staged.exact.pts.abs_diff(target);
                        assert!(
                            gap <= Duration::from_millis(1500),
                            "{name} exact near {fraction}: {gap:?}"
                        );
                    }

                    assert!(staged.exact.image.width() <= 320, "{name} preview narrow");
                    totals.push(staged.timings.total_ms);
                    opens.push(staged.timings.open_ms);
                    indexes.push(staged.timings.index_ms);
                    decodes.push(staged.timings.decode_ms);
                    resizes.push(staged.timings.resize_ms);
                    if let Some(approx_ms) = staged.timings.approx_ms {
                        approx_lead.push(staged.timings.total_ms.saturating_sub(approx_ms));
                    }
                    if round == 0 {
                        eprintln!(
                            "preview-ladder {name} frac={fraction} cold total={}ms open={}ms index={}ms decode={}ms resize={}ms samples={}",
                            staged.timings.total_ms,
                            staged.timings.open_ms,
                            staged.timings.index_ms,
                            staged.timings.decode_ms,
                            staged.timings.resize_ms,
                            staged.timings.samples
                        );
                    }
                }
            }
            let mut previewer = Previewer::default();
            let generation = previewer.begin(&path);
            let first_fraction = fractions[0];
            let first = await_preview(&mut previewer, &path, generation, first_fraction, total);
            assert!(!first.approximate, "cache path lands exact");
            let hit_start = Instant::now();
            let hit = previewer.request(&path, generation, first_fraction, total);
            let hit_ms = hit_start.elapsed().as_millis();
            assert!(hit.is_some(), "{name} exact bucket hits");
            eprintln!(
                "preview-ladder {name} size={size} samples={} total_p50={}ms total_p95={}ms open_p50={}ms index_p50={}ms decode_p50={}ms resize_p50={}ms approx_lead_p50={}ms hit_ms={}ms raw_totals={totals:?}",
                fractions.len() * 2,
                pct(totals.clone(), 0.5),
                pct(totals.clone(), 0.95),
                pct(opens.clone(), 0.5),
                pct(indexes.clone(), 0.5),
                pct(decodes.clone(), 0.5),
                pct(resizes.clone(), 0.5),
                if approx_lead.is_empty() {
                    0
                } else {
                    pct(approx_lead.clone(), 0.5)
                },
                hit_ms
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }
    // new preview tests end
    #[test]
    fn preview_stale_generations_never_apply() {
        let dir =
            std::env::temp_dir().join(format!("vespera-preview-stale-{}", std::process::id()));
        let Some(path) =
            sample_clip_sized(&dir, "stale.mp4", "640x360", 4, "baseline", "0", "10", true)
        else {
            eprintln!("skipped: ffmpeg unavailable for fixtures");
            return;
        };
        let total = probe(&path).expect("the header reads").duration;
        let mut previewer = Previewer::default();
        let old = previewer.begin(&path);
        previewer.request(&path, old, 0.1, total);
        // A switch retires the whole drag: the old answer must die even
        // if the worker decoded it.
        let generation = previewer.begin(&path);
        let ready = await_preview(&mut previewer, &path, generation, 0.9, total);
        assert_eq!(ready.generation, generation);
        assert!((ready.fraction - 0.9).abs() < 0.001);
        assert!(previewer.poll(&path, old).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn preview_latest_request_wins() {
        let dir =
            std::env::temp_dir().join(format!("vespera-preview-latest-{}", std::process::id()));
        let Some(path) = sample_clip_sized(
            &dir,
            "latest.mp4",
            "640x360",
            4,
            "baseline",
            "0",
            "10",
            true,
        ) else {
            eprintln!("skipped: ffmpeg unavailable for fixtures");
            return;
        };
        let total = probe(&path).expect("the header reads").duration;
        let mut previewer = Previewer::default();
        let generation = previewer.begin(&path);
        let mut seen = Vec::new();
        for index in 0..10u32 {
            previewer.request(&path, generation, index as f32 / 10.0, total);
        }
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(ready) = previewer.poll(&path, generation) {
                if ready.approximate {
                    continue;
                }
                if seen.last().is_none_or(|last: &u64| ready.seq >= *last) {
                    seen.push(ready.seq);
                }
                if (ready.fraction - 0.9).abs() < 0.001 {
                    break;
                }
            }
            assert!(Instant::now() < deadline, "the newest answer lands");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(*seen.last().expect("answers"), previewer.seq);
        let mut ordered = seen.clone();
        ordered.sort_unstable();
        assert_eq!(seen, ordered, "answers never step back");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn preview_cache_stays_budgeted() {
        let dir =
            std::env::temp_dir().join(format!("vespera-preview-cache-{}", std::process::id()));
        let Some(path) =
            sample_clip_sized(&dir, "cache.mp4", "640x360", 4, "baseline", "0", "10", true)
        else {
            eprintln!("skipped: ffmpeg unavailable for fixtures");
            return;
        };
        let total = probe(&path).expect("the header reads").duration;
        let mut previewer = Previewer::default();
        let generation = previewer.begin(&path);
        for index in 0..20u32 {
            await_preview(
                &mut previewer,
                &path,
                generation,
                index as f32 / 20.0,
                total,
            );
        }
        assert!(previewer.order.len() <= PREVIEW_CACHE_ENTRIES);
        assert!(previewer.cached_bytes <= PREVIEW_CACHE_BYTES);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn preview_latency_reports_p50_p95() {
        // Representative pixels, not thumbnails: 854x480 decodes full
        // frames like a real chat video. Twenty sequential round trips
        // with generous deadlines; timing is reported, never asserted,
        // except that every answer lands near its target.
        let dir = std::env::temp_dir().join(format!("vespera-preview-lat-{}", std::process::id()));
        let Some(path) =
            sample_clip_sized(&dir, "lat.mp4", "854x480", 8, "baseline", "0", "10", true)
        else {
            eprintln!("skipped: ffmpeg unavailable for fixtures");
            return;
        };
        let total = probe(&path).expect("the header reads").duration;
        let mut previewer = Previewer::default();
        let generation = previewer.begin(&path);
        let mut times_ms: Vec<u128> = Vec::new();
        let mut first_ms = 0u128;
        for (index, fraction) in [
            0.05f32, 0.9, 0.2, 0.75, 0.4, 0.6, 0.15, 0.85, 0.3, 0.7, 0.1, 0.95, 0.45, 0.55, 0.25,
            0.65, 0.35, 0.5, 0.8, 0.12,
        ]
        .into_iter()
        .enumerate()
        {
            let start = Instant::now();
            let ready = await_preview(&mut previewer, &path, generation, fraction, total);
            let elapsed = start.elapsed().as_millis();
            if index == 0 {
                first_ms = elapsed;
            } else {
                times_ms.push(elapsed);
            }
            let target = total.mul_f32(fraction);
            let gap = ready.pts.abs_diff(target);
            assert!(gap <= Duration::from_millis(1500), "preview near target");
        }
        fn pct(mut values: Vec<u128>, q: f64) -> u128 {
            values.sort_unstable();
            values[((values.len() as f64 * q).floor() as usize).min(values.len() - 1)]
        }
        eprintln!(
            "preview-lat samples=20 cold_first={}ms hot_p50={}ms hot_p95={}ms hot_raw={:?}",
            first_ms,
            pct(times_ms.clone(), 0.5),
            pct(times_ms.clone(), 0.95),
            {
                let mut sorted = times_ms.clone();
                sorted.sort_unstable();
                sorted
            }
        );
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn preview_hot_repeat_hits_cache_and_warm_new_misses() {
        let dir = std::env::temp_dir().join(format!("vespera-preview-hit-{}", std::process::id()));
        let Some(path) =
            sample_clip_sized(&dir, "hit.mp4", "320x240", 4, "baseline", "0", "10", true)
        else {
            eprintln!("skipped: ffmpeg unavailable for fixtures");
            return;
        };
        let total = probe(&path).expect("the header reads").duration;
        let mut previewer = Previewer::default();
        let generation = previewer.begin(&path);
        let mut hits = 0u32;
        let mut misses = 0u32;
        assert!(
            previewer.request(&path, generation, 0.5, total).is_none(),
            "a cold destination misses"
        );
        misses += 1;
        let first = await_preview(&mut previewer, &path, generation, 0.5, total);
        for _ in 0..5 {
            let start = Instant::now();
            let ready = previewer
                .request(&path, generation, 0.5, total)
                .expect("a decoded destination hits");
            assert_eq!(ready.seq, first.seq, "the hit replays the same decode");
            assert!(
                start.elapsed() < Duration::from_millis(100),
                "a hit never decodes"
            );
            hits += 1;
        }
        for fraction in [0.1f32, 0.9, 0.3] {
            assert!(
                previewer
                    .request(&path, generation, fraction, total)
                    .is_none(),
                "a new destination misses with the file warm"
            );
            misses += 1;
            await_preview(&mut previewer, &path, generation, fraction, total);
            assert!(
                previewer
                    .request(&path, generation, fraction, total)
                    .is_some(),
                "a decoded destination hits"
            );
            hits += 1;
        }
        eprintln!("preview-cache hits={hits} misses={misses}");
        assert_eq!((hits, misses), (8, 4));
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn preview_continuous_drag_answers_mid_motion() {
        let dir = std::env::temp_dir().join(format!("vespera-preview-drag-{}", std::process::id()));
        let Some(path) =
            sample_clip_sized(&dir, "drag.mp4", "320x240", 4, "baseline", "0", "10", false)
        else {
            eprintln!("skipped: ffmpeg unavailable for fixtures");
            return;
        };
        let total = probe(&path).expect("the header reads").duration;
        let mut previewer = Previewer::default();
        let generation = previewer.begin(&path);
        let moves = 60u32;
        let mut mid_motion = 0u32;
        let mut fresher = 0u32;
        let mut top_seq = 0u64;
        for index in 0..moves {
            let fraction = index as f32 / moves as f32;
            previewer.request(&path, generation, fraction, total);
            if let Some(ready) = previewer.poll(&path, generation) {
                if ready.seq > top_seq {
                    top_seq = ready.seq;
                    if index + 1 < moves {
                        fresher += 1;
                    }
                }
                if index + 1 < moves {
                    mid_motion += 1;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let wanted = (moves - 1) as f32 / moves as f32;
        let deadline = Instant::now() + Duration::from_secs(20);
        let landed = loop {
            if let Some(ready) = previewer.poll(&path, generation)
                && (ready.fraction - wanted).abs() < 0.001
                && !ready.approximate
            {
                break ready.fraction;
            }
            assert!(Instant::now() < deadline, "the final drag answers");
            std::thread::sleep(Duration::from_millis(10));
        };
        eprintln!("preview-drag moves={moves} mid_motion={mid_motion} fresher={fresher}");
        assert!(mid_motion >= 1, "previews stay up while the pointer moves");
        assert!(
            fresher >= 1,
            "fresh decodes land mid-motion, not only replays"
        );
        assert!(
            (landed - wanted).abs() < 0.001,
            "the drag ends on its last move"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn preview_cancel_storm_retires_without_stranding() {
        let dir =
            std::env::temp_dir().join(format!("vespera-preview-storm-{}", std::process::id()));
        let Some(path) = sample_clip_sized(
            &dir,
            "storm.mp4",
            "320x240",
            4,
            "baseline",
            "0",
            "10",
            false,
        ) else {
            eprintln!("skipped: ffmpeg unavailable for fixtures");
            return;
        };
        let total = probe(&path).expect("the header reads").duration;
        let mut previewer = Previewer::default();
        for _ in 0..10 {
            let generation = previewer.begin(&path);
            previewer.request(&path, generation, 0.5, total);
            previewer.cancel(&path);
        }
        let generation = previewer.begin(&path);
        let ready = await_preview(&mut previewer, &path, generation, 0.5, total);
        assert_eq!(
            ready.generation, generation,
            "a fresh drag answers after the storm"
        );
        assert!(
            previewer.pending.is_none(),
            "no cancelled request stays queued"
        );
        assert!(
            previewer.in_flight.is_none(),
            "no cancelled flight stays tracked"
        );
        eprintln!("preview-storm retired=10 answered={}", ready.seq);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn preview_contact_sheet_shows_four_positions() {
        // Visual proof with synthetic content: one strip with the
        // decoded picture at four drag positions.
        let dir =
            std::env::temp_dir().join(format!("vespera-preview-sheet-{}", std::process::id()));
        let path = dir.join("sheet.mp4");
        std::fs::create_dir_all(&dir).ok();
        let made = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y"])
            .args(["-f", "lavfi", "-i", "testsrc2=s=640x360:d=4:r=10"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .args(["-profile:v", "baseline", "-bf", "0"])
            .args(["-g", "10", "-keyint_min", "10", "-sc_threshold", "0"])
            .args(["-an", "-movflags", "+faststart"])
            .arg(&path)
            .status()
            .is_ok_and(|status| status.success());
        if !made {
            eprintln!("skipped: ffmpeg unavailable for fixtures");
            return;
        }
        let total = probe(&path).expect("the header reads").duration;
        let mut cells = Vec::new();
        for fraction in [0.0f32, 0.33, 0.66, 1.0] {
            let target = total.mul_f32(fraction);
            let image = preview_frame(&path, target, total).expect("decodes");
            let raw = image::RgbaImage::from_raw(
                image.image.width() as u32,
                image.image.height() as u32,
                image
                    .image
                    .pixels
                    .iter()
                    .flat_map(|pixel| pixel.to_array())
                    .collect(),
            )
            .expect("rebuilds");
            cells.push(image::imageops::resize(
                &raw,
                320,
                180,
                image::imageops::FilterType::Triangle,
            ));
        }
        let mut sheet = image::RgbaImage::new(640, 360);
        for (index, cell) in cells.iter().enumerate() {
            image::imageops::overlay(
                &mut sheet,
                cell,
                ((index % 2) as i64) * 320,
                ((index / 2) as i64) * 180,
            );
        }
        let out = std::path::PathBuf::from(".local-roadmap/preview-contact.png");
        std::fs::create_dir_all(".local-roadmap").expect("the sheet folder exists");
        sheet.save(&out).expect("saves the sheet");
        assert!(out.is_file());
        let mut seen = std::collections::HashSet::new();
        for cell in &cells {
            assert!(
                seen.insert(cell.as_raw().clone()),
                "each drag position decodes its own picture"
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn output_level_follows_without_reopening() {
        // Opening a clip starts decoder threads. On a headless Windows
        // runner those threads can keep the process alive past nextest's
        // 60s budget even after the assertion has passed. The level itself
        // does not open or replace a clip.
        let mut player = Player::default();
        let path = std::path::Path::new("clip.mp4");
        player.set_output(0.3, false);
        assert!(
            !player.is_active(path),
            "a level change does not open a clip"
        );
        player.set_output(0.3, false);
        player.set_output(0.0, true);
        assert!(player.muted);
        assert_eq!(player.volume, 0.0);
        assert!(
            !player.is_active(path),
            "a later level change still does not open a clip"
        );
    }

    #[test]
    fn fragments_fall_back_to_ffmpeg_for_their_length() {
        if !ffmpeg_present() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("vespera-fraglen-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("creates");
        let Some(path) = sample_variant(&dir, "frag.mp4", &["+frag_keyframe", "+empty_moov"])
        else {
            return;
        };
        // Fragments keep no sample table the in-process reader can walk, so the
        // header honestly refuses and ffmpeg measures instead.
        assert!(
            probe(&path).is_err(),
            "fragments refuse the in-process header"
        );
        let clip = probe_ffmpeg(&path).expect("ffmpeg measures fragments");
        assert!(clip.ffmpeg, "fragments play through the ffmpeg engine");
        assert!(
            clip.duration >= Duration::from_secs(1),
            "about two seconds: {:?}",
            clip.duration
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn five_seeks_collapse_to_the_latest_target() {
        let control = PlaybackControl::new(Duration::ZERO, false, 1.0);
        let generation = AtomicU64::new(1);
        for secs in 1..=5 {
            control.set_target(Duration::from_secs(secs));
            generation.fetch_add(1, Ordering::SeqCst);
        }
        assert_eq!(control.target(), Duration::from_secs(5));
        assert_eq!(generation.load(Ordering::SeqCst), 6);
    }

    #[test]
    fn a_seek_frame_before_the_target_is_dropped() {
        let target = Duration::from_secs(10);
        assert!(frame_reaches_seek(target, target));
        assert!(frame_reaches_seek(Duration::from_secs(11), target));
        // The landing tolerance keeps the final picture of a jump to the
        // very end: one millisecond early still settles the seek, so only
        // pictures more than the tolerance early are dropped.
        assert!(frame_reaches_seek(Duration::from_millis(9_999), target));
        assert!(frame_reaches_seek(
            target.saturating_sub(SEEK_LANDING_TOLERANCE),
            target
        ));
        assert!(!frame_reaches_seek(
            target.saturating_sub(SEEK_LANDING_TOLERANCE + Duration::from_millis(1)),
            target
        ));
    }

    #[test]
    fn five_seeks_keep_one_playback_decoder() {
        let dir = std::env::temp_dir().join(format!("vespera-one-decoder-{}", std::process::id()));
        let Some(path) = sample_clip_seconds(&dir, 4) else {
            return;
        };
        reset_engine_peak();
        #[cfg(windows)]
        let (devices, opens) = (
            crate::native_video::d3d_devices_created(),
            crate::native_video::mf_decoders_opened(),
        );
        let ctx = egui::Context::default();
        let mut player = Player::default();
        let mut stop = || {};
        player.toggle(&path, &mut stop).expect("opens");
        let worker = player.active.as_ref().expect("open player").worker;
        for fraction in [0.2, 0.5, 0.8, 0.1, 0.4] {
            player.seek(&path, fraction).expect("jumps");
        }
        let _ = player.poll(&ctx, &path);
        // Per-player identity: the process-global worker counter also
        // moves for other tests' players running on other threads, so a
        // global delta would fail nondeterministically.
        assert_eq!(
            player.active.as_ref().expect("open player").worker,
            worker,
            "five seeks still use the decoder opened with the file"
        );
        assert!(
            engine_peak() <= 1,
            "seeks must not leave two decoders alive"
        );
        #[cfg(windows)]
        {
            assert!(
                crate::native_video::d3d_devices_created() - devices <= 1,
                "the D3D device is created once"
            );
            assert!(
                crate::native_video::mf_decoders_opened() - opens <= 1,
                "a seek must not open another source reader"
            );
            assert!(
                crate::native_video::mf_decoders_alive() <= 1,
                "one Media Foundation reader stays alive"
            );
        }
        player.stop();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn seek_delivers_a_frame_at_or_after_the_target() {
        assert_seek_lands_at("chat", 12, 10);
    }

    /// Status clips use the same player as chat attachments
    /// (`Action::VideoSeek` calls `Player::seek`).
    #[test]
    fn status_seek_delivers_a_frame_at_or_after_the_target() {
        assert_seek_lands_at("status", 12, 10);
    }

    fn assert_seek_lands_at(label: &str, seconds: u32, at_secs: u64) {
        let dir = std::env::temp_dir().join(format!(
            "vespera-seek-land-{label}-{}-{}",
            at_secs,
            std::process::id()
        ));
        let Some(path) = sample_clip_seconds(&dir, seconds) else {
            return;
        };
        let clip = probe(&path).expect("the header reads");
        let target = Duration::from_secs(at_secs).min(clip.duration);
        let fraction = target.as_secs_f32() / clip.duration.as_secs_f32();
        let ctx = egui::Context::default();
        let mut player = Player::default();
        let mut stop = || {};
        player.toggle(&path, &mut stop).expect("opens");
        player.seek(&path, fraction).expect("jumps");
        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        let mut landed = None;
        while std::time::Instant::now() < deadline {
            player.poll(&ctx, &path);
            landed = player.active.as_ref().and_then(|active| active.landed_pts);
            if landed.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        player.stop();
        let _ = std::fs::remove_dir_all(dir);
        let landed = landed.expect("a picture arrives after the jump");
        assert!(
            frame_reaches_seek(landed, target),
            "the first picture is at or after the jump: {landed:?} for {target:?}"
        );
    }

    #[test]
    fn scrubbing_does_not_stop_playback_frames() {
        let dir = std::env::temp_dir().join(format!("vespera-scrub-play-{}", std::process::id()));
        let Some(path) = sample_clip_seconds(&dir, 4) else {
            return;
        };
        let ctx = egui::Context::default();
        let mut player = Player::default();
        let mut stop = || {};
        player.toggle(&path, &mut stop).expect("opens");
        let preview_path = path.clone();
        let preview = std::thread::spawn(move || {
            let mut aborted = None;
            let _ = preview_staged(
                &preview_path,
                Duration::from_secs(2),
                Duration::from_secs(4),
                &|| false,
                &mut aborted,
            );
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        let mut saw_frame = false;
        while std::time::Instant::now() < deadline {
            if let State::Showing { .. } = player.poll(&ctx, &path) {
                saw_frame = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        player.stop();
        preview
            .join()
            .expect("the scrub finishes after playback releases it");
        let _ = std::fs::remove_dir_all(dir);
        assert!(saw_frame, "dragging the bar must not block playback frames");
    }

    #[test]
    fn test_playback_control_shared_pause_initial_volume_and_native_audio() {
        let control = PlaybackControl::new(Duration::from_secs(10), false, 0.35);
        let vol_bits = control.volume.load(Ordering::Acquire);
        assert_eq!(f32::from_bits(vol_bits), 0.35);

        let shared_paused = control.paused.clone();
        assert!(!shared_paused.load(Ordering::Acquire));
        control.set_paused(true);
        assert!(shared_paused.load(Ordering::Acquire));
        control.set_paused(false);
        assert!(!shared_paused.load(Ordering::Acquire));

        assert!(!control.has_native_audio());
        control.set_native_audio(true);
        assert!(control.has_native_audio());
        control.set_native_audio(false);
        assert!(!control.has_native_audio());
    }
}
