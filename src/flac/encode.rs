//! FLAC encoder, clean-room from RFC 9639 and the published literature on
//! linear prediction and Rice coding.
//!
//! A fixed-block-size stream of 4096-sample frames. For every subframe the
//! encoder weighs the forms the format has — constant, verbatim, the fixed
//! polynomial predictors of orders 0–4 and LPC — and keeps the cheapest in
//! exact bits; LPC coefficients come from the autocorrelation of a
//! Tukey-windowed block through Levinson-Durbin, quantised with error
//! feedback. Residuals are Rice coded in partitions whose order and
//! parameters are searched per subframe, with the escape to raw bits where
//! that is smaller. Stereo frames try all four channel assignments
//! (independent, left/side, side/right, mid/side). Low bits that are zero
//! throughout a block ("wasted bits") are shifted out.
//!
//! [`Level`] trades speed for size: `Fast` stops at the fixed
//! predictors, `Default` adds LPC up to order 8 with the order picked from
//! the Levinson error estimate, `Best` tries every LPC order to 12 exactly.
//!
//! The stream's STREAMINFO — frame size bounds, sample count and the MD5 of
//! the audio — is complete once [`Encoder::finish`] has run, which is when
//! a muxer should ask for [`Encoder::metadata_blocks`].

use super::format::{BLOCK_STREAMINFO, StreamInfo, block_header, crc8, crc16, md5_bytes};
use crate::Error;
use crate::bits::BitWriter;
use crate::lpc;

#[cfg(test)]
mod tests;

/// Samples per channel in every frame but the last.
pub const BLOCK_SIZE: usize = 4096;

/// Compression effort.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Level {
    /// Fixed predictors only, Rice partitions to order 3.
    Fast,
    /// LPC to order 8 (order picked by estimate), partitions to order 6.
    #[default]
    Default,
    /// LPC to order 12 (every order tried), partitions to order 8.
    Best,
}

impl Level {
    fn max_lpc_order(self) -> usize {
        match self {
            Self::Fast => 0,
            Self::Default => 8,
            Self::Best => 12,
        }
    }

    fn max_partition_order(self) -> u32 {
        match self {
            Self::Fast => 3,
            Self::Default => 6,
            Self::Best => 8,
        }
    }
}

/// What a FLAC encoder is built for.
#[derive(Clone, Copy, Debug)]
pub struct EncoderConfig {
    pub sample_rate: u32,
    /// 1–8, in FLAC's order ([`layout`](super::layout)).
    pub channels: u8,
    /// 4–32.
    pub bits_per_sample: u8,
    pub level: Level,
}

/// A FLAC stream's encoder: interleaved integer samples in, whole frames
/// out, and the STREAMINFO that describes them.
pub struct Encoder {
    config: EncoderConfig,
    /// Interleaved samples waiting for a whole frame.
    pending: Vec<i32>,
    frames: u64,
    samples: u64,
    min_frame: u32,
    max_frame: u32,
    /// The size of the only block, while there has been one.
    last_block: usize,
    md5: crate::md5::Md5,
    md5_scratch: Vec<u8>,
    md5_digest: Option<[u8; 16]>,
    /// Threads for a batch of whole frames; 0 is the machine's count.
    threads: usize,
    /// The analysis window of a whole block.
    window: Vec<f64>,
}

impl Encoder {
    /// An encoder for `config`; refused (`Error::Unsupported`) outside 1–8
    /// channels, 4–32 bits or a sample rate of 1 Hz to 2^20 - 1 Hz.
    pub fn new(config: EncoderConfig) -> Result<Self, Error> {
        if !(1..=8).contains(&config.channels) {
            return Err(Error::Unsupported(format!("flac: {} channels (1–8)", config.channels)));
        }
        if !(4..=32).contains(&config.bits_per_sample) {
            return Err(Error::Unsupported(format!("flac: {}-bit samples (4–32)", config.bits_per_sample)));
        }
        if config.sample_rate == 0 || config.sample_rate >= 1 << 20 {
            return Err(Error::Unsupported(format!("flac: sample rate {} Hz", config.sample_rate)));
        }
        Ok(Self {
            config,
            pending: Vec::new(),
            frames: 0,
            samples: 0,
            min_frame: u32::MAX,
            max_frame: 0,
            last_block: 0,
            md5: crate::md5::Md5::new(),
            md5_scratch: Vec::new(),
            md5_digest: None,
            threads: 0,
            window: if config.level.max_lpc_order() > 0 { lpc::tukey(BLOCK_SIZE, 0.5) } else { Vec::new() },
        })
    }

