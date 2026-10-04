//! FLAC and ALAC against independent implementations, used as black boxes.
//!
//! - FLAC: the reference implementation's command-line tool, `flac`
//!   (Xiph.Org). Decode: synthetic PCM → `flac` → this crate's decoder,
//!   which must give back the PCM exactly. Encode: synthetic PCM → this
//!   crate's encoder → `flac -t` (its MD5 check) and `flac -d`, which must
//!   give back the PCM exactly. `mkvmerge` (MKVToolNix) wraps `flac`'s
//!   streams in Matroska, so the decoder also takes its configuration in
//!   the form another muxer writes it.
//! - ALAC: Apple's reference encoder and decoder, the `alacconvert` utility
//!   built from Apple's open-source ALAC release
//!   (<https://github.com/macosforge/alac>, Apache 2.0). It reads and writes
//!   Core Audio Format files, so the PCM and the ALAC packets go in and out
//!   through the small CAF reader and writer at the end of this file. Both
//!   directions, bit-exact.
//!
//! Each test SKIPs (passes, printing why) when a tool it needs is missing,
//! so the suite runs anywhere — unless `RIVET_REQUIRE_LOSSLESS_ORACLES` is
//! set, as CI sets it, where a missing tool is a failure. `FLAC`,
//! `MKVMERGE` and `ALACCONVERT` name the binaries; by default each is looked
//! for on PATH.

use std::path::{Path, PathBuf};
use std::process::Command;

use lossless::flac::{EncoderConfig as FlacEncoderConfig, Level as FlacLevel};
use lossless::{alac, flac};

