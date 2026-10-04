# rivet-lossless

[![CI](https://github.com/safewords/rivet-lossless/actions/workflows/ci.yml/badge.svg)](https://github.com/safewords/rivet-lossless/actions/workflows/ci.yml)

**FLAC** and **ALAC** (Apple Lossless) encoders and decoders in Rust: no C,
no system libraries, no build script, nothing to install on a build host.
Written from RFC 9639 and the published ALAC format description, not
translated from any other implementation. Every stream it was checked on
decodes to exactly the PCM that went in, both ways, against the `flac`
command-line tool and Apple's ALAC reference encoder and decoder
([below](#how-it-is-checked)).

Written for the **[rivet](https://github.com/safewords/rivet)**
transcoder, where it is the lossless codec on both sides: the encoders
behind `audio=flac` and `audio=alac`, and the decoders that let a FLAC or
ALAC source be downmixed, filtered or transcoded. Usable on its own by
anything that has FLAC or ALAC packets and wants PCM back, or PCM and wants
FLAC or ALAC.

Published as `rivet-lossless`; **imported as `lossless`** (`use lossless::…`).
One dependency (`thiserror`; the MD5 of FLAC's audio signature is this
crate's own, from RFC 1321), no build script. One feature, `force-scalar`,
for testing (see [Speed](#speed)).

```toml
[dependencies]
lossless = { package = "rivet-lossless", git = "https://github.com/safewords/rivet-lossless", branch = "develop" }
```

## What it does

Both codecs work on **interleaved integer PCM** (`i32`, at the stream's bit
depth) on one side and **packets plus the codec configuration** on the
other. Containers are the caller's: the decoders take the configuration in
the forms MP4 and Matroska carry it, and the encoders hand back frames and
the configuration to put in one.

| | FLAC (`lossless::flac`) | ALAC (`lossless::alac`) |
|---|---|---|
| **Bit depths** | 4–32 | 16, 20, 24, 32 |
| **Channels** | 1–8 | 1–8 |
| **Sample rates** | any; the encoder takes 1 Hz to 1,048,575 Hz | any the cookie names |
| **Packet** | one or more whole frames (an MP4 `fLaC` sample, a Matroska `A_FLAC` block, a native stream's frames) | one frame (an MP4 `alac` sample, a Matroska `A_ALAC` block) |
| **Configuration** | STREAMINFO, from the native stream head (`fLaC` + metadata blocks, as Matroska CodecPrivate holds it), the bare metadata blocks, an MP4 `dfLa` body, or a bare 34-byte STREAMINFO; optional to the decoder when every frame header is self-describing | the 24-byte magic cookie (`ALACSpecificConfig`): bare, as the 28-byte `alac` FullBox body, as a whole `alac` atom, or inside a QuickTime `wave` atom; required |

### Decoding

- **FLAC** — everything the format allows: the four subframe types
  (constant, verbatim, fixed and LPC prediction), wasted bits, the three
  stereo decorrelation modes, fixed and variable block sizes. The frame
  header's CRC-8 and the frame's CRC-16 are checked, and when STREAMINFO
  carries an MD5 the decoded audio is hashed and `Decoder::md5_matches`
  compares it at the stream's end.
- **ALAC** — frames of single-channel, channel-pair and LFE elements, each
  predicted and Rice coded or escaped to raw samples, with shifted-out low
  bytes; data stream and fill elements are skipped. Coupling channel and
  program config elements, and a cookie of a compatible version other than
  0, are refused with `Error::Unsupported`.

Malformed input comes back as `Error::Invalid`, naming the codec and what
is wrong.

### Encoding

- **FLAC** — a fixed-block-size stream of 4096-sample frames (the last one
  shorter). Every subframe weighs constant, verbatim, the fixed predictors
  of orders 0–4 and LPC, and keeps the cheapest in exact bits; LPC
  coefficients come from the autocorrelation of a Tukey-windowed block
  through Levinson-Durbin, quantised with error feedback. Residuals are
  Rice coded in partitions whose order and parameters are searched per
  subframe, with the raw-bits escape where that is smaller. Stereo frames
  try all four channel assignments; low bits that are zero throughout a
  block are shifted out. STREAMINFO — frame size bounds, sample count, the
  MD5 of the audio — is complete after `Encoder::finish`.

  | `Level` | predictors | Rice partition order |
  |---|---|---|
  | `Fast` | fixed only (order estimated) | to 3 |
  | `Default` | LPC to order 8, the order picked from the Levinson error estimate | to 6 |
  | `Best` | LPC to order 12, every order priced exactly | to 8 |

- **ALAC** — 4096-sample frames of elements in ALAC's channel order for the
  count. Each element is coded two ways and the smaller kept: predicted (the
  adaptive predictor seeded per channel and frame with LPC coefficients of
  orders 4 and 8, its residual in the adaptive Rice code; a channel pair
  first mixed to a weighted mid and a side), or escaped to raw samples. 20-
  and 24-bit elements are also tried with their low byte split off (32-bit
  ones always split off two). The predictor's arithmetic is checked against
  the 32-bit range a reference decoder computes in, so every frame decodes
  the same everywhere. The cookie's largest-frame and average-bit-rate
  fields are filled in from what was encoded.

### What it does not do

- **Containers.** No MP4, Matroska, Ogg, CAF or native-FLAC file reading or
  writing beyond what the configuration forms above need; that is the
  caller's (in rivet, the container crate).
- **FLAC metadata other than STREAMINFO.** The decoder skips over seek
  tables, Vorbis comments, pictures and application blocks; the encoder
  writes STREAMINFO alone. `flac::block_header` and the `BLOCK_*` constants
  are there for a caller that writes more.
- **Variable block sizes or other frame lengths on the encoding side**:
  both encoders write 4096-sample frames.
- **Resampling, remixing, dither.** A rate, layout or depth change is the
  caller's.

## Channel order

Samples go in and come out in the channel order most multichannel PCM
pipelines use (the WAVE channel-mask order). FLAC's order for every count is that
order. ALAC's layouts lead with the centre channel, so the ALAC decoder and
encoder reorder. `flac::layout(n)` and `alac::layout(n)` name the
`Speaker`s:

| Channels | FLAC | ALAC order | ALAC, as it comes out |
|---|---|---|---|
| 1 | FC | C | FC |
| 2 | FL FR | L R | FL FR |
| 3 | FL FR FC | C L R | 3.0: FL FR FC |
| 4 | FL FR BL BR (quad) | C L R Cs | **4.0**: FL FR FC BC |
| 5 | FL FR FC BL BR | C L R Ls Rs | 5.0: FL FR FC BL BR |
| 6 | 5.1: FL FR FC LFE BL BR | C L R Ls Rs LFE | 5.1: FL FR FC LFE BL BR |
| 7 | 6.1: FL FR FC LFE BC SL SR | C L R Ls Rs Cs LFE | 6.1: FL FR FC LFE BC SL SR |
| 8 | 7.1: FL FR FC LFE BL BR SL SR | C Lc Rc L R Ls Rs LFE | **7.1(wide)**: FL FR FC LFE BL BR FLC FRC |

## Integer PCM and f32

The integer interfaces (`decode_int`, `encode_int`) are exact at every
depth. For a pipeline that carries f32 in `[-1.0, 1.0]`, `lossless::pcm`
maps an integer sample of `b` bits to `s / 2^(b-1)` and back by the
inverse: exact for every depth up to 24 bits (the f32 significand), so
FLAC / ALAC → f32 → FLAC / ALAC is bit-exact end to end at 24 bits or
less; a 32-bit sample keeps its top 24 bits.

## Speed

On a Ryzen 9 9950X (Windows, a shared machine, best of five), in multiples
of real time, for the first 60 s of a 16-bit and a 24-bit stereo 44.1 kHz
album track; `cargo run --release --example bench -- <file.flac>` measures
it on any FLAC file. Encoders on one thread, then on all of them.

| | 16-bit before | 16-bit now | 24-bit before | 24-bit now |
|---|---|---|---|---|
| FLAC decode (the whole file, MD5 checked) | 424 | 1527 | 417 | 1317 |
| FLAC encode `Fast` | 262 | 949 / 1696 threaded | 305 | 917 / 1135 |
| FLAC encode `Default` | 116 | 613 / 1316 | 131 | 574 / 983 |
| FLAC encode `Best` | 35 | 169 / 1149 | 37 | 189 / 962 |
| ALAC encode | 154 | 275 / 1193 | 90 | 159 / 745 |
| ALAC decode | 231 | 496 | 226 | 435 |

What does it: a bit reader that takes each Rice code from a 64-bit
register, and LPC and fixed-predictor restoration unrolled per order with
the newest sample in a register; slicing-by-16 CRC-16 and a faster MD5;
in the encoders, the autocorrelation computed for every lag at once, the
LPC residual as 32 × 32 → 64-bit vector multiplies, the Rice partition
search from per-partition sums and ORs without per-value passes, and the
ALAC predictor's coefficient update without its data-dependent exit.

**Vector code** is selected at run time: on x86-64 the kernels are built
twice, for the baseline and with AVX2, and CPUID picks; on aarch64 NEON is
the baseline. Every kernel is integer code or floating point summed in a
fixed order without fused multiply-add, so **the output is the same to the
bit on every CPU and code path** — decoders trivially (lossless), encoders
too: `the_encoded_bytes_do_not_change` holds both encoders to hashes of
their output from before this work, on every level, depth and layout
tested. The `force-scalar` feature compiles the run-time selection out
(CI runs the tests both ways, on x86-64 and arm64).

**Threads**: the encoders code the whole frames one `encode_int` call
completes on scoped threads, one per CPU by default
(`Encoder::set_threads(1)` keeps everything on the caller's thread); the
stream is the same byte for byte whatever the count. A caller feeding one
frame's worth at a time gets no threading. The decoders decode on the
caller's thread; the FLAC decoder's MD5 check — one serial hash over the
stream, a third of a 24-bit decode — runs beside it on one helper thread,
fed each decoded frame in order, and `md5_matches` waits for it to catch
up, so the answer is the same as hashing inline (`src/flac/verify.rs`).
That took the decode of a 180 s 96 kHz 24-bit stereo file from 785× to
1265× real time, and of the same at 44.1 kHz 16-bit from 2066× to 3308×.

## How it is checked

- **Against independent implementations, as black boxes**
  (`tests/oracle.rs`; CI installs `flac` and `mkvtoolnix`, builds
  `alacconvert` with `tools/build-alacconvert.sh`, and sets
  `RIVET_REQUIRE_LOSSLESS_ORACLES=1`, under which a missing tool fails the
  run instead of skipping it). Synthetic PCM is encoded by the reference and
  decoded here, or encoded here and decoded by the reference; either way the
  PCM must come back exactly. The references: for FLAC, `flac`, the
  Xiph.Org reference implementation's command-line tool; for ALAC,
  `alacconvert`, built from Apple's open-source ALAC release
  ([macosforge/alac](https://github.com/macosforge/alac), pinned), which
  reads and writes CAF files. `FLAC`, `MKVMERGE` and `ALACCONVERT` name the
  binaries when they are not on PATH.
  - Decode: `flac` CLI streams at `-0`, `-3`, `-5`, `-8`, `-8 -l 32`, block
    sizes 576 / 1152 / 4096, `--no-mid-side`; 8, 16, 24 and 32 bits;
    22.05–192 kHz; 1–8 channels; the STREAMINFO MD5 checked. FLAC muxed into
    Matroska by `mkvmerge`, and into MP4 (`dfLa`) by the test's own writer.
    Apple's ALAC encoder: 16, 24 and 32 bits, 22.05–192 kHz, 1–8 channels,
    the cookie with its channel layout info as Apple writes it.
  - Encode: FLAC through `flac -t` (MD5 verified) and `flac -d`; all three
    levels; 8, 16, 24 and 32 bits; 22.05–192 kHz; 1, 2, 3, 5, 6, 7 and 8
    channels. ALAC decoded by Apple's decoder: 16, 24 and 32 bits;
    44.1–192 kHz; 1–8 channels. Apple's tool does no channel reordering,
    so both directions also check this crate's ALAC channel order against
    the orders Apple documents.
- **Round trips** through this crate's own encoder and decoder at every
  depth, layout and level, and short, silent, constant, full-scale and
  noise-only inputs.
- **The format pieces**: both CRCs against their published check values,
  STREAMINFO and the cookie in every wrapping, the bit reader and writer at
  every width, the Rice coder through runs and escapes, the ALAC predictor's
  inverse, Levinson-Durbin on a known AR(2) process.

First run in the rivet repository against `flac` 1.4.2 and ffmpeg 5.1, and
at the move to this repository (2026-10-02) against `flac` 1.4.3 and
ffmpeg 8.1.1. Since 2026-10-03 the references are `flac` 1.4.3, MKVToolNix
82 and Apple's `alacconvert` (macosforge/alac `c38887c`), every case
passing; ffmpeg is no longer used.

**Size against the reference encoders** (10 s of stereo at 44.1 kHz, % of
the raw PCM, the ALAC columns counting packets only; `flac -5` and Apple's
ALAC encoder at their defaults):

| Signal | FLAC `Fast` | `Default` | `Best` | `flac -5` | ALAC | Apple ALAC |
|---|---|---|---|---|---|---|
| tones + noise, 16-bit | 69.8% | 68.7% | 68.4% | 69.2% | 68.8% | 69.0% |
| tones + noise, 24-bit | 79.3% | 78.5% | 78.3% | 78.9% | 78.9% | 79.4% |
| 1 kHz sine, 16-bit | 26.1% | 18.4% | 14.6% | 26.6% | 22.9% | 26.7% |
| brown noise, 16-bit | 62.6% | 62.6% | 62.6% | 63.6% | 63.3% | 63.4% |

**Known issue (ALAC encode):** on about 1 in 70 of the synthetic test
signals, a frame this crate's encoder writes decodes differently in
Apple's decoder than in this crate's — always a few samples after a
stretch of silence or of a constant ends, where the residual coder has been
coding runs of zeros. The two halves of this crate agree with each other
there, so they share the departure from the format. The cases are kept in
`tests/oracle.rs` as an ignored test until it is found and fixed.

**Not verified here:** 20-bit ALAC against another implementation, either
way (`alacconvert` neither writes nor reads it; the round trips cover it);
FLAC in MP4 from another muxer (the packaged MP4 muxer that writes `dfLa`,
GPAC's MP4Box, links FFmpeg's libraries; the test wraps `flac`'s frames
itself); FLAC streams with variable
block sizes from another encoder (`flac` does not write them; the decoder
handles the flag and the sample-numbered header).

## Provenance and licensing

Written from:

- the FLAC format specification, IETF RFC 9639 (and the xiph.org format
  documentation it standardises), with "Encapsulation of FLAC in ISO Base
  Media File Format" (xiph.org) for the `dfLa` form of the configuration;
- the published description of the Apple Lossless format: the
  `ALACSpecificConfig` magic cookie, its channel layouts, the frame element
  syntax, and the adaptive Golomb-Rice and adaptive-predictor coding scheme;
- published literature on linear prediction (the autocorrelation method,
  the Levinson-Durbin recursion, coefficient quantisation) and on Rice /
  Golomb coding.

**No implementation's source was consulted** — not libFLAC, not FFmpeg's
FLAC or ALAC codecs, not Apple's ALAC reference code, not claxon, symphonia
or any other decoder or encoder. The `flac` command-line tool, `mkvmerge`
and Apple's `alacconvert` (built from its release, never read) are used
only as black boxes: to make test streams, and to decode this crate's
output so it could be compared with the source PCM. ffmpeg served the same
black-box role until 2026-10-03 and is no longer used. The code was
written in the rivet repository first and moved here with its history.

**Patents.** FLAC is an open format (RFC 9639), and Apple published ALAC
under the Apache License 2.0; both are royalty-free. Nothing here is a
licence to any patent, and the authors make no claim about whether anyone
needs one.

## Using it

```rust
use lossless::{alac, flac};

// FLAC: the configuration (dfLa body / CodecPrivate) first, then packets.
let mut dec = flac::Decoder::new(Some(&config), sample_rate, channels)?;
for packet in packets {
    let (samples, channels, bits) = dec.decode_int(packet)?; // interleaved i32
}
assert_ne!(dec.md5_matches(), Some(false));

// FLAC encoding: interleaved i32 in, frames (with their sample counts) out.
let mut enc = flac::Encoder::new(flac::EncoderConfig {
    sample_rate: 48_000, channels: 2, bits_per_sample: 24, level: flac::Level::Default,
})?;
let mut frames = enc.encode_int(&pcm);
frames.extend(enc.finish());
let blocks = enc.metadata_blocks(); // STREAMINFO, for dfLa or after `fLaC`

// ALAC: the magic cookie, then one frame per packet.
let mut dec = alac::Decoder::new(Some(&cookie))?;
let samples = dec.decode_int(packet)?;

let mut enc = alac::Encoder::new(48_000, 2, 16)?;
let mut frames = enc.encode_int(&pcm);
frames.extend(enc.finish());
let cookie = enc.cookie().to_bytes();

// f32 at full scale ±1.0, exactly, up to 24 bits.
let f = lossless::pcm::ints_to_f32(&samples, 16);
```

## License

Open Encoding Attribution License v1.0 — a source-available (not OSI open-source)
license, royalty-free, with a commercial-attribution requirement. See
[LICENSE.md](LICENSE.md) and [NOTICE](NOTICE).
