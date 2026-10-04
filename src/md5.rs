//! MD5 (RFC 1321), for FLAC's STREAMINFO signature of the audio.
//!
//! Written from the RFC: the four rounds of sixteen steps, the sine-derived
//! constants `T[i] = floor(|sin(i + 1)| · 2^32)`, little-endian words and the
//! bit-length padding. The round functions are written in the forms that
//! keep the dependency on the newest state word short (F as
//! `d ^ (b & (c ^ d))`; G with its two disjoint halves added separately, so
//! `c & !d` is ready before `b` is), which is what bounds a one-stream
//! hash's speed. No `unsafe`.

/// `T[i] = floor(|sin(i + 1)| · 2^32)` (RFC 1321 §3.4).
const T: [u32; 64] = [
    0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501, 0x698098d8,
    0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340,
    0x265e5a51, 0xe9b6c7aa, 0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87,
    0x455a14ed, 0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c,
    0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05, 0xd9d4d039,
    0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92,
    0xffeff47d, 0x85845dd1, 0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
    0xeb86d391,
];

/// A running MD5.
#[derive(Clone)]
pub(crate) struct Md5 {
    state: [u32; 4],
    /// Bytes hashed so far.
    len: u64,
    /// A partial block waiting for more input.
    buf: [u8; 64],
    buffered: usize,
}

impl Default for Md5 {
    fn default() -> Self {
        Self::new()
    }
}

macro_rules! step {
    ($f:ident, $a:ident, $b:ident, $c:ident, $d:ident, $m:expr, $t:expr, $s:expr) => {
        $a = $b.wrapping_add($f!($a, $b, $c, $d, $m.wrapping_add($t)).rotate_left($s));
    };
}
// Each takes `a` and the message word plus constant, and returns
// `a + fn(b, c, d) + m + t`, in an order that leaves `b` for last.
macro_rules! ff {
    ($a:ident, $b:ident, $c:ident, $d:ident, $mt:expr) => {
        $a.wrapping_add($mt).wrapping_add($d ^ ($b & ($c ^ $d)))
    };
}
macro_rules! gg {
    ($a:ident, $b:ident, $c:ident, $d:ident, $mt:expr) => {
        $a.wrapping_add($mt).wrapping_add($c & !$d).wrapping_add($b & $d)
    };
}
macro_rules! hh {
    ($a:ident, $b:ident, $c:ident, $d:ident, $mt:expr) => {
        $a.wrapping_add($mt).wrapping_add($b ^ $c ^ $d)
    };
}
macro_rules! ii {
    ($a:ident, $b:ident, $c:ident, $d:ident, $mt:expr) => {
        $a.wrapping_add($mt).wrapping_add($c ^ ($b | !$d))
    };
}

fn compress(state: &mut [u32; 4], block: &[u8; 64]) {
    let mut m = [0u32; 16];
    for (w, b) in m.iter_mut().zip(block.as_chunks::<4>().0) {
        *w = u32::from_le_bytes(*b);
    }
    let [mut a, mut b, mut c, mut d] = *state;
    // Round 1: word i, shifts 7 12 17 22.
    step!(ff, a, b, c, d, m[0], T[0], 7);
    step!(ff, d, a, b, c, m[1], T[1], 12);
    step!(ff, c, d, a, b, m[2], T[2], 17);
    step!(ff, b, c, d, a, m[3], T[3], 22);
    step!(ff, a, b, c, d, m[4], T[4], 7);
    step!(ff, d, a, b, c, m[5], T[5], 12);
    step!(ff, c, d, a, b, m[6], T[6], 17);
    step!(ff, b, c, d, a, m[7], T[7], 22);
    step!(ff, a, b, c, d, m[8], T[8], 7);
    step!(ff, d, a, b, c, m[9], T[9], 12);
    step!(ff, c, d, a, b, m[10], T[10], 17);
    step!(ff, b, c, d, a, m[11], T[11], 22);
    step!(ff, a, b, c, d, m[12], T[12], 7);
    step!(ff, d, a, b, c, m[13], T[13], 12);
    step!(ff, c, d, a, b, m[14], T[14], 17);
    step!(ff, b, c, d, a, m[15], T[15], 22);
    // Round 2: word (5i + 1) mod 16, shifts 5 9 14 20.
    step!(gg, a, b, c, d, m[1], T[16], 5);
    step!(gg, d, a, b, c, m[6], T[17], 9);
    step!(gg, c, d, a, b, m[11], T[18], 14);
    step!(gg, b, c, d, a, m[0], T[19], 20);
    step!(gg, a, b, c, d, m[5], T[20], 5);
    step!(gg, d, a, b, c, m[10], T[21], 9);
    step!(gg, c, d, a, b, m[15], T[22], 14);
    step!(gg, b, c, d, a, m[4], T[23], 20);
    step!(gg, a, b, c, d, m[9], T[24], 5);
    step!(gg, d, a, b, c, m[14], T[25], 9);
    step!(gg, c, d, a, b, m[3], T[26], 14);
    step!(gg, b, c, d, a, m[8], T[27], 20);
    step!(gg, a, b, c, d, m[13], T[28], 5);
    step!(gg, d, a, b, c, m[2], T[29], 9);
    step!(gg, c, d, a, b, m[7], T[30], 14);
    step!(gg, b, c, d, a, m[12], T[31], 20);
    // Round 3: word (3i + 5) mod 16, shifts 4 11 16 23.
    step!(hh, a, b, c, d, m[5], T[32], 4);
    step!(hh, d, a, b, c, m[8], T[33], 11);
    step!(hh, c, d, a, b, m[11], T[34], 16);
    step!(hh, b, c, d, a, m[14], T[35], 23);
    step!(hh, a, b, c, d, m[1], T[36], 4);
    step!(hh, d, a, b, c, m[4], T[37], 11);
    step!(hh, c, d, a, b, m[7], T[38], 16);
    step!(hh, b, c, d, a, m[10], T[39], 23);
    step!(hh, a, b, c, d, m[13], T[40], 4);
    step!(hh, d, a, b, c, m[0], T[41], 11);
    step!(hh, c, d, a, b, m[3], T[42], 16);
    step!(hh, b, c, d, a, m[6], T[43], 23);
    step!(hh, a, b, c, d, m[9], T[44], 4);
    step!(hh, d, a, b, c, m[12], T[45], 11);
    step!(hh, c, d, a, b, m[15], T[46], 16);
    step!(hh, b, c, d, a, m[2], T[47], 23);
    // Round 4: word 7i mod 16, shifts 6 10 15 21.
    step!(ii, a, b, c, d, m[0], T[48], 6);
    step!(ii, d, a, b, c, m[7], T[49], 10);
    step!(ii, c, d, a, b, m[14], T[50], 15);
    step!(ii, b, c, d, a, m[5], T[51], 21);
    step!(ii, a, b, c, d, m[12], T[52], 6);
    step!(ii, d, a, b, c, m[3], T[53], 10);
    step!(ii, c, d, a, b, m[10], T[54], 15);
    step!(ii, b, c, d, a, m[1], T[55], 21);
    step!(ii, a, b, c, d, m[8], T[56], 6);
    step!(ii, d, a, b, c, m[15], T[57], 10);
    step!(ii, c, d, a, b, m[6], T[58], 15);
    step!(ii, b, c, d, a, m[13], T[59], 21);
    step!(ii, a, b, c, d, m[4], T[60], 6);
    step!(ii, d, a, b, c, m[11], T[61], 10);
    step!(ii, c, d, a, b, m[2], T[62], 15);
    step!(ii, b, c, d, a, m[9], T[63], 21);
    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
}