    /// What the encoder was built for.
    pub fn config(&self) -> &EncoderConfig {
        &self.config
    }

    /// How many threads code a batch of whole frames (the frames one
    /// [`encode_int`](Self::encode_int) call completes): 0, the default,
    /// is one per CPU; 1 codes everything on the caller's thread. The
    /// stream is the same byte for byte whatever the count.
    pub fn set_threads(&mut self, threads: usize) {
        self.threads = threads;
    }

    /// Encode interleaved integer samples; returns the frames completed, each
    /// with its sample count.
    pub fn encode_int(&mut self, samples: &[i32]) -> Vec<(Vec<u8>, u32)> {
        self.pending.extend_from_slice(samples);
        let ch = usize::from(self.config.channels);
        let len = BLOCK_SIZE * ch;
        let whole = self.pending.len() / len;
        let bps = u32::from(self.config.bits_per_sample);
        for block in self.pending[..whole * len].chunks_exact(len) {
            self.md5_scratch.clear();
            md5_bytes(block, bps, &mut self.md5_scratch);
            self.md5.consume(&self.md5_scratch);
        }
        let threads = if self.threads == 0 { crate::parallel::auto_threads() } else { self.threads };
        let (config, first, window, pending) = (&self.config, self.frames, &self.window, &self.pending);
        let frames = crate::parallel::map(whole, threads, |i| {
            encode_frame(config, first + i as u64, &pending[i * len..(i + 1) * len], window)
        });
        self.pending.drain(..whole * len);
        frames
            .into_iter()
            .map(|frame| {
                self.count_frame(BLOCK_SIZE, frame.len());
                (frame, BLOCK_SIZE as u32)
            })
            .collect()
    }

    /// Encode what is left as a final, shorter frame and seal the MD5.
    pub fn finish(&mut self) -> Vec<(Vec<u8>, u32)> {
        let mut out = Vec::new();
        if !self.pending.is_empty() {
            let block = std::mem::take(&mut self.pending);
            let n = block.len() / usize::from(self.config.channels);
            out.push((self.encode_block(&block), n as u32));
        }
        if self.md5_digest.is_none() {
            self.md5_digest = Some(self.md5.compute());
        }
        out
    }

    /// The stream's STREAMINFO as it stands (complete after [`finish`](Self::finish)).
    pub fn stream_info(&self) -> StreamInfo {
        let block = if self.frames <= 1 { self.last_block.max(16) } else { BLOCK_SIZE } as u16;
        StreamInfo {
            min_block_size: block,
            max_block_size: block,
            min_frame_size: if self.max_frame == 0 { 0 } else { self.min_frame },
            max_frame_size: self.max_frame,
            sample_rate: self.config.sample_rate,
            channels: self.config.channels,
            bits_per_sample: self.config.bits_per_sample,
            total_samples: self.samples,
            md5: self.md5_digest.unwrap_or([0; 16]),
        }
    }

    /// The metadata blocks a container carries (`dfLa` body, Matroska
    /// CodecPrivate after `fLaC`): STREAMINFO alone, flagged last.
    pub fn metadata_blocks(&self) -> Vec<u8> {
        let mut b = block_header(true, BLOCK_STREAMINFO, StreamInfo::LEN).to_vec();
        b.extend_from_slice(&self.stream_info().to_bytes());
        b
    }

