//! FLAC and ALAC (Apple Lossless), both ways, in pure Rust.
//!
//! - [`flac`]: a FLAC decoder and encoder, written from RFC 9639 — packets
//!   of whole frames in or out, the STREAMINFO block, the metadata block
//!   framing; 4–32 bits, 1–8 channels; both CRCs and the MD5 of the audio
//!   checked or written.
//! - [`alac`]: an ALAC decoder and encoder, written from the published
//!   format description — one frame per packet and the 24-byte magic
//!   cookie (`ALACSpecificConfig`); 16, 20, 24 and 32 bits, 1–8 channels.
//! - [`pcm`]: conversions between the integer PCM the codecs work in and
//!   f32 samples.
//!
//! Both codecs work on interleaved integer PCM (`i32`, one sample per slot,
//! at the stream's bit depth) in the channel order most multichannel PCM
//! pipelines use (FL FR FC LFE BL BR BC SL SR, those present). FLAC's own
//! order is that order; ALAC's, which leads with the centre channel, is
//! reordered on the way in and out. [`flac::layout`] and [`alac::layout`]
//! name the [`Speaker`]s for each channel count.
//!
//! Containers are the caller's: the decoders take the codec configuration
//! in the forms MP4 (`dfLa`, `alac`) and Matroska (`A_FLAC`, `A_ALAC`
//! CodecPrivate) carry it, and the encoders hand back frames and the
//! configuration to put in one.

pub mod alac;
mod bits;
mod error;
pub mod flac;
mod layout;
mod lpc;
mod md5;
mod parallel;
pub mod pcm;
mod simd;

pub use error::{Error, Result};
pub use layout::Speaker;