/// The tool `name` (or the binary its environment variable names) if it
/// runs; a panic instead of `None` under `RIVET_REQUIRE_LOSSLESS_ORACLES`.
fn tool(name: &str) -> Option<PathBuf> {
    let var = name.to_ascii_uppercase();
    let path = std::env::var_os(&var).map_or_else(|| PathBuf::from(name), PathBuf::from);
    let found = match name {
        // Run bare, `alacconvert` prints its usage and exits non-zero.
        "alacconvert" => Command::new(&path).output().is_ok_and(|o| {
            String::from_utf8_lossy(&[o.stdout, o.stderr].concat()).contains("alacconvert")
        }),
        _ => Command::new(&path)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success()),
    };
    if !found && std::env::var_os("RIVET_REQUIRE_LOSSLESS_ORACLES").is_some() {
        panic!(
            "RIVET_REQUIRE_LOSSLESS_ORACLES is set, and `{name}` does not run (set {var} or put it on PATH)"
        );
    }
    found.then_some(path)
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rivet-lossless-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn run(cmd: &mut Command) {
    let out = cmd.output().expect("spawn");
    assert!(
        out.status.success(),
        "{cmd:?} failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Deterministic test audio: per channel, a mix of a few tones and noise,
/// the channels partly correlated (so every stereo mode has something to
/// win), a stretch of digital silence, a stretch of a constant, and a burst
/// at full scale.
fn signal(frames: usize, channels: usize, bits: u32, seed: u32) -> Vec<i32> {
    let full = f64::from((1u32 << (bits - 1)) - 1);
    let mut rng = seed.wrapping_mul(2_654_435_761).max(1);
    let mut noise = move || {
        rng ^= rng << 13;
        rng ^= rng >> 17;
        rng ^= rng << 5;
        f64::from(rng) / f64::from(u32::MAX) - 0.5
    };
    let mut out = Vec::with_capacity(frames * channels);
    for i in 0..frames {
        let t = i as f64 / 48_000.0;
        let common = (t * 2.0 * std::f64::consts::PI * 220.0).sin() * 0.4
            + (t * 2.0 * std::f64::consts::PI * 1_375.0).sin() * 0.1;
        let n = noise();
        for c in 0..channels {
            let own = (t * 2.0 * std::f64::consts::PI * (330.0 + 110.0 * c as f64)).sin() * 0.2;
            let v = if i > frames / 3 && i < frames / 3 + 5_000 {
                0.0
            } else if i >= frames / 2 && i < frames / 2 + 3_000 {
                0.25
            } else if i >= frames * 3 / 4 && i < frames * 3 / 4 + 500 {
                if (i / 7 + c) % 2 == 0 { 1.0 } else { -1.0 }
            } else {
                common + own * (c as f64 * 0.3) + n * 0.05 + noise() * 0.02
            };
            let s = (v * full).round().clamp(-full - 1.0, full) as i64;
            out.push(s as i32);
        }
    }
    out
}

/// Little-endian raw PCM in the fewest whole bytes per sample.
fn raw_bytes(samples: &[i32], bits: u32) -> Vec<u8> {
    let width = bits.div_ceil(8) as usize;
    samples
        .iter()
        .flat_map(|s| s.to_le_bytes()[..width].to_vec())
        .collect()
}

fn read_raw(path: &Path, bits: u32) -> Vec<i32> {
    let width = bits.div_ceil(8) as usize;
    std::fs::read(path)
        .unwrap()
        .chunks_exact(width)
        .map(|b| {
            let mut v = [0u8; 4];
            v[4 - width..].copy_from_slice(b);
            i32::from_le_bytes(v) >> (32 - 8 * width)
        })
        .collect()
}

fn decode_flac_track(track: &Track) -> (Vec<i32>, flac::Decoder) {
    let mut dec = flac::Decoder::new(Some(&track.config), 0, 0).unwrap();
    let mut out = Vec::new();
    for p in &track.packets {
        out.extend(dec.decode_int(p).unwrap().0);
    }
    (out, dec)
}

fn decode_alac_track(track: &Track) -> Vec<i32> {
    let mut dec = alac::Decoder::new(Some(&track.config)).unwrap();
    let mut out = Vec::new();
    for p in &track.packets {
        out.extend(dec.decode_int(p).unwrap());
    }
    out
}

fn first_mismatch(a: &[i32], b: &[i32]) -> String {
    match a.iter().zip(b).position(|(x, y)| x != y) {
        Some(i) => format!("first difference at sample {i}: got {} want {}", a[i], b[i]),
        None => format!("lengths differ: got {} want {}", a.len(), b.len()),
    }
}

/// `flac`'s encode of `pcm` (raw, little-endian, signed) with `args`.
fn flac_encode(
    flac: &Path,
    pcm: &[i32],
    rate: u32,
    channels: usize,
    bits: u32,
    args: &[&str],
    name: &str,
) -> PathBuf {
    let raw = scratch(&format!("{name}.raw"));
    let out = scratch(&format!("{name}.flac"));
    std::fs::write(&raw, raw_bytes(pcm, bits)).unwrap();
    run(Command::new(flac)
        .args([
            "--silent",
            "-f",
            "--force-raw-format",
            "--endian=little",
            "--sign=signed",
        ])
        .arg(format!("--channels={channels}"))
        .arg(format!("--bps={bits}"))
        .arg(format!("--sample-rate={rate}"))
        .args(args)
        .arg("-o")
        .arg(&out)
        .arg(&raw));
    out
}

#[test]
fn flac_cli_streams_decode_bit_exact() {
    let Some(flac) = tool("flac") else {
        eprintln!("SKIP: no `flac`");
        return;
    };
    let cases: &[(u32, usize, u32, &[&str])] = &[
        (44_100, 2, 16, &["-5"]),
        (44_100, 2, 16, &["-0"]),
        (44_100, 2, 16, &["-8"]),
        (48_000, 1, 16, &["-5"]),
        (48_000, 2, 24, &["-8"]),
        (96_000, 2, 24, &["-5", "--no-mid-side"]),
        (48_000, 6, 16, &["-5"]),
        (96_000, 6, 24, &["-8"]),
        (48_000, 8, 24, &["-5"]),
        (44_100, 2, 8, &["-5"]),
        (48_000, 2, 32, &["-5"]),
        (44_100, 2, 16, &["-5", "-b", "1152"]),
        (22_050, 3, 16, &["-3"]),
        (32_000, 5, 16, &["-5"]),
        (48_000, 7, 16, &["-5"]),
        (48_000, 4, 16, &["-5", "-b", "576"]),
        (192_000, 2, 24, &["-8", "-l", "32"]),
    ];
    for (i, &(rate, channels, bits, args)) in cases.iter().enumerate() {
        let pcm = signal(rate as usize * 3 / 2 + 1_234, channels, bits, i as u32 + 1);
        let path = flac_encode(&flac, &pcm, rate, channels, bits, args, &format!("f{i}"));
        let src = demux_audio(&std::fs::read(&path).unwrap());
        assert_eq!(src.codec, "flac");
        let (got, dec) = decode_flac_track(&src);
        let label = format!("{rate} Hz {channels}ch {bits}-bit flac {args:?}");
        assert!(got == pcm, "{label}: {}", first_mismatch(&got, &pcm));
        assert_eq!(dec.md5_matches(), Some(true), "{label}: MD5");
        assert_eq!(dec.samples_decoded() as usize, pcm.len() / channels);
        eprintln!("ok: {label}");
    }
}

/// `flac`'s streams in Matroska as `mkvmerge` writes them (CodecPrivate
/// `fLaC` + the metadata blocks, one frame per block), and in MP4 with the
/// metadata blocks in a `dfLa` box. The MP4 wrapping is this file's own
/// writer — the packaged MP4 muxer that writes `dfLa`, GPAC's MP4Box, links
/// FFmpeg's libraries — so it checks only that the decoder takes the `dfLa`
/// form of the configuration.
#[test]
fn flac_in_matroska_and_mp4_decodes_bit_exact() {
    let (Some(flac), Some(mkvmerge)) = (tool("flac"), tool("mkvmerge")) else {
        eprintln!("SKIP: needs `flac` and `mkvmerge`");
        return;
    };
    for (i, &(rate, channels, bits)) in
        [(48_000u32, 2usize, 16u32), (96_000, 6, 24), (44_100, 1, 16)]
            .iter()
            .enumerate()
    {
        let pcm = signal(rate as usize + 777, channels, bits, 40 + i as u32);
        let native = flac_encode(&flac, &pcm, rate, channels, bits, &[], &format!("m{i}"));
        let mkv = scratch(&format!("m{i}.mkv"));
        run(Command::new(&mkvmerge)
            .args(["--quiet", "--disable-lacing", "-o"])
            .arg(&mkv)
            .arg(&native));
        let src = demux_audio(&std::fs::read(&mkv).unwrap());
        assert_eq!(src.codec, "flac", "mkv");
        let (got, dec) = decode_flac_track(&src);
        assert!(
            got == pcm,
            "FLAC in Matroska {channels}ch {bits}-bit: {}",
            first_mismatch(&got, &pcm)
        );
        assert_eq!(dec.samples_decoded(), (pcm.len() / channels) as u64);
        eprintln!("ok: FLAC in Matroska (mkvmerge) {rate} Hz {channels}ch {bits}-bit");

        // The same frames with the configuration in a `dfLa` box.
        let data = std::fs::read(&native).unwrap();
        let (info, blocks_len) = flac::parse_metadata_blocks(&data[4..]).unwrap();
        let blocks = &data[4..4 + blocks_len];
        let mut frames = Vec::new();
        let mut at = 4 + blocks_len;
        while at < data.len() {
            let f = flac::decode_frame(&data[at..], Some(&info)).unwrap();
            frames.push((data[at..at + f.len].to_vec(), f.header.block_size));
            at += f.len;
        }
        let mp4 = m4a(blocks, rate, channels as u8, bits as u8, &frames);
        let src = demux_audio(&mp4);
        let (got, _) = decode_flac_track(&src);
        assert!(
            got == pcm,
            "FLAC in MP4 (dfLa) {channels}ch {bits}-bit: {}",
            first_mismatch(&got, &pcm)
        );
        eprintln!("ok: FLAC in MP4 (dfLa) {rate} Hz {channels}ch {bits}-bit");
    }
}

/// For each native-order channel (the crate's [`alac::layout`]), the slot it
/// takes in ALAC's own channel order, as Apple's ALAC release documents the
/// orders: 3 C L R; 4 C L R Cs; 5 C L R Ls Rs; 6 C L R Ls Rs LFE; 7 C L R
/// Ls Rs Cs LFE; 8 C Lc Rc L R Ls Rs LFE. `alacconvert` does no channel
/// reordering, so its PCM is in these orders. Written out here rather than
/// taken from the crate, so a mistake in the crate's table shows.
fn alac_slot(channels: usize) -> &'static [usize] {
    match channels {
        1 => &[0],
        2 => &[0, 1],
        3 => &[1, 2, 0],
        4 => &[1, 2, 0, 3],
        5 => &[1, 2, 0, 3, 4],
        6 => &[1, 2, 0, 5, 3, 4],
        7 => &[1, 2, 0, 6, 5, 3, 4],
        8 => &[3, 4, 0, 7, 5, 6, 1, 2],
        n => panic!("{n} channels"),
    }
}

/// Native-order interleaved PCM to ALAC order.
fn to_alac_order(pcm: &[i32], channels: usize) -> Vec<i32> {
    let slot = alac_slot(channels);
    let mut out = vec![0; pcm.len()];
    for (src, dst) in pcm
        .chunks_exact(channels)
        .zip(out.chunks_exact_mut(channels))
    {
        for (native, &s) in slot.iter().enumerate() {
            dst[s] = src[native];
        }
    }
    out
}

/// ALAC-order interleaved PCM to native order.
fn from_alac_order(pcm: &[i32], channels: usize) -> Vec<i32> {
    let slot = alac_slot(channels);
    pcm.chunks_exact(channels)
        .flat_map(|f| slot.iter().map(|&s| f[s]))
        .collect()
}

/// What Apple's encoder is given: 16-, 24- and 32-bit PCM, every channel
/// count ALAC has. (`alacconvert` reads 20-bit PCM packed, as nothing else
/// writes it, and does not decode 20-bit ALAC, so that depth is checked by
/// this crate's own round trips only.)
const APPLE_ENCODES: &[(u32, usize, u32)] = &[
    (44_100, 2, 16),
    (44_100, 1, 16),
    (48_000, 2, 24),
    (96_000, 2, 24),
    (192_000, 2, 24),
    (48_000, 1, 24),
    (48_000, 3, 16),
    (48_000, 3, 24),
    (48_000, 4, 16),
    (48_000, 5, 24),
    (48_000, 6, 16),
    (96_000, 6, 24),
    (48_000, 7, 16),
    (48_000, 8, 16),
    (48_000, 8, 24),
    (48_000, 2, 32),
    (44_100, 1, 32),
    (22_050, 2, 16),
];

/// What Apple's decoder is given from this crate's encoder: 16, 24 and 32
/// bits, every channel count ALAC has.
const APPLE_DECODES: &[(u32, usize, u32)] = &[
    (44_100, 2, 16),
    (44_100, 1, 16),
    (48_000, 2, 24),
    (96_000, 2, 24),
    (192_000, 2, 24),
    (48_000, 3, 16),
    (48_000, 4, 24),
    (48_000, 5, 16),
    (48_000, 6, 16),
    (96_000, 6, 24),
    (48_000, 7, 16),
    (48_000, 8, 16),
    (48_000, 8, 24),
    (48_000, 2, 32),
    (48_000, 6, 32),
];

#[test]
fn apple_alac_decodes_bit_exact() {
    let Some(alacconvert) = tool("alacconvert") else {
        eprintln!("SKIP: no `alacconvert`");
        return;
    };
    for (i, &(rate, channels, bits)) in APPLE_ENCODES.iter().enumerate() {
        let pcm = signal(rate as usize * 3 / 2 + 99, channels, bits, 80 + i as u32);
        let src = scratch(&format!("a{i}.pcm.caf"));
        let out = scratch(&format!("a{i}.alac.caf"));
        std::fs::write(
            &src,
            caf_pcm(&to_alac_order(&pcm, channels), rate, channels, bits),
        )
        .unwrap();
        run(Command::new(&alacconvert).arg(&src).arg(&out));
        let caf = read_caf(&std::fs::read(&out).unwrap());
        let track = caf.alac.expect("an ALAC CAF");
        let got = decode_alac_track(&track);
        let label = format!("Apple ALAC {rate} Hz {channels}ch {bits}-bit");
        // The packet table's valid-frame count trims the last packet.
        assert!(
            got.len() >= pcm.len(),
            "{label}: {} samples, want {}",
            got.len(),
            pcm.len()
        );
        assert_eq!(
            caf.valid_frames,
            Some((pcm.len() / channels) as u64),
            "{label}: pakt frames"
        );
        let got = &got[..pcm.len()];
        assert!(got == pcm, "{label}: {}", first_mismatch(got, &pcm));
        eprintln!("ok: {label}");
    }
}

fn rivet_flac(
    pcm: &[i32],
    rate: u32,
    channels: u8,
    bits: u8,
    level: FlacLevel,
) -> (Vec<u8>, Vec<(Vec<u8>, u32)>) {
    let mut enc = flac::Encoder::new(FlacEncoderConfig {
        sample_rate: rate,
        channels,
        bits_per_sample: bits,
        level,
    })
    .unwrap();
    let mut frames = enc.encode_int(pcm);
    frames.extend(enc.finish());
    (enc.metadata_blocks(), frames)
}

fn rivet_alac(pcm: &[i32], rate: u32, channels: u8, bits: u8) -> (Vec<u8>, Vec<(Vec<u8>, u32)>) {
    let mut enc = alac::Encoder::new(rate, channels, bits).unwrap();
    let mut frames = enc.encode_int(pcm);
    frames.extend(enc.finish());
    (enc.cookie().to_bytes().to_vec(), frames)
}

#[test]
fn rivet_flac_decodes_bit_exact_in_flac() {
    let Some(flac) = tool("flac") else {
        eprintln!("SKIP: no `flac`");
        return;
    };
    let cases: &[(u32, u8, u8, FlacLevel)] = &[
        (44_100, 2, 16, FlacLevel::Fast),
        (44_100, 2, 16, FlacLevel::Default),
        (44_100, 2, 16, FlacLevel::Best),
        (48_000, 1, 16, FlacLevel::Default),
        (48_000, 2, 24, FlacLevel::Default),
        (96_000, 2, 24, FlacLevel::Best),
        (48_000, 6, 16, FlacLevel::Default),
        (96_000, 6, 24, FlacLevel::Default),
        (48_000, 8, 24, FlacLevel::Default),
        (192_000, 2, 24, FlacLevel::Fast),
        (48_000, 2, 32, FlacLevel::Default),
        (22_050, 3, 16, FlacLevel::Default),
        (32_000, 5, 16, FlacLevel::Best),
        (48_000, 7, 24, FlacLevel::Default),
        (44_100, 2, 8, FlacLevel::Default),
    ];
    for (i, &(rate, channels, bits, level)) in cases.iter().enumerate() {
        let pcm = signal(
            rate as usize * 3 / 2 + 555,
            usize::from(channels),
            u32::from(bits),
            200 + i as u32,
        );
        let (blocks, frames) = rivet_flac(&pcm, rate, channels, bits, level);
        let label = format!("rivet FLAC {rate} Hz {channels}ch {bits}-bit {level:?}");
        let native = scratch(&format!("e{i}.flac"));
        std::fs::write(&native, native_flac(&blocks, &frames)).unwrap();
        // The reference decoder, with its MD5 check.
        run(Command::new(&flac).args(["--silent", "-t"]).arg(&native));
        let raw = scratch(&format!("e{i}.dec.raw"));
        run(Command::new(&flac)
            .args([
                "--silent",
                "-f",
                "-d",
                "--force-raw-format",
                "--endian=little",
                "--sign=signed",
                "-o",
            ])
            .arg(&raw)
            .arg(&native));
        let got = read_raw(&raw, u32::from(bits));
        assert!(
            got == pcm,
            "{label} via flac -d: {}",
            first_mismatch(&got, &pcm)
        );
        eprintln!("ok: {label}");
    }
}

#[test]
fn rivet_alac_decodes_bit_exact_in_apple_alac() {
    let Some(alacconvert) = tool("alacconvert") else {
        eprintln!("SKIP: no `alacconvert`");
        return;
    };
    for (i, &(rate, channels, bits)) in APPLE_DECODES.iter().enumerate() {
        let pcm = signal(rate as usize * 3 / 2 + 321, channels, bits, 300 + i as u32);
        let (cookie, frames) = rivet_alac(&pcm, rate, channels as u8, bits as u8);
        let src = scratch(&format!("r{i}.alac.caf"));
        let out = scratch(&format!("r{i}.pcm.caf"));
        std::fs::write(
            &src,
            caf_alac(
                &cookie,
                rate,
                channels,
                bits,
                (pcm.len() / channels) as u64,
                &frames,
            ),
        )
        .unwrap();
        run(Command::new(&alacconvert).arg(&src).arg(&out));
        let caf = read_caf(&std::fs::read(&out).unwrap());
        let label = format!("rivet ALAC {rate} Hz {channels}ch {bits}-bit");
        let got = from_alac_order(
            &caf.pcm.unwrap_or_else(|| panic!("{label}: no PCM out")),
            channels,
        );
        assert!(
            got == pcm,
            "{label} via alacconvert: {}",
            first_mismatch(&got, &pcm)
        );
        eprintln!("ok: {label}");
    }
}

/// Sparse test audio: mostly digital silence broken by short bursts and
/// isolated clicks of every size up to full scale, so the residual coder
/// keeps entering and leaving its zero-run mode — with runs that end at the
/// block's end, runs that end on a sample of either sign and magnitude, and
/// runs that end on the smallest values, where the first sample after a
/// run is coded one lower.
fn sparse_signal(frames: usize, channels: usize, bits: u32, seed: u32) -> Vec<i32> {
    let full = (1i64 << (bits - 1)) - 1;
    let mut rng = seed.wrapping_mul(2_246_822_519).max(1);
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 17;
        rng ^= rng << 5;
        rng
    };
    let mut out = vec![0i32; frames * channels];
    let mut i = 0usize;
    while i < frames {
        i += (next() % 600) as usize;
        let len = (next() % 40) as usize;
        let shift = next() % bits;
        for j in i..(i + len).min(frames) {
            for c in 0..channels {
                if next() % 3 != 0 {
                    let v = i64::from(next() as i32) >> (31 - shift.min(31));
                    out[j * channels + c] = v.clamp(-full - 1, full) as i32;
                }
            }
        }
        i += len;
    }
    out
}