    fn encode_block(&mut self, interleaved: &[i32]) -> Vec<u8> {
        let ch = usize::from(self.config.channels);
        let n = interleaved.len() / ch;
        let bps = u32::from(self.config.bits_per_sample);
        self.md5_scratch.clear();
        md5_bytes(interleaved, bps, &mut self.md5_scratch);
        self.md5.consume(&self.md5_scratch);
        let frame = encode_frame(&self.config, self.frames, interleaved, &self.window);
        self.count_frame(n, frame.len());
        frame
    }

    fn count_frame(&mut self, n: usize, bytes: usize) {
        self.frames += 1;
        self.samples += n as u64;
        self.last_block = n;
        self.min_frame = self.min_frame.min(bytes as u32);
        self.max_frame = self.max_frame.max(bytes as u32);
    }
}

/// Code one block as frame number `frame`. `window` is the analysis window
/// of a whole block (`BLOCK_SIZE`), used when the block is one.
fn encode_frame(config: &EncoderConfig, frame: u64, interleaved: &[i32], window: &[f64]) -> Vec<u8> {
    let ch = usize::from(config.channels);
    let n = interleaved.len() / ch;
    let bps = u32::from(config.bits_per_sample);
    let chans: Vec<Vec<i64>> =
        (0..ch).map(|c| interleaved.iter().skip(c).step_by(ch).map(|&s| i64::from(s)).collect()).collect();
    let level = config.level;
    let own;
    let window = if level.max_lpc_order() == 0 || n < 2 {
        &[][..]
    } else if window.len() == n {
        window
    } else {
        own = lpc::tukey(n, 0.5);
        &own[..]
    };
    let (assignment, subframes) = if ch == 2 {
        let (l, r) = (&chans[0], &chans[1]);
        let mid: Vec<i64> = l.iter().zip(r).map(|(a, b)| (a + b) >> 1).collect();
        let side: Vec<i64> = l.iter().zip(r).map(|(a, b)| a - b).collect();
        let pl = plan_subframe(l, bps, level, window);
        let pr = plan_subframe(r, bps, level, window);
        let pm = plan_subframe(&mid, bps, level, window);
        let ps = plan_subframe(&side, bps + 1, level, window);
        let options =
            [(1u8, pl.bits + pr.bits), (8, pl.bits + ps.bits), (9, ps.bits + pr.bits), (10, pm.bits + ps.bits)];
        let best = options.iter().min_by_key(|o| o.1).expect("four options").0;
        let pair = match best {
            1 => vec![pl, pr],
            8 => vec![pl, ps],
            9 => vec![ps, pr],
            _ => vec![pm, ps],
        };
        (best, pair)
    } else {
        ((ch - 1) as u8, chans.iter().map(|c| plan_subframe(c, bps, level, window)).collect())
    };

    let mut bw = BitWriter::with_capacity(n * ch * bps as usize / 8 + 64);
    write_frame_header(&mut bw, frame, n, config.sample_rate, assignment, bps);
    for s in &subframes {
        write_subframe(&mut bw, s);
    }
    bw.align();
    let crc = crc16(bw.bytes());
    bw.write(u64::from(crc), 16);
    bw.into_bytes()
}

