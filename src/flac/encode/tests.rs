use super::*;
use crate::flac::{Decoder, decode_frame};

fn signal(frames: usize, channels: usize, bits: u32, seed: u32) -> Vec<i32> {
    let full = ((1i64 << (bits - 1)) - 1) as f64;
    let mut rng = seed.max(1);
    let mut noise = move || {
        rng ^= rng << 13;
        rng ^= rng >> 17;
        rng ^= rng << 5;
        f64::from(rng) / f64::from(u32::MAX) - 0.5
    };
    let mut out = Vec::with_capacity(frames * channels);
    for i in 0..frames {
        let t = i as f64 / 44_100.0;
        let common = (t * 2.0 * std::f64::consts::PI * 440.0).sin() * 0.5;
        for c in 0..channels {
            let v = if i % 9000 < 700 {
                0.0
            } else {
                common * (1.0 - 0.2 * c as f64) + noise() * 0.01 * (c + 1) as f64
            };
            out.push((v * full).round().clamp(-full - 1.0, full) as i32);
        }
    }
    out
}

fn round_trip(pcm: &[i32], channels: u8, bits: u8, level: Level) -> (Vec<u8>, StreamInfo) {
    let mut enc = Encoder::new(EncoderConfig {
        sample_rate: 44_100,
        channels,
        bits_per_sample: bits,
        level,
    })
    .unwrap();
    let mut frames = enc.encode_int(pcm);
    frames.extend(enc.finish());
    let info = enc.stream_info();
    let mut dec = Decoder::new(Some(&enc.metadata_blocks()), 44_100, channels).unwrap();
    let mut got = Vec::new();
    let mut stream = Vec::new();
    for (f, n) in &frames {
        let (s, ch, b) = dec.decode_int(f).unwrap();
        assert_eq!((ch, b), (channels, u32::from(bits)));
        assert_eq!(s.len(), *n as usize * usize::from(channels));
        got.extend(s);
        stream.extend_from_slice(f);
    }
    assert!(
        got == pcm,
        "{channels}ch {bits}-bit {level:?}: round trip differs"
    );
    assert_eq!(dec.md5_matches(), Some(true), "MD5");
    (stream, info)
}

#[test]
fn every_level_depth_and_layout_round_trips() {
    for level in [Level::Fast, Level::Default, Level::Best] {
        for (channels, bits) in [(1u8, 16u8), (2, 16), (2, 24), (6, 24), (8, 16)] {
            let pcm = signal(10_000, usize::from(channels), u32::from(bits), 7);
            round_trip(&pcm, channels, bits, level);
        }
    }
    for bits in [4u8, 8, 12, 20, 32] {
        let pcm = signal(5_000, 2, u32::from(bits), 3);
        round_trip(&pcm, 2, bits, Level::Default);
    }
}

#[test]
fn edge_shapes_round_trip() {
    // Shorter than one block, shorter than 16 samples, exactly one block,
    // all silence, a constant, wasted low bits, full-scale square waves.
    let cases: Vec<(Vec<i32>, u8)> = vec![
        (signal(1_000, 2, 16, 1), 2),
        (signal(5, 1, 16, 1), 1),
        (signal(BLOCK_SIZE, 2, 16, 2), 2),
        (vec![0; 9_000], 2),
        (vec![1234; 9_000], 1),
        (
            signal(9_000, 2, 16, 4).iter().map(|s| s & !0xFF).collect(),
            2,
        ),
        (
            (0..9_000)
                .map(|i| if (i / 3) % 2 == 0 { 32_767 } else { -32_768 })
                .collect(),
            1,
        ),
    ];
    for (pcm, ch) in cases {
        let (_, info) = round_trip(&pcm, ch, 16, Level::Default);
        assert_eq!(info.total_samples, (pcm.len() / usize::from(ch)) as u64);
    }
}

#[test]
fn stereo_decorrelation_is_used_on_correlated_channels() {
    // Identical channels: the side channel is all zeros.
    let mono = signal(BLOCK_SIZE, 1, 16, 9);
    let pcm: Vec<i32> = mono.iter().flat_map(|&s| [s, s]).collect();
    let mut enc = Encoder::new(EncoderConfig {
        sample_rate: 48_000,
        channels: 2,
        bits_per_sample: 16,
        level: Level::Default,
    })
    .unwrap();
    let frames = enc.encode_int(&pcm);
    let frame = decode_frame(&frames[0].0, None).unwrap();
    assert!(
        matches!(frame.header.assignment, 8..=10),
        "assignment {}",
        frame.header.assignment
    );
    assert_eq!(frame.samples, pcm);
}