/// This crate's ALAC through Apple's decoder over a sweep of signals,
/// depths and channel counts. Where this crate once departed from the
/// format — the Rice history's update for the first sample after a run of
/// zeros, which goes by that sample's value as it decodes, not as it is
/// coded (one lower) — about 1 stream in 70 of the test signal decoded
/// differently in Apple's decoder; the first seeds here are those streams.
/// `RIVET_ALAC_SWEEP=n` widens the sweep to `n` seeds per configuration.
#[test]
fn rivet_alac_sweep_decodes_bit_exact_in_apple_alac() {
    let Some(alacconvert) = tool("alacconvert") else {
        eprintln!("SKIP: no `alacconvert`");
        return;
    };
    let known: &[(usize, u32)] = &[
        (2, 335),
        (3, 327),
        (3, 347),
        (5, 371),
        (5, 384),
        (5, 388),
        (6, 309),
        (6, 381),
        (7, 309),
        (7, 320),
        (7, 347),
        (7, 396),
        (8, 324),
    ];
    let seeds: u32 = std::env::var("RIVET_ALAC_SWEEP")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);
    let mut cases: Vec<(u32, usize, u32, u32, bool)> = known
        .iter()
        .map(|&(c, s)| (48_000, c, 16, s, false))
        .collect();
    // 20 bits through this crate's decoder alone: `alacconvert` does not
    // decode 20-bit ALAC. The entropy coder is the same at every depth.
    for bits in [16u32, 20, 24, 32] {
        for channels in 1..=8usize {
            for seed in 0..seeds {
                let seed = 1000 + seed * 7 + channels as u32 * 131 + bits;
                cases.push((48_000, channels, bits, seed, false));
                cases.push((44_100, channels, bits, seed, true));
            }
        }
    }
    let mut bad = Vec::new();
    for (n, &(rate, channels, bits, seed, sparse)) in cases.iter().enumerate() {
        let frames = if sparse {
            30_000 + seed as usize % 5_000
        } else {
            rate as usize * 3 / 2 + 321
        };
        let pcm = if sparse {
            sparse_signal(frames, channels, bits, seed)
        } else {
            signal(frames, channels, bits, seed)
        };
        let (cookie, packets) = rivet_alac(&pcm, rate, channels as u8, bits as u8);
        let label = format!(
            "{}{channels}ch {bits}-bit seed {seed}",
            if sparse { "sparse " } else { "" }
        );
        let track = Track {
            codec: "alac",
            config: cookie.clone(),
            packets: packets.iter().map(|(p, _)| p.clone()).collect(),
        };
        let own = decode_alac_track(&track);
        assert!(
            own[..pcm.len()] == pcm[..],
            "{label}: own decoder: {}",
            first_mismatch(&own, &pcm)
        );
        if bits == 20 {
            continue;
        }
        let src = scratch(&format!("w{n}.alac.caf"));
        let out = scratch(&format!("w{n}.pcm.caf"));
        std::fs::write(
            &src,
            caf_alac(
                &cookie,
                rate,
                channels,
                bits,
                (pcm.len() / channels) as u64,
                &packets,
            ),
        )
        .unwrap();
        run(Command::new(&alacconvert).arg(&src).arg(&out));
        let got = from_alac_order(
            &read_caf(&std::fs::read(&out).unwrap()).pcm.expect("PCM"),
            channels,
        );
        if got != pcm {
            eprintln!("{label}: {}", first_mismatch(&got, &pcm));
            bad.push(label);
        }
    }
    eprintln!("{} streams, {} differ", cases.len(), bad.len());
    assert!(bad.is_empty(), "Apple's decoder differs on {bad:?}");
}