/// The frame header (§9.1) of a fixed-block-size stream, CRC-8 included.
fn write_frame_header(bw: &mut BitWriter, frame: u64, n: usize, rate: u32, assignment: u8, bps: u32) {
    let start = bw.len_bits();
    debug_assert_eq!(start % 8, 0);
    let (bs_code, bs_extra) = match n {
        192 => (1, None),
        576 | 1152 | 2304 | 4608 => (2 + (n / 576).trailing_zeros(), None),
        256 | 512 | 1024 | 2048 | 4096 | 8192 | 16384 | 32768 => (8 + (n / 256).trailing_zeros(), None),
        n if n <= 256 => (6, Some(((n - 1) as u64, 8))),
        n => (7, Some(((n - 1) as u64, 16))),
    };
    let (sr_code, sr_extra) = match rate {
        88_200 => (1, None),
        176_400 => (2, None),
        192_000 => (3, None),
        8_000 => (4, None),
        16_000 => (5, None),
        22_050 => (6, None),
        24_000 => (7, None),
        32_000 => (8, None),
        44_100 => (9, None),
        48_000 => (10, None),
        96_000 => (11, None),
        r if r % 1000 == 0 && r / 1000 <= 255 => (12, Some((u64::from(r / 1000), 8))),
        r if r <= 0xFFFF => (13, Some((u64::from(r), 16))),
        r if r % 10 == 0 && r / 10 <= 0xFFFF => (14, Some((u64::from(r / 10), 16))),
        // Only in STREAMINFO.
        _ => (0, None),
    };
    let bps_code = match bps {
        8 => 1,
        12 => 2,
        16 => 4,
        20 => 5,
        24 => 6,
        32 => 7,
        _ => 0,
    };
    bw.write(0x3FFE, 14);
    bw.write(0, 1);
    bw.write(0, 1); // fixed block size
    bw.write(u64::from(bs_code), 4);
    bw.write(sr_code, 4);
    bw.write(u64::from(assignment), 4);
    bw.write(bps_code, 3);
    bw.write(0, 1);
    write_coded_number(bw, frame);
    if let Some((v, n)) = bs_extra {
        bw.write(v, n);
    }
    if let Some((v, n)) = sr_extra {
        bw.write(v, n);
    }
    let crc = crc8(&bw.bytes()[start / 8..]);
    bw.write(u64::from(crc), 8);
}

/// The UTF-8-style coded number (§9.1.5).
fn write_coded_number(bw: &mut BitWriter, v: u64) {
    if v < 0x80 {
        bw.write(v, 8);
        return;
    }
    // Continuation bytes carry 6 bits each; the lead byte what is left.
    let extra = (1..=6).find(|&k| v < 1u64 << (6 * k + 6 - k)).unwrap_or(6);
    let lead_bits = 6 - extra;
    let marker = (0xFFu64 << (7 - extra)) & 0xFF;
    bw.write(marker | (v >> (6 * extra)) & ((1 << lead_bits) - 1), 8);
    for k in (0..extra).rev() {
        bw.write(0x80 | ((v >> (6 * k)) & 0x3F), 8);
    }
}

/// How one subframe will be written.
struct SubframePlan {
    kind: SubKind,
    /// Wasted low bits shifted out.
    wasted: u32,
    /// Bits per sample after the shift.
    bps: u32,
    /// The samples after the shift (warm-up and verbatim come from here).
    samples: Vec<i64>,
    residual: Vec<i64>,
    rice: Option<RicePlan>,
    /// Exact size in bits.
    bits: usize,
}

enum SubKind {
    Constant,
    Verbatim,
    Fixed(usize),
    Lpc { coefs: Vec<i32>, precision: u32, shift: i32 },
}

impl SubKind {
    fn order(&self) -> usize {
        match self {
            SubKind::Fixed(o) => *o,
            SubKind::Lpc { coefs, .. } => coefs.len(),
            _ => 0,
        }
    }
}

/// Rice partitioning of one residual (§9.2.7).
struct RicePlan {
    order: u32,
    /// Per partition: the Rice parameter, or `None` for the escape (with the
    /// raw bit width).
    params: Vec<Result<u32, u32>>,
    /// Coding method 1 (5-bit parameters) when some parameter needs it.
    wide: bool,
    bits: usize,
    /// Per partition, the OR of its folded values.
    ors: Vec<u32>,
}

/// Working buffers of one subframe's planning, reused across candidates.
#[derive(Default)]
struct Scratch {
    /// The candidate residual.
    residual: Vec<i64>,
    /// Its folded (zigzag) form.
    folded: Vec<u32>,
}