#[test]
fn coded_numbers_use_the_utf8_form() {
    for (v, want) in [
        (0x7Fu64, vec![0x7F]),
        (0x80, vec![0xC2, 0x80]),
        (0x7FF, vec![0xDF, 0xBF]),
        (0x800, vec![0xE0, 0xA0, 0x80]),
        (
            (1 << 36) - 1,
            vec![0xFE, 0xBF, 0xBF, 0xBF, 0xBF, 0xBF, 0xBF],
        ),
    ] {
        let mut bw = BitWriter::default();
        write_coded_number(&mut bw, v);
        assert_eq!(bw.into_bytes(), want, "{v:#x}");
    }
}

#[test]
fn streaminfo_describes_the_stream() {
    let pcm = signal(10_000, 2, 16, 5);
    let (_, info) = round_trip(&pcm, 2, 16, Level::Default);
    assert_eq!((info.min_block_size, info.max_block_size), (4096, 4096));
    assert_eq!(info.total_samples, 10_000);
    assert!(info.min_frame_size > 0 && info.min_frame_size <= info.max_frame_size);
    assert_ne!(info.md5, [0; 16]);
}

/// FNV-1a of every frame the encoder makes of `pcm` on `threads` threads.
fn stream_hash(pcm: &[i32], channels: u8, bits: u8, level: Level, threads: usize) -> u64 {
    let mut enc = Encoder::new(EncoderConfig {
        sample_rate: 44_100,
        channels,
        bits_per_sample: bits,
        level,
    })
    .unwrap();
    enc.set_threads(threads);
    let mut frames = enc.encode_int(pcm);
    frames.extend(enc.finish());
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in frames.iter().flat_map(|f| f.0.iter()) {
        h = (h ^ u64::from(*b)).wrapping_mul(0x100_0000_01b3);
    }
    h
}

#[test]
fn the_encoded_bytes_do_not_change() {
    // The encoder's choices are exact (integer sizes, a fixed order of
    // floating-point operations), so its output is the same on every CPU
    // and every code path; these are the hashes of its output before the
    // vector kernels and threads went in.
    let mut got = Vec::new();
    for level in [Level::Fast, Level::Default, Level::Best] {
        for (channels, bits) in [(2u8, 16u8), (2, 24), (6, 20), (1, 32), (2, 32), (1, 8)] {
            let mut pcm = signal(30_000, usize::from(channels), u32::from(bits), 11);
            // Some full-scale noise, for the wide residuals and escapes.
            for (i, s) in pcm
                .iter_mut()
                .enumerate()
                .skip(20_000 * usize::from(channels))
                .take(2_000)
            {
                *s = ((i as u32).wrapping_mul(2_654_435_761) as i32) >> (32 - u32::from(bits));
            }
            let h = stream_hash(&pcm, channels, bits, level, 1);
            assert_eq!(stream_hash(&pcm, channels, bits, level, 3), h, "threaded");
            got.push(h);
        }
    }
    let want: [u64; 18] = [
        0x5f38e97c86ebefc4,
        0x508227b143274966,
        0x2a31ae0c7b219756,
        0xf3d02ba2f6a40560,
        0xa1568e94d7c42b67,
        0xa2da222dadd9f273,
        0x9a9f2cac3d20c787,
        0x58ecdd82f2b60eb0,
        0xbb38863ffe3cdb98,
        0x1f3e8758b6dedd39,
        0x36d521012ec4152a,
        0xc4a662063af53dec,
        0x82e9214629244cb5,
        0x1eadf46301d5818a,
        0x9d09099e9710464e,
        0x727b4bc1e7fda6af,
        0xcb46400e32a57a9a,
        0x120019d4ebdfb24d,
    ];
    assert_eq!(got, want, "{got:#018x?}");
}

#[test]
fn a_wrong_md5_is_caught_with_the_hash_off_the_decoding_thread() {
    let pcm = signal(30_000, 2, 24, 11);
    let mut enc = Encoder::new(EncoderConfig {
        sample_rate: 44_100,
        channels: 2,
        bits_per_sample: 24,
        level: Level::Fast,
    })
    .unwrap();
    let mut frames = enc.encode_int(&pcm);
    frames.extend(enc.finish());
    let md5 = enc.stream_info().md5;
    let head = enc.metadata_blocks();
    let at = head
        .windows(16)
        .position(|w| w == md5)
        .expect("the MD5 in STREAMINFO");
    for (wrong, expect) in [(false, true), (true, false)] {
        let mut extra = head.clone();
        if wrong {
            extra[at + 15] ^= 1;
        }
        let mut dec = Decoder::new(Some(&extra), 44_100, 2).unwrap();
        for (i, (f, _)) in frames.iter().enumerate() {
            dec.decode_int(f).unwrap();
            if i + 1 < frames.len() {
                assert_eq!(dec.md5_matches(), None, "not at the end yet");
            }
        }
        assert_eq!(dec.md5_matches(), Some(expect), "MD5 altered: {wrong}");
        // Asking again gives the same answer.
        assert_eq!(dec.md5_matches(), Some(expect));
    }
}
