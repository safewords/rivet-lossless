//! ALAC (Apple Lossless), written from the published format description.
//!
//! - [`Decoder`]: one frame per packet (an MP4 `alac` sample, a Matroska
//!   `A_ALAC` block) to interleaved integer samples, given the magic cookie
//!   ([`Config`]); 16, 20, 24 and 32 bits, 1–8 channels.
//! - [`Encoder`]: interleaved integer samples to 4096-sample frames, and the
//!   cookie ([`Encoder::cookie`]) a container carries.
//! - The format pieces both use: the cookie, the element layout of each
//!   channel count, the adaptive predictor.
//!
//! ALAC's channel layouts lead with the centre channel; the decoder and the
//! encoder reorder between them and the native order [`layout`] names.

mod decode;
mod encode;
mod format;

pub use decode::{Decoder, decode_frame};
pub use encode::Encoder;
pub use format::{
    Config, DEFAULT_FRAME_LENGTH, DEFAULT_KB, DEFAULT_MAX_RUN, DEFAULT_MB, DEFAULT_PB, ID_CCE,
    ID_CPE, ID_DSE, ID_END, ID_FIL, ID_LFE, ID_PCE, ID_SCE, element_layout, native_from_alac,
    predict, unpredict,
};

use crate::Speaker;

/// The speakers of an ALAC stream of `channels` channels, in the slot order
/// the decoder returns and the encoder takes; `None` past eight. ALAC's
/// layouts in native order: mono, stereo, 3.0, 4.0 (FL FR FC BC — not
/// quad), 5.0, 5.1, 6.1, and for eight channels 7.1(wide), whose front
/// left- and right-of-centre pair is ALAC's `Lc` / `Rc`.
pub fn layout(channels: u8) -> Option<&'static [Speaker]> {
    use Speaker::*;
    Some(match channels {
        1 => &[FC],
        2 => &[FL, FR],
        3 => &[FL, FR, FC],
        4 => &[FL, FR, FC, BC],
        5 => &[FL, FR, FC, BL, BR],
        6 => &[FL, FR, FC, LFE, BL, BR],
        7 => &[FL, FR, FC, LFE, BC, SL, SR],
        8 => &[FL, FR, FC, LFE, BL, BR, FLC, FRC],
        _ => return None,
    })
}