fn plan_subframe(x: &[i64], bps: u32, level: Level, window: &[f64]) -> SubframePlan {
    let n = x.len();
    // Constant: one sample.
    if x.iter().all(|&v| v == x[0]) {
        return SubframePlan {
            kind: SubKind::Constant,
            wasted: 0,
            bps,
            samples: vec![x[0]],
            residual: Vec::new(),
            rice: None,
            bits: 8 + bps as usize,
        };
    }
    let or = x.iter().fold(0i64, |a, &v| a | v);
    let wasted = (or.trailing_zeros()).min(bps - 1);
    let samples: Vec<i64> = if wasted > 0 { x.iter().map(|&v| v >> wasted).collect() } else { x.to_vec() };
    let ebps = bps - wasted;
    // Header: padding bit, type, wasted flag, and the wasted count in unary.
    let header = 8 + wasted as usize;
    let mut best = SubframePlan {
        kind: SubKind::Verbatim,
        wasted,
        bps: ebps,
        samples,
        residual: Vec::new(),
        rice: None,
        bits: header + n * ebps as usize,
    };
    let mut scratch = Scratch::default();

    // Fixed predictors. `Fast` estimates the order from the residual sums
    // (the first of the smallest); the others price every order exactly.
    let orders = 0..=4.min(n.saturating_sub(1));
    if level == Level::Fast {
        let mut pick: Option<(u64, usize)> = None;
        for order in orders {
            fixed_residual(&best.samples, order, &mut scratch.residual);
            let sum: u64 = scratch.residual.iter().map(|v| v.unsigned_abs()).sum();
            if pick.is_none_or(|(s, _)| sum < s) {
                pick = Some((sum, order));
            }
        }
        if let Some((_, order)) = pick {
            fixed_residual(&best.samples, order, &mut scratch.residual);
            consider(&mut best, SubKind::Fixed(order), &mut scratch, header + order * ebps as usize, level);
        }
    } else {
        for order in orders {
            fixed_residual(&best.samples, order, &mut scratch.residual);
            consider(&mut best, SubKind::Fixed(order), &mut scratch, header + order * ebps as usize, level);
        }
    }

    // LPC.
    let max_order = level.max_lpc_order().min(n.saturating_sub(1));
    if max_order > 0 {
        let r = lpc::autocorrelation(&best.samples, window, max_order);
        let (coefs, errors) = lpc::levinson(&r, max_order);
        let precision: u32 = if ebps <= 16 { 13 } else { 15 };
        let orders: Vec<usize> = if level == Level::Best {
            (1..=coefs.len()).collect()
        } else {
            // The order whose estimated size — residual entropy from the
            // Levinson error, plus the coefficients — is least.
            let est = |k: usize| -> f64 {
                let e = (errors[k - 1] / n as f64).max(1e-9);
                n as f64 * (0.5 * e.log2()).max(0.0) + (k as f64) * f64::from(precision + ebps)
            };
            (1..=coefs.len()).min_by(|&a, &b| est(a).total_cmp(&est(b))).into_iter().collect()
        };
        // The samples as i32 when they all fit, for the vector residual.
        let narrow: Option<Vec<i32>> = best.samples.iter().map(|&v| i32::try_from(v).ok()).collect();
        for order in orders {
            let (q, shift) = lpc::quantize(&coefs[order - 1], precision, 15);
            if !lpc_residual(&best.samples, narrow.as_deref(), &q, shift, &mut scratch.residual) {
                continue;
            }
            let head = header + order * ebps as usize + 4 + 5 + order * precision as usize;
            consider(&mut best, SubKind::Lpc { coefs: q, precision, shift }, &mut scratch, head, level);
        }
    }
    best
}

/// Replace `best` with the predicted form when its residual (in
/// `scratch.residual`) codes smaller.
fn consider(best: &mut SubframePlan, kind: SubKind, scratch: &mut Scratch, head_bits: usize, level: Level) {
    // Residuals a decoder cannot hold in 32 bits are not an option; the
    // rest fold into 31 bits.
    if !fold(&scratch.residual, &mut scratch.folded) {
        return;
    }
    let n = best.samples.len();
    let rice = plan_rice(&scratch.folded, n, kind.order(), level.max_partition_order());
    let bits = head_bits + rice.bits;
    if bits < best.bits {
        best.kind = kind;
        best.residual.clear();
        best.residual.extend_from_slice(&scratch.residual);
        best.rice = Some(rice);
        best.bits = bits;
    }
}

