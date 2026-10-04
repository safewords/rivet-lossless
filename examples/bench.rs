//! Throughput of the four codecs on a native FLAC file:
//! `cargo run --release --example bench -- <file.flac> [seconds] [runs]`.
//!
//! The file is decoded (its MD5 checked), then its first `seconds` (default
//! 60) are encoded as FLAC at every level and as ALAC, and those streams
//! decoded back, each the best of `runs` (default 3) timings. Every round
//! trip is checked, and each encoded stream's FNV-1a hash is printed so a
//! change to the encoders' output shows. Encoders run on one thread, then
//! on all of them (frame-parallel).

use std::time::Instant;

use lossless::{alac, flac};

fn fnv(frames: &[(Vec<u8>, u32)]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for (f, _) in frames {
        for &b in f {
            h = (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
        }
    }
    h
}

fn best<T>(runs: usize, mut f: impl FnMut() -> T) -> (f64, T) {
    let mut out = None;
    let mut t = f64::MAX;
    for _ in 0..runs {
        let s = Instant::now();
        let v = f();
        t = t.min(s.elapsed().as_secs_f64());
        out = Some(v);
    }
    (t, out.expect("at least one run"))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).expect("usage: bench <file.flac> [seconds] [runs]");
    let seconds: f64 = args.get(2).map_or(60.0, |s| s.parse().expect("seconds"));
    let runs: usize = args.get(3).map_or(3, |s| s.parse().expect("runs"));
    let data = std::fs::read(path).expect("read");
    assert_eq!(&data[..4], b"fLaC");
    let (info, meta) = flac::parse_metadata_blocks(&data[4..]).expect("metadata");
    let frames = &data[4 + meta..];
    let (rate, ch, bits) = (info.sample_rate, info.channels, u32::from(info.bits_per_sample));

    let (t, pcm) = best(runs, || {
        let mut d = flac::Decoder::new(Some(&data[..4 + meta]), 0, 0).expect("decoder");
        let (pcm, _, _) = d.decode_int(frames).expect("decode");
        assert_eq!(d.md5_matches(), Some(true), "MD5");
        pcm
    });
    let dur = pcm.len() as f64 / f64::from(ch) / f64::from(rate);
    println!("{ch} ch, {bits} bit, {rate} Hz, {dur:.1} s");
    println!("flac decode (file)      {:8.1} x realtime", dur / t);

    let take = ((seconds * f64::from(rate)) as usize * usize::from(ch)).min(pcm.len());
    let pcm = &pcm[..take];
    let dur = take as f64 / f64::from(ch) / f64::from(rate);
    for level in [flac::Level::Fast, flac::Level::Default, flac::Level::Best] {
        let (tm, _) = best(runs, || {
            let mut e = flac::Encoder::new(flac::EncoderConfig {
                sample_rate: rate,
                channels: ch,
                bits_per_sample: bits as u8,
                level,
            })
            .expect("encoder");
            let mut f = e.encode_int(pcm);
            f.extend(e.finish());
            f
        });
        let (t, (frames, head)) = best(runs, || {
            let mut e = flac::Encoder::new(flac::EncoderConfig {
                sample_rate: rate,
                channels: ch,
                bits_per_sample: bits as u8,
                level,
            })
            .expect("encoder");
            e.set_threads(1);
            let mut f = e.encode_int(pcm);
            f.extend(e.finish());
            (f, e.metadata_blocks())
        });
        let size: usize = frames.iter().map(|f| f.0.len()).sum();
        let stream: Vec<u8> = frames.iter().flat_map(|f| f.0.iter().copied()).collect();
        let (td, out) = best(runs, || {
            let mut d = flac::Decoder::new(Some(&head), 0, 0).expect("decoder");
            let out = d.decode_int(&stream).expect("decode").0;
            assert_eq!(d.md5_matches(), Some(true));
            out
        });
        assert!(out == pcm, "FLAC {level:?} round trip");
        println!(
            "flac encode {:<8}    {:8.1} x realtime, {:8.1} x threaded  ({:.2}% of PCM, hash {:016x}); decode {:8.1} x",
            format!("{level:?}"),
            dur / t,
            dur / tm,
            100.0 * size as f64 / (take as f64 * f64::from(bits) / 8.0),
            fnv(&frames),
            dur / td
        );
    }
    let abits = if matches!(bits, 16 | 20 | 24 | 32) { bits } else { 24 };
    let shift = abits - bits;
    let apcm: Vec<i32> = pcm.iter().map(|&s| s << shift).collect();
    let (tm, _) = best(runs, || {
        let mut e = alac::Encoder::new(rate, ch, abits as u8).expect("encoder");
        let mut f = e.encode_int(&apcm);
        f.extend(e.finish());
        f
    });
    let (t, (frames, cookie)) = best(runs, || {
        let mut e = alac::Encoder::new(rate, ch, abits as u8).expect("encoder");
        e.set_threads(1);
        let mut f = e.encode_int(&apcm);
        f.extend(e.finish());
        (f, e.cookie())
    });
    let size: usize = frames.iter().map(|f| f.0.len()).sum();
    let (td, out) = best(runs, || {
        let mut d = alac::Decoder::new(Some(&cookie.to_bytes())).expect("decoder");
        let mut out = Vec::with_capacity(apcm.len());
        for (f, _) in &frames {
            out.extend(d.decode_int(f).expect("decode"));
        }
        out
    });
    assert!(out == apcm, "ALAC round trip");
    println!(
        "alac encode             {:8.1} x realtime, {:8.1} x threaded  ({:.2}% of PCM, hash {:016x}); decode {:8.1} x",
        dur / t,
        dur / tm,
        100.0 * size as f64 / (take as f64 * f64::from(abits) / 8.0),
        fnv(&frames),
        dur / td
    );
}