impl Md5 {
    pub fn new() -> Self {
        Self { state: [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476], len: 0, buf: [0; 64], buffered: 0 }
    }

    pub fn consume(&mut self, mut data: &[u8]) {
        self.len = self.len.wrapping_add(data.len() as u64);
        if self.buffered > 0 {
            let take = (64 - self.buffered).min(data.len());
            self.buf[self.buffered..self.buffered + take].copy_from_slice(&data[..take]);
            self.buffered += take;
            data = &data[take..];
            if self.buffered < 64 {
                return;
            }
            let block = self.buf;
            compress(&mut self.state, &block);
            self.buffered = 0;
        }
        let (blocks, rest) = data.as_chunks::<64>();
        for block in blocks {
            compress(&mut self.state, block);
        }
        self.buf[..rest.len()].copy_from_slice(rest);
        self.buffered = rest.len();
    }

    /// The digest of everything consumed (the running hash is left as is).
    pub fn compute(&self) -> [u8; 16] {
        let mut h = self.clone();
        let bits = self.len.wrapping_mul(8);
        // A 1 bit, zeros to 56 mod 64 bytes, then the bit length (LE).
        let pad = if self.buffered < 56 { 56 - self.buffered } else { 120 - self.buffered };
        let mut tail = [0u8; 72];
        tail[0] = 0x80;
        h.consume(&tail[..pad]);
        h.consume(&bits.to_le_bytes());
        debug_assert_eq!(h.buffered, 0);
        let mut out = [0u8; 16];
        for (o, w) in out.as_chunks_mut::<4>().0.iter_mut().zip(h.state) {
            o.copy_from_slice(&w.to_le_bytes());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(d: [u8; 16]) -> String {
        d.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn rfc_1321_test_suite() {
        // RFC 1321 appendix A.5.
        let cases = [
            ("", "d41d8cd98f00b204e9800998ecf8427e"),
            ("a", "0cc175b9c0f1b6a831c399e269772661"),
            ("abc", "900150983cd24fb0d6963f7d28e17f72"),
            ("message digest", "f96b697d7cb7938d525a2f31aaf161d0"),
            ("abcdefghijklmnopqrstuvwxyz", "c3fcd3d76192e4007dfb496cca67e13b"),
            ("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789", "d174ab98d277d9f5a5611c2c9f419d9f"),
            (
                "12345678901234567890123456789012345678901234567890123456789012345678901234567890",
                "57edf4a22be3c955ac49da2e2107b67a",
            ),
        ];
        for (input, want) in cases {
            let mut h = Md5::new();
            h.consume(input.as_bytes());
            assert_eq!(hex(h.compute()), want, "{input:?}");
        }
    }

    #[test]
    fn split_input_hashes_the_same() {
        let data: Vec<u8> = (0..1000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
        let mut whole = Md5::new();
        whole.consume(&data);
        for split in [0, 1, 55, 56, 63, 64, 65, 127, 128, 999] {
            let mut h = Md5::new();
            h.consume(&data[..split]);
            h.consume(&data[split..]);
            assert_eq!(h.compute(), whole.compute(), "split at {split}");
        }
    }
}