/// Apple's encoder over the same sweep (sparse signals too, so its streams
/// enter and leave the zero-run mode), through this crate's decoder.
#[test]
fn apple_alac_sweep_decodes_bit_exact() {
    let Some(alacconvert) = tool("alacconvert") else {
        eprintln!("SKIP: no `alacconvert`");
        return;
    };
    let seeds: u32 = std::env::var("RIVET_ALAC_SWEEP")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);
    let mut n = 0;
    for bits in [16u32, 24, 32] {
        for channels in 1..=8usize {
            for seed in 0..seeds {
                for sparse in [false, true] {
                    let seed = 5000 + seed * 11 + channels as u32 * 37 + bits;
                    let rate = 48_000;
                    let pcm = if sparse {
                        sparse_signal(30_000 + seed as usize % 5_000, channels, bits, seed)
                    } else {
                        signal(rate as usize + 777, channels, bits, seed)
                    };
                    let src = scratch(&format!("v{n}.pcm.caf"));
                    let out = scratch(&format!("v{n}.alac.caf"));
                    n += 1;
                    std::fs::write(
                        &src,
                        caf_pcm(&to_alac_order(&pcm, channels), rate, channels, bits),
                    )
                    .unwrap();
                    run(Command::new(&alacconvert).arg(&src).arg(&out));
                    let caf = read_caf(&std::fs::read(&out).unwrap());
                    let got = decode_alac_track(&caf.alac.expect("an ALAC CAF"));
                    let label = format!(
                        "Apple {}{channels}ch {bits}-bit seed {seed}",
                        if sparse { "sparse " } else { "" }
                    );
                    assert!(got.len() >= pcm.len(), "{label}: short");
                    assert!(
                        got[..pcm.len()] == pcm[..],
                        "{label}: {}",
                        first_mismatch(&got, &pcm)
                    );
                }
            }
        }
    }
    eprintln!("{n} Apple streams decoded bit-exact");
}