/// The fixed predictor's residual of `order` (§9.2.5) into `out`.
fn fixed_residual(x: &[i64], order: usize, out: &mut Vec<i64>) {
    out.clear();
    if x.len() <= order {
        return;
    }
    // One straight-line loop per order, which vectorises.
    match order {
        0 => out.extend_from_slice(x),
        1 => out.extend(x.windows(2).map(|w| w[1] - w[0])),
        2 => out.extend(x.windows(3).map(|w| w[2] - 2 * w[1] + w[0])),
        3 => out.extend(x.windows(4).map(|w| w[3] - 3 * w[2] + 3 * w[1] - w[0])),
        _ => out.extend(x.windows(5).map(|w| w[4] - 4 * w[3] + 6 * w[2] - 4 * w[1] + w[0])),
    }
}

/// The LPC residual into `out`; `false` when the prediction overflows 64
/// bits (only possible for samples beyond 32 bits). `narrow` is `x` as
/// i32 when it fits, which takes the vector kernel: 15-bit coefficients
/// times 32-bit samples, 32 of them, stay well inside 64 bits, so its
/// result is the checked loop's.
fn lpc_residual(x: &[i64], narrow: Option<&[i32]>, coefs: &[i32], shift: i32, out: &mut Vec<i64>) -> bool {
    let order = coefs.len();
    out.clear();
    if x.len() <= order {
        return true;
    }
    if let Some(x32) = narrow
        && order <= 32
        && coefs.iter().all(|c| c.unsigned_abs() < 1 << 15)
    {
        out.resize(x.len() - order, 0);
        lpc_residual_narrow(x32, coefs, shift as u32, out);
        return true;
    }
    for i in order..x.len() {
        let mut acc: i64 = 0;
        for (j, &c) in coefs.iter().enumerate() {
            let Some(next) = i64::from(c).checked_mul(x[i - 1 - j]).and_then(|p| acc.checked_add(p)) else {
                return false;
            };
            acc = next;
        }
        out.push(x[i] - (acc >> shift));
    }
    true
}

crate::simd::multiversion! {
/// [`lpc_residual`]'s vector kernel: `out[i - order] = x[i] - (Σ c[j] ·
/// x[i - 1 - j]) >> shift`, the order a compile-time constant so the sum
/// unrolls and the loop over `i` vectorises (32 × 32 → 64-bit multiplies).
fn lpc_residual_narrow(x: &[i32], coefs: &[i32], shift: u32, out: &mut [i64]) {
    macro_rules! orders {
        ($($n:literal)*) => {
            match coefs.len() {
                $($n => residual_n::<$n>(x, coefs, shift, out),)*
                _ => unreachable!("LPC orders are 1 to 32"),
            }
        };
    }
    orders!(1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31 32)
}
}

#[inline(always)]
fn residual_n<const N: usize>(x: &[i32], coefs: &[i32], shift: u32, out: &mut [i64]) {
    let c: [i64; N] = std::array::from_fn(|j| i64::from(coefs[j]));
    for (o, w) in out.iter_mut().zip(x.windows(N + 1)) {
        let mut acc = 0i64;
        for j in 0..N {
            acc += c[j] * i64::from(w[N - 1 - j]);
        }
        *o = i64::from(w[N]) - (acc >> shift);
    }
}

crate::simd::multiversion! {
/// The residual folded to unsigned (zigzag: 0, -1, 1, -2, … → 0, 1, 2, 3,
/// …) into `out`; `false` when some value is 2^30 or more in magnitude
/// (the decoder's 32-bit limit with room for the fold).
fn fold(residual: &[i64], out: &mut Vec<u32>) -> bool {
    out.clear();
    out.resize(residual.len(), 0);
    let mut wide = 0u64;
    for (o, &r) in out.iter_mut().zip(residual) {
        wide |= r.unsigned_abs();
        *o = ((r << 1) ^ (r >> 63)) as u32;
    }
    wide < 1 << 30
}
}

