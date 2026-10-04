//! FLAC, written from RFC 9639 ("Free Lossless Audio Codec").
//!
//! - [`Decoder`]: packets of one or more whole frames (an MP4 `fLaC`
//!   sample, a Matroska `A_FLAC` block, a native stream cut at frame
//!   boundaries) to interleaved integer samples; every subframe type,
//!   wasted bits, the stereo decorrelation modes, 4–32 bits, 1–8 channels,
//!   fixed and variable block sizes; both CRCs checked, and the STREAMINFO
//!   MD5 when the stream has one ([`Decoder::md5_matches`]).
//! - [`Encoder`]: interleaved integer samples to 4096-sample frames, at one
//!   of three [`Level`]s, and the STREAMINFO ([`Encoder::stream_info`],
//!   [`Encoder::metadata_blocks`]) a container carries.
//! - The format pieces both use: [`StreamInfo`], the metadata block
//!   framing, the CRCs.

mod decode;
mod encode;
mod format;
mod verify;

pub use decode::{DecodedFrame, Decoder, FrameHeader, decode_frame, parse_frame_header};
pub use encode::{BLOCK_SIZE, Encoder, EncoderConfig, Level};
pub use format::{
    BLOCK_PADDING, BLOCK_SEEKTABLE, BLOCK_STREAMINFO, BLOCK_VORBIS_COMMENT, StreamInfo, block_header, crc8, crc16,
    md5_bytes, parse_metadata_blocks, stream_info_from_extra,
};

use crate::Speaker;

/// The speakers of a FLAC stream of `channels` channels (§9.1.3), in slot
/// order; `None` past eight. Samples go in and come out in this order.
pub fn layout(channels: u8) -> Option<&'static [Speaker]> {
    use Speaker::*;
    Some(match channels {
        1 => &[FC],
        2 => &[FL, FR],
        3 => &[FL, FR, FC],
        4 => &[FL, FR, BL, BR],
        5 => &[FL, FR, FC, BL, BR],
        6 => &[FL, FR, FC, LFE, BL, BR],
        7 => &[FL, FR, FC, LFE, BC, SL, SR],
        8 => &[FL, FR, FC, LFE, BL, BR, SL, SR],
        _ => return None,
    })
}