/// Sizes against the reference encoders on a few synthetic signals, for
/// the record: printed, with nothing asserted beyond beating raw PCM.
#[test]
fn compression_against_the_reference_encoders() {
    let (Some(flac), Some(alacconvert)) = (tool("flac"), tool("alacconvert")) else {
        eprintln!("SKIP: needs `flac` and `alacconvert`");
        return;
    };
    let rate = 44_100u32;
    let frames = rate as usize * 10;
    let mut rng = 99u32;
    let mut noise = move || {
        rng ^= rng << 13;
        rng ^= rng >> 17;
        rng ^= rng << 5;
        f64::from(rng) / f64::from(u32::MAX) - 0.5
    };
    let mut brown = [0.0f64; 2];
    let sine: Vec<i32> = (0..frames)
        .flat_map(|i| {
            let v = ((i as f64 / 44_100.0 * 2.0 * std::f64::consts::PI * 1000.0).sin() * 20_000.0)
                as i32;
            [v, v / 2]
        })
        .collect();
    let brownian: Vec<i32> = (0..frames)
        .flat_map(|_| {
            let mut out = [0i32; 2];
            for (c, p) in brown.iter_mut().enumerate() {
                *p = (*p * 0.995 + noise() * 800.0).clamp(-32_000.0, 32_000.0);
                out[c] = *p as i32;
            }
            out
        })
        .collect();
    let signals: Vec<(&str, u32, Vec<i32>)> = vec![
        ("tones+noise 16-bit", 16, signal(frames, 2, 16, 1)),
        ("tones+noise 24-bit", 24, signal(frames, 2, 24, 2)),
        ("sine 1 kHz 16-bit", 16, sine),
        ("brown noise 16-bit", 16, brownian),
    ];
    eprintln!(
        "| signal | PCM bytes | rivet FLAC fast | rivet FLAC default | rivet FLAC best | flac -5 | rivet ALAC | Apple ALAC |"
    );
    for (i, (name, bits, pcm)) in signals.iter().enumerate() {
        let raw_len = pcm.len() * (*bits as usize / 8);
        let rivet: Vec<usize> = [FlacLevel::Fast, FlacLevel::Default, FlacLevel::Best]
            .iter()
            .map(|&l| {
                let (blocks, frames) = rivet_flac(pcm, rate, 2, *bits as u8, l);
                native_flac(&blocks, &frames).len()
            })
            .collect();
        let reference = flac_encode(&flac, pcm, rate, 2, *bits, &["-5"], &format!("c{i}"));
        let flac5 = std::fs::metadata(&reference).unwrap().len() as usize;
        // Both ALAC sizes are the packets alone.
        let (_, frames_a) = rivet_alac(pcm, rate, 2, *bits as u8);
        let rivet_alac_len: usize = frames_a.iter().map(|(f, _)| f.len()).sum();
        let src = scratch(&format!("c{i}.pcm.caf"));
        let out = scratch(&format!("c{i}.alac.caf"));
        std::fs::write(&src, caf_pcm(pcm, rate, 2, *bits)).unwrap();
        run(Command::new(&alacconvert).arg(&src).arg(&out));
        let apple = read_caf(&std::fs::read(&out).unwrap()).alac.expect("ALAC");
        let apple_len: usize = apple.packets.iter().map(Vec::len).sum();
        let pct = |n: usize| format!("{:.1}%", 100.0 * n as f64 / raw_len as f64);
        eprintln!(
            "| {name} | {raw_len} | {} | {} | {} | {} | {} | {} |",
            pct(rivet[0]),
            pct(rivet[1]),
            pct(rivet[2]),
            pct(flac5),
            pct(rivet_alac_len),
            pct(apple_len)
        );
        assert!(rivet.iter().all(|&n| n < raw_len) && rivet_alac_len < raw_len + raw_len / 50);
    }
}

// ---------------------------------------------------------------------------
// Containers: just enough of each to reach the packets and the codec
// configuration, and to wrap this crate's output for the reference tools.
// ---------------------------------------------------------------------------

/// What the tests need of a file's one audio track.
struct Track {
    codec: &'static str,
    /// The codec configuration as the container holds it: the native
    /// stream's head (`fLaC` + metadata blocks), a `dfLa` or `alac` box's
    /// body, or Matroska CodecPrivate.
    config: Vec<u8>,
    packets: Vec<Vec<u8>>,
}