/// Per partition of the finest order: the sum of the folded values and
/// their OR (whose top bit gives the escape's width).
struct PartitionStats {
    sum: u64,
    or: u32,
    count: usize,
}

crate::simd::multiversion! {
/// Sum and OR of each `per`-value partition of `u`, the first `order`
/// values shorter.
fn partition_stats(u: &[u32], parts: usize, per: usize, order: usize) -> Vec<PartitionStats> {
    let mut at = 0usize;
    (0..parts)
        .map(|p| {
            let count = per - if p == 0 { order } else { 0 };
            let part = &u[at..at + count];
            at += count;
            let sum = part.iter().map(|&v| u64::from(v)).sum();
            let or = part.iter().fold(0, |a, &v| a | v);
            PartitionStats { sum, or, count }
        })
        .collect()
}
}

crate::simd::multiversion! {
/// `Σ v >> k` over a partition: the Rice code's quotient bits.
fn quotient_bits(part: &[u32], k: u32) -> u64 {
    part.iter().map(|&v| u64::from(v >> k)).sum()
}
}

/// Choose the partition order and per-partition parameters for the folded
/// residual `u` of a block of `n` samples with predictor order `order`.
fn plan_rice(u: &[u32], n: usize, order: usize, max_order: u32) -> RicePlan {
    // Finest partitioning allowed: the block divides evenly and the first
    // partition still holds more than the warm-up.
    let mut top = 0u32;
    while top < max_order.min(15) && n.is_multiple_of(1 << (top + 1)) && (n >> (top + 1)) > order {
        top += 1;
    }
    // Sums per partition at the finest order, merged pairwise going up;
    // each order is priced from its sums alone.
    let parts = 1usize << top;
    let per = n >> top;
    let finest = partition_stats(u, parts, per, order);
    let mut merged: Vec<(u64, usize)> = finest.iter().map(|s| (s.sum, s.count)).collect();
    let mut best: Option<(u32, usize)> = None;
    for level in (0..=top).rev() {
        let mut bits = 2 + 4;
        let mut wide = false;
        for &(sum, count) in &merged {
            let k = best_k(sum, count);
            wide |= k > 14;
            bits += count * (k as usize + 1) + (sum >> k) as usize;
        }
        bits += merged.len() * if wide { 5 } else { 4 };
        if best.is_none_or(|(_, b)| bits < b) {
            best = Some((level, bits));
        }
        for i in 0..merged.len() / 2 {
            merged[i] = (merged[2 * i].0 + merged[2 * i + 1].0, merged[2 * i].1 + merged[2 * i + 1].1);
        }
        merged.truncate(merged.len() / 2);
    }
    // The chosen order's partitions, again from the finest.
    let (level, bits) = best.expect("partition order 0 always exists");
    let group = 1usize << (top - level);
    let mut params = Vec::with_capacity(1 << level);
    let mut ors = Vec::with_capacity(1 << level);
    let mut wide = false;
    for g in finest.chunks(group) {
        let (sum, count) = g.iter().fold((0u64, 0usize), |(s, c), p| (s + p.sum, c + p.count));
        let k = best_k(sum, count);
        wide |= k > 14;
        params.push(Ok(k));
        ors.push(g.iter().fold(0, |o, p| o | p.or));
    }
    let mut plan = RicePlan { order: level, params, wide, bits, ors };
    exact_rice(&mut plan, u, n, order);
    plan
}

/// The Rice parameter minimising `count·(k+1) + sum>>k`.
fn best_k(sum: u64, count: usize) -> u32 {
    if count == 0 || sum == 0 {
        return 0;
    }
    let mean = sum / count as u64;
    let guess = if mean == 0 { 0 } else { 63 - mean.leading_zeros() };
    let cost = |k: u32| count as u64 * u64::from(k + 1) + (sum >> k);
    let mut k = guess.min(30);
    while k > 0 && cost(k - 1) <= cost(k) {
        k -= 1;
    }
    while k < 30 && cost(k + 1) < cost(k) {
        k += 1;
    }
    k
}

