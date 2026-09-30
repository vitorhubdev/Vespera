//! System video decoding on Windows.
//!
//! Media Foundation is part of the operating system. Hardware decoding is
//! used when a video-capable GPU device exists. Nothing from this module is
//! shipped as a decoder library.

mod media_foundation;

pub use media_foundation::Decoder;

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