fn demux_audio(data: &[u8]) -> Track {
    if data.starts_with(b"fLaC") {
        read_native_flac(data)
    } else if data.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        read_matroska(data)
    } else {
        read_mp4(data)
    }
}

/// A native FLAC stream: `fLaC`, the metadata blocks, then the frames back
/// to back — handed over as one packet, which the decoder takes frame by
/// frame.
fn read_native_flac(data: &[u8]) -> Track {
    let (_, blocks) = flac::parse_metadata_blocks(&data[4..]).unwrap();
    let head = 4 + blocks;
    Track {
        codec: "flac",
        config: data[..head].to_vec(),
        packets: vec![data[head..].to_vec()],
    }
}

/// `fLaC`, STREAMINFO (alone, flagged last, as the encoder gives it) and
/// the frames.
fn native_flac(blocks: &[u8], frames: &[(Vec<u8>, u32)]) -> Vec<u8> {
    let mut out = b"fLaC".to_vec();
    out.extend_from_slice(blocks);
    for (f, _) in frames {
        out.extend_from_slice(f);
    }
    out
}

fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
}

/// The boxes directly inside `data` (ISO/IEC 14496-12): `(type, body)`.
fn boxes(data: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 8 <= data.len() {
        let size = be32(data, i) as usize;
        let kind: [u8; 4] = data[i + 4..i + 8].try_into().unwrap();
        let (header, size) = match size {
            1 => (
                16,
                u64::from_be_bytes(data[i + 8..i + 16].try_into().unwrap()) as usize,
            ),
            0 => (8, data.len() - i),
            n => (8, n),
        };
        out.push((kind, &data[i + header..i + size]));
        i += size;
    }
    out
}

fn child<'a>(data: &'a [u8], path: &[&[u8; 4]]) -> &'a [u8] {
    path.iter().fold(data, |d, want| {
        boxes(d)
            .into_iter()
            .find(|(k, _)| k == *want)
            .unwrap_or_else(|| panic!("no {want:?} box"))
            .1
    })
}

/// The first track of an MP4: its sample entry's configuration box body
/// and its samples, through `stsz`, `stsc` and `stco` / `co64`.
fn read_mp4(data: &[u8]) -> Track {
    let stbl = child(data, &[b"moov", b"trak", b"mdia", b"minf", b"stbl"]);
    let stsd = child(stbl, &[b"stsd"]);
    let (kind, entry) = boxes(&stsd[8..])[0];
    // An AudioSampleEntry's fields take 28 bytes; QuickTime's sound
    // descriptions of version 1 and 2 take 44 and 64.
    let head = match u16::from_be_bytes([entry[8], entry[9]]) {
        1 => 44,
        2 => 64,
        _ => 28,
    };
    let (codec, want) = match &kind {
        b"fLaC" => ("flac", b"dfLa"),
        b"alac" => ("alac", b"alac"),
        k => panic!("sample entry {:?}", String::from_utf8_lossy(k)),
    };
    let config = boxes(&entry[head..])
        .into_iter()
        .find(|(k, _)| k == want)
        .expect("the configuration box")
        .1;

    let stsz = child(stbl, &[b"stsz"]);
    let count = be32(stsz, 8) as usize;
    let sizes: Vec<usize> = match be32(stsz, 4) {
        0 => (0..count)
            .map(|i| be32(stsz, 12 + 4 * i) as usize)
            .collect(),
        fixed => vec![fixed as usize; count],
    };
    let tables = boxes(stbl);
    let offsets: Vec<u64> = if let Some((_, co)) = tables.iter().find(|(k, _)| k == b"stco") {
        (0..be32(co, 4) as usize)
            .map(|i| u64::from(be32(co, 8 + 4 * i)))
            .collect()
    } else {
        let co = child(stbl, &[b"co64"]);
        (0..be32(co, 4) as usize)
            .map(|i| u64::from_be_bytes(co[8 + 8 * i..16 + 8 * i].try_into().unwrap()))
            .collect()
    };
    let stsc = child(stbl, &[b"stsc"]);
    let runs: Vec<(usize, usize)> = (0..be32(stsc, 4) as usize)
        .map(|i| {
            (
                be32(stsc, 8 + 12 * i) as usize,
                be32(stsc, 12 + 12 * i) as usize,
            )
        })
        .collect();
    let mut packets = Vec::with_capacity(count);
    let mut sample = 0;
    for (chunk, &offset) in offsets.iter().enumerate() {
        let per = runs
            .iter()
            .rev()
            .find(|(first, _)| *first <= chunk + 1)
            .expect("an stsc run")
            .1;
        let mut at = offset as usize;
        for _ in 0..per.min(count - sample) {
            packets.push(data[at..at + sizes[sample]].to_vec());
            at += sizes[sample];
            sample += 1;
        }
    }
    assert_eq!(packets.len(), count, "every sample placed");
    Track {
        codec,
        config: config.to_vec(),
        packets,
    }
}

fn mp4_box(kind: &[u8; 4], parts: &[&[u8]]) -> Vec<u8> {
    let len = 8 + parts.iter().map(|p| p.len()).sum::<usize>();
    let mut b = (len as u32).to_be_bytes().to_vec();
    b.extend_from_slice(kind);
    for p in parts {
        b.extend_from_slice(p);
    }
    b
}