/// Re-price the chosen partitioning exactly, taking the raw-bits escape for
/// any partition where it is smaller.
fn exact_rice(plan: &mut RicePlan, u: &[u32], n: usize, order: usize) {
    let per = n >> plan.order;
    let mut at = 0usize;
    let mut bits = 2 + 4;
    for (p, param) in plan.params.iter_mut().enumerate() {
        let count = per - if p == 0 { order } else { 0 };
        let part = &u[at..at + count];
        at += count;
        let k = param.expect("planned as Rice");
        let rice = count * (1 + k as usize) + quotient_bits(part, k) as usize;
        // The escape: 5 bits of width, then every value in that many bits.
        // A value `r` takes the bits of `r ^ (r >> 63)` (the folded value
        // halved) plus a sign bit, and 0 takes none: the widest follows
        // from the OR of the partition's folded values.
        let or = plan.ors[p];
        let width = if or == 0 { 0 } else { 33 - (or >> 1).leading_zeros() };
        let escape = 5 + count * width as usize;
        let escape_code = if plan.wide { 31 } else { 15 };
        if escape < rice && width <= 31 || k >= escape_code {
            *param = Err(width);
            bits += escape;
        } else {
            bits += rice;
        }
    }
    bits += plan.params.len() * if plan.wide { 5 } else { 4 };
    plan.bits = bits;
}

fn zigzag(r: i64) -> u64 {
    ((r << 1) ^ (r >> 63)) as u64
}

fn write_subframe(bw: &mut BitWriter, s: &SubframePlan) {
    let kind = match &s.kind {
        SubKind::Constant => 0,
        SubKind::Verbatim => 1,
        SubKind::Fixed(o) => 8 + *o as u64,
        SubKind::Lpc { coefs, .. } => 32 + coefs.len() as u64 - 1,
    };
    bw.write(0, 1);
    bw.write(kind, 6);
    if s.wasted > 0 {
        bw.write(1, 1);
        bw.write_unary_zeros(s.wasted - 1);
    } else {
        bw.write(0, 1);
    }
    match &s.kind {
        SubKind::Constant => bw.write_signed(s.samples[0], s.bps),
        SubKind::Verbatim => {
            for &v in &s.samples {
                bw.write_signed(v, s.bps);
            }
        }
        SubKind::Fixed(order) => {
            for &v in &s.samples[..*order] {
                bw.write_signed(v, s.bps);
            }
            write_residual(bw, s);
        }
        SubKind::Lpc { coefs, precision, shift } => {
            for &v in &s.samples[..coefs.len()] {
                bw.write_signed(v, s.bps);
            }
            bw.write(u64::from(precision - 1), 4);
            bw.write_signed(i64::from(*shift), 5);
            for &c in coefs {
                bw.write_signed(i64::from(c), *precision);
            }
            write_residual(bw, s);
        }
    }
}

fn write_residual(bw: &mut BitWriter, s: &SubframePlan) {
    let rice = s.rice.as_ref().expect("a predicted subframe has a Rice plan");
    let (param_bits, escape) = if rice.wide { (5, 31) } else { (4, 15) };
    bw.write(u64::from(rice.wide), 2);
    bw.write(u64::from(rice.order), 4);
    let n = s.samples.len();
    let order = s.kind.order();
    let per = n >> rice.order;
    let mut at = 0usize;
    for (p, param) in rice.params.iter().enumerate() {
        let count = per - if p == 0 { order } else { 0 };
        let part = &s.residual[at..at + count];
        at += count;
        match *param {
            Ok(k) => {
                bw.write(u64::from(k), param_bits);
                for &r in part {
                    let u = zigzag(r);
                    bw.write_rice((u >> k) as u32, u, k);
                }
            }
            Err(width) => {
                bw.write(escape, param_bits);
                bw.write(u64::from(width), 5);
                for &r in part {
                    bw.write_signed(r, width);
                }
            }
        }
    }
}
