//! System video decoding on Windows.
//!
//! Media Foundation is part of the operating system. Hardware decoding is
//! used when a video-capable GPU device exists. Nothing from this module is
//! shipped as a decoder library.

mod media_foundation;

pub use media_foundation::Decoder;
#[cfg(test)]
pub(crate) use media_foundation::{d3d_devices_created, mf_decoders_alive, mf_decoders_opened};

use std::io::{Read, Seek};

pub trait ReadSeek: Read + Seek + Send {}
impl<T: Read + Seek + Send> ReadSeek for T {}

#[derive(Clone, Copy, Debug)]
pub struct Info {
    pub width: u32,
    pub height: u32,
    pub duration: f64,
    pub sample_rate: u32,
    pub channels: u16,
}

pub enum Sample {
    Video {
        pts: f64,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
    Audio {
        pts: f64,
        frames: Vec<[f32; 2]>,
    },
}

/// Name of the system decoder this build plays with.
pub fn engine_name() -> &'static str {
    "Media Foundation"
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_4k_source_is_asked_at_1080p() {
        assert_eq!(
            super::media_foundation::fit_playback(3840, 2160),
            (1920, 1080)
        );
        assert_eq!(
            super::media_foundation::fit_playback(1280, 720),
            (1280, 720)
        );
        assert_eq!(super::media_foundation::fit_playback(640, 360), (640, 360));
    }
}