/// An audio-only MP4 of one track and one chunk: `ftyp`, `moov`, `mdat`.
/// The sample rate is the media timescale (and, above 65535 Hz, 0 in the
/// sample entry's 16.16 field, which cannot hold it).
fn m4a(
    flac_blocks: &[u8],
    rate: u32,
    channels: u8,
    bits: u8,
    frames: &[(Vec<u8>, u32)],
) -> Vec<u8> {
    let u16b = |v: u16| v.to_be_bytes();
    let u32b = |v: u32| v.to_be_bytes();
    let total: u32 = frames.iter().map(|(_, n)| n).sum();
    let matrix: Vec<u8> = [0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000]
        .iter()
        .flat_map(|v| v.to_be_bytes())
        .collect();

    let (kind, config) = (b"fLaC", mp4_box(b"dfLa", &[&[0; 4], flac_blocks]));
    let sample_entry = mp4_box(
        kind,
        &[
            &[0; 6],
            &u16b(1),
            &[0; 8],
            &u16b(u16::from(channels)),
            &u16b(u16::from(bits)),
            &[0; 4],
            &u32b(if rate <= 0xFFFF { rate << 16 } else { 0 }),
            &config,
        ],
    );
    let stsd = mp4_box(b"stsd", &[&[0; 4], &u32b(1), &sample_entry]);
    let mut stts_runs: Vec<(u32, u32)> = Vec::new();
    for &(_, n) in frames {
        match stts_runs.last_mut() {
            Some((count, d)) if *d == n => *count += 1,
            _ => stts_runs.push((1, n)),
        }
    }
    let stts_body: Vec<u8> = stts_runs
        .iter()
        .flat_map(|&(c, d)| [c.to_be_bytes(), d.to_be_bytes()].concat())
        .collect();
    let stts = mp4_box(
        b"stts",
        &[&[0; 4], &u32b(stts_runs.len() as u32), &stts_body],
    );
    let stsc = mp4_box(
        b"stsc",
        &[
            &[0; 4],
            &u32b(1),
            &u32b(1),
            &u32b(frames.len() as u32),
            &u32b(1),
        ],
    );
    let sizes: Vec<u8> = frames
        .iter()
        .flat_map(|(f, _)| (f.len() as u32).to_be_bytes())
        .collect();
    let stsz = mp4_box(
        b"stsz",
        &[&[0; 4], &u32b(0), &u32b(frames.len() as u32), &sizes],
    );
    let ftyp = mp4_box(b"ftyp", &[b"M4A ", &[0; 4], b"M4A mp42isom"]);

    let moov = |mdat_at: u32| -> Vec<u8> {
        let stco = mp4_box(b"stco", &[&[0; 4], &u32b(1), &u32b(mdat_at + 8)]);
        let stbl = mp4_box(b"stbl", &[&stsd, &stts, &stsc, &stsz, &stco]);
        let dinf = mp4_box(
            b"dinf",
            &[&mp4_box(
                b"dref",
                &[&[0; 4], &u32b(1), &mp4_box(b"url ", &[&[0, 0, 0, 1]])],
            )],
        );
        let minf = mp4_box(b"minf", &[&mp4_box(b"smhd", &[&[0; 8]]), &dinf, &stbl]);
        let hdlr = mp4_box(b"hdlr", &[&[0; 8], b"soun", &[0; 12], b"\0"]);
        let mdhd = mp4_box(
            b"mdhd",
            &[&[0; 12], &u32b(rate), &u32b(total), &u16b(0x55C4), &[0; 2]],
        );
        let mdia = mp4_box(b"mdia", &[&mdhd, &hdlr, &minf]);
        let tkhd = mp4_box(
            b"tkhd",
            &[
                &[0, 0, 0, 3],
                &[0; 8],
                &u32b(1),
                &[0; 4],
                &u32b(total),
                &[0; 12],
                &u16b(0x0100),
                &[0; 2],
                &matrix,
                &[0; 8],
            ],
        );
        let trak = mp4_box(b"trak", &[&tkhd, &mdia]);
        let mvhd = mp4_box(
            b"mvhd",
            &[
                &[0; 12],
                &u32b(rate),
                &u32b(total),
                &u32b(0x0001_0000),
                &u16b(0x0100),
                &[0; 10],
                &matrix,
                &[0; 24],
                &u32b(2),
            ],
        );
        mp4_box(b"moov", &[&mvhd, &trak])
    };
    // The chunk offset's value does not change the moov's size.
    let moov_len = moov(0).len();
    let mut out = ftyp.clone();
    out.extend_from_slice(&moov((ftyp.len() + moov_len) as u32));
    let payload: Vec<&[u8]> = frames.iter().map(|(f, _)| f.as_slice()).collect();
    out.extend_from_slice(&mp4_box(b"mdat", &payload));
    out
}

/// An EBML element ID at `at` (its length marker kept): (id, length).
fn ebml_id(data: &[u8], at: usize) -> (u32, usize) {
    let len = data[at].leading_zeros() as usize + 1;
    (
        data[at..at + len]
            .iter()
            .fold(0u32, |v, &b| (v << 8) | u32::from(b)),
        len,
    )
}

/// An EBML variable-length integer at `at` (its length marker removed):
/// (value, length); a size of all ones (unknown) comes back as `None`.
fn ebml_vint(data: &[u8], at: usize) -> (Option<u64>, usize) {
    let len = data[at].leading_zeros() as usize + 1;
    let first = u64::from(data[at]) & (0xFF >> len);
    let v = data[at + 1..at + len]
        .iter()
        .fold(first, |v, &b| (v << 8) | u64::from(b));
    let unknown = v == (1u64 << (7 * len)) - 1;
    (if unknown { None } else { Some(v) }, len)
}

/// A Matroska file's one audio track: CodecID, CodecPrivate, and the frames
/// of its SimpleBlocks and Blocks (unlaced: `mkvmerge --disable-lacing`).
fn read_matroska(data: &[u8]) -> Track {
    const MASTERS: [u32; 5] = [0x1853_8067, 0x1654_AE6B, 0xAE, 0x1F43_B675, 0xA0];
    let (mut codec, mut config, mut packets) = (None, Vec::new(), Vec::new());
    let mut at = 0;
    while at < data.len() {
        let (id, n) = ebml_id(data, at);
        at += n;
        let (size, n) = ebml_vint(data, at);
        at += n;
        if MASTERS.contains(&id) {
            continue; // descend
        }
        let size = size.expect("an unknown size on an element that is not a master") as usize;
        let body = &data[at..at + size];
        match id {
            0x86 => {
                codec = Some(match body {
                    b"A_FLAC" => "flac",
                    b"A_ALAC" => "alac",
                    other => panic!("CodecID {}", String::from_utf8_lossy(other)),
                })
            }
            0x63A2 => config = body.to_vec(),
            0xA3 | 0xA1 => {
                let (_, track_len) = ebml_vint(body, 0);
                let flags = body[track_len + 2];
                assert_eq!(flags & 0x06, 0, "laced block");
                packets.push(body[track_len + 3..].to_vec());
            }
            _ => {}
        }
        at += size;
    }
    Track {
        codec: codec.expect("a CodecID"),
        config,
        packets,
    }
}

// ---------------------------------------------------------------------------
// Core Audio Format (Apple's published CAF specification): what
// `alacconvert` reads and writes. A file header, then chunks of a type and a
// 64-bit big-endian size: `desc` (the stream format), `kuki` (the codec's
// magic cookie), `pakt` (the packet table) and `data` (a 32-bit edit count,
// then the audio).
// ---------------------------------------------------------------------------

const CAF_LPCM: u32 = u32::from_be_bytes(*b"lpcm");
const CAF_ALAC: u32 = u32::from_be_bytes(*b"alac");
/// `kCAFLinearPCMFormatFlagIsLittleEndian`.
const CAF_LITTLE_ENDIAN: u32 = 2;

fn caf_chunk(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = kind.to_vec();
    out.extend_from_slice(&(body.len() as i64).to_be_bytes());
    out.extend_from_slice(body);
    out
}

fn caf_desc(
    rate: u32,
    format: u32,
    flags: u32,
    bytes_per_packet: u32,
    frames_per_packet: u32,
    channels: usize,
    bits: u32,
) -> Vec<u8> {
    let mut d = f64::from(rate).to_be_bytes().to_vec();
    for v in [
        format,
        flags,
        bytes_per_packet,
        frames_per_packet,
        channels as u32,
        bits,
    ] {
        d.extend_from_slice(&v.to_be_bytes());
    }
    caf_chunk(b"desc", &d)
}

fn caf_data(audio: &[u8]) -> Vec<u8> {
    let mut body = 0u32.to_be_bytes().to_vec();
    body.extend_from_slice(audio);
    caf_chunk(b"data", &body)
}

/// Little-endian signed integer PCM, `bits` in whole bytes.
fn caf_pcm(pcm: &[i32], rate: u32, channels: usize, bits: u32) -> Vec<u8> {
    let width = bits.div_ceil(8);
    let mut out = b"caff\x00\x01\x00\x00".to_vec();
    out.extend(caf_desc(
        rate,
        CAF_LPCM,
        CAF_LITTLE_ENDIAN,
        width * channels as u32,
        1,
        channels,
        bits,
    ));
    out.extend(caf_data(&raw_bytes(pcm, bits)));
    out
}

/// ALAC packets: `desc` names the depth in its flags (1–4 for 16, 20, 24
/// and 32 bits), `kuki` is the 24-byte cookie, `pakt` lists each packet's
/// size as a variable-length integer and the frames the last one pads.
fn caf_alac(
    cookie: &[u8],
    rate: u32,
    channels: usize,
    bits: u32,
    valid: u64,
    frames: &[(Vec<u8>, u32)],
) -> Vec<u8> {
    let depth_flag = match bits {
        16 => 1,
        20 => 2,
        24 => 3,
        32 => 4,
        b => panic!("{b}-bit ALAC"),
    };
    let per_packet = u32::from_be_bytes(cookie[..4].try_into().unwrap());
    let mut pakt = (frames.len() as i64).to_be_bytes().to_vec();
    pakt.extend_from_slice(&(valid as i64).to_be_bytes());
    pakt.extend_from_slice(&0i32.to_be_bytes());
    pakt.extend_from_slice(
        &((u64::from(per_packet) * frames.len() as u64 - valid) as i32).to_be_bytes(),
    );
    for (f, _) in frames {
        let mut v = f.len() as u64;
        let mut bytes = vec![(v & 0x7F) as u8];
        v >>= 7;
        while v > 0 {
            bytes.push((v & 0x7F) as u8 | 0x80);
            v >>= 7;
        }
        pakt.extend(bytes.iter().rev());
    }
    let audio: Vec<u8> = frames.iter().flat_map(|(f, _)| f.iter().copied()).collect();
    let mut out = b"caff\x00\x01\x00\x00".to_vec();
    out.extend(caf_desc(
        rate, CAF_ALAC, depth_flag, 0, per_packet, channels, 0,
    ));
    out.extend(caf_chunk(b"kuki", cookie));
    out.extend(caf_chunk(b"pakt", &pakt));
    out.extend(caf_data(&audio));
    out
}

/// What a CAF file holds: integer PCM (interleaved, in the file's channel
/// order) or ALAC packets, and the packet table's valid-frame count.
struct Caf {
    pcm: Option<Vec<i32>>,
    alac: Option<Track>,
    valid_frames: Option<u64>,
}

fn read_caf(data: &[u8]) -> Caf {
    assert_eq!(&data[..4], b"caff", "a CAF file");
    let mut chunks = std::collections::HashMap::new();
    let mut at = 8;
    while at + 12 <= data.len() {
        let kind: [u8; 4] = data[at..at + 4].try_into().unwrap();
        let size = i64::from_be_bytes(data[at + 4..at + 12].try_into().unwrap());
        // A size of -1 is a `data` chunk running to the end of the file.
        let end = if size < 0 {
            data.len()
        } else {
            at + 12 + size as usize
        };
        chunks.insert(kind, &data[at + 12..end]);
        at = end;
    }
    let desc = chunks[b"desc"];
    let field = |i: usize| be32(desc, 8 + 4 * i);
    let (format, flags, bytes_per_packet, channels, bits) =
        (field(0), field(1), field(2), field(4), field(5));
    let audio = &chunks[b"data"][4..];
    if format == CAF_LPCM {
        assert_eq!(flags & 1, 0, "integer PCM");
        let width = (bytes_per_packet / channels) as usize;
        assert!(
            width * 8 >= bits as usize && width <= 4,
            "{bits} bits in {width} bytes"
        );
        let pcm = audio
            .chunks_exact(width)
            .map(|b| {
                let mut v = [0u8; 4];
                if flags & CAF_LITTLE_ENDIAN != 0 {
                    v[4 - width..].copy_from_slice(b);
                    i32::from_le_bytes(v) >> (32 - 8 * width)
                } else {
                    v[..width].copy_from_slice(b);
                    i32::from_be_bytes(v) >> (32 - 8 * width)
                }
            })
            .collect();
        return Caf {
            pcm: Some(pcm),
            alac: None,
            valid_frames: None,
        };
    }
    assert_eq!(format, CAF_ALAC, "lpcm or alac");
    let pakt = chunks[b"pakt"];
    let count = i64::from_be_bytes(pakt[..8].try_into().unwrap()) as usize;
    let valid = i64::from_be_bytes(pakt[8..16].try_into().unwrap()) as u64;
    let mut at = 24;
    let mut packets = Vec::with_capacity(count);
    let mut offset = 0;
    for _ in 0..count {
        let mut len = 0usize;
        loop {
            let b = pakt[at];
            at += 1;
            len = (len << 7) | usize::from(b & 0x7F);
            if b & 0x80 == 0 {
                break;
            }
        }
        packets.push(audio[offset..offset + len].to_vec());
        offset += len;
    }
    let config = chunks[b"kuki"].to_vec();
    Caf {
        pcm: None,
        alac: Some(Track {
            codec: "alac",
            config,
            packets,
        }),
        valid_frames: Some(valid),
    }
}
