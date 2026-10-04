//! MSB-first bit reader and writer for the FLAC and ALAC bitstreams.
//!
//! Both formats pack fields most-significant bit first, both code residuals
//! with a unary prefix, and both run past 32 bits in places (a FLAC side
//! channel of 32-bit audio is 33 bits wide), so the reader hands out up to 64
//! bits and the writer takes as many.

use crate::Error;

pub(crate) struct BitReader<'a> {
    data: &'a [u8],
    /// Bits consumed from the start of `data`.
    pos: usize,
    /// Which codec is reading, for the error text.
    codec: &'static str,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8], codec: &'static str) -> Self {
        Self { data, pos: 0, codec }
    }

    /// Bits consumed so far.
    pub fn pos(&self) -> usize {
        self.pos
    }

    /// Bits left before the end of the data.
    pub fn remaining(&self) -> usize {
        self.data.len() * 8 - self.pos
    }

    fn overrun(&self, n: u32) -> Error {
        Error::Invalid(format!(
            "{}: read of {n} bits at bit {} runs past the end of a {}-byte packet",
            self.codec,
            self.pos,
            self.data.len()
        ))
    }

    /// The 64 bits from the read position on, MSB first, zero past the end
    /// of the data. At least 57 of them are the data's whenever that much
    /// is left: a whole 8-byte load shifted by the bit offset.
    #[inline(always)]
    fn peek64(&self) -> u64 {
        let byte = self.pos >> 3;
        let off = (self.pos & 7) as u32;
        let word = match self.data.get(byte..byte + 8) {
            Some(b) => u64::from_be_bytes(b.try_into().expect("8 bytes")),
            None => {
                let mut b = [0u8; 8];
                let tail = self.data.get(byte..).unwrap_or(&[]);
                b[..tail.len()].copy_from_slice(tail);
                u64::from_be_bytes(b)
            }
        };
        word << off
    }

    /// Read `n` (≤ 64) bits as an unsigned value.
    #[inline]
    pub fn read(&mut self, n: u32) -> Result<u64, Error> {
        debug_assert!(n <= 64);
        if n == 0 {
            return Ok(0);
        }
        if n as usize > self.remaining() {
            return Err(self.overrun(n));
        }
        if n <= 56 {
            let v = self.peek64() >> (64 - n);
            self.pos += n as usize;
            return Ok(v);
        }
        let hi = self.peek64() >> 32;
        self.pos += 32;
        let lo = self.peek64() >> (96 - n);
        self.pos += n as usize - 32;
        Ok((hi << (n - 32)) | lo)
    }

    /// Read `n` (≤ 32) bits as an unsigned value.
    #[inline]
    pub fn read_u32(&mut self, n: u32) -> Result<u32, Error> {
        debug_assert!(n <= 32);
        Ok(self.read(n)? as u32)
    }

    #[inline]
    pub fn read_bit(&mut self) -> Result<bool, Error> {
        Ok(self.read(1)? == 1)
    }

    /// Read `n` (1..=64) bits as a two's-complement value.
    #[inline]
    pub fn read_signed(&mut self, n: u32) -> Result<i64, Error> {
        if n == 0 {
            return Ok(0);
        }
        let v = self.read(n)?;
        let shift = 64 - n;
        Ok(((v << shift) as i64) >> shift)
    }

    /// Count `0` bits up to the next `1`, and consume that `1` (FLAC's
    /// unary: the quotient of a Rice code, the wasted-bits count).
    #[inline]
    pub fn read_unary_zeros(&mut self) -> Result<u32, Error> {
        let mut n = 0u32;
        loop {
            let left = self.remaining();
            if left == 0 {
                return Err(self.overrun(1));
            }
            // Bits of `peek64` that are the data's.
            let valid = (64 - (self.pos & 7)).min(left) as u32;
            let z = self.peek64().leading_zeros();
            if z < valid {
                self.pos += z as usize + 1;
                return Ok(n + z);
            }
            n += valid;
            self.pos += valid as usize;
        }
    }

    /// A FLAC Rice code with parameter `k` (≤ 32): the unary quotient, then
    /// `k` low bits, as the folded (zigzag) value. One load when the code
    /// fits the 57 bits a load holds, which is all but the escapes of
    /// pathological streams.
    #[inline(always)]
    pub fn read_rice(&mut self, k: u32) -> Result<u64, Error> {
        debug_assert!(k <= 32);
        let left = self.remaining();
        let peek = self.peek64();
        let z = peek.leading_zeros();
        let len = z + 1 + k;
        if len <= 57 && (len as usize) <= left {
            // `peek << z << 1` drops the quotient and its stop bit; `k` may
            // be 0, so the low bits come by two shifts that never reach 64.
            let low = ((peek << z) << 1) >> 1 >> (63 - k);
            self.pos += len as usize;
            return Ok((u64::from(z) << k) | low);
        }
        let q = u64::from(self.read_unary_zeros()?);
        Ok((q << k) | self.read(k)?)
    }

    /// A run of FLAC Rice codes with parameter `k` (≤ 32), unfolded to
    /// signed residuals in `out`. The same as [`read_rice`](Self::read_rice)
    /// per value, with the position kept in a register and no end-of-data
    /// test while a whole 8-byte load still fits.
    pub fn read_rice_block(&mut self, k: u32, out: &mut [i64]) -> Result<(), Error> {
        debug_assert!(k <= 32);
        let data = self.data;
        // Positions whose 8-byte load lies wholly inside the data; such a
        // load holds at least 57 of the data's bits.
        let fast_end = data.len().saturating_sub(7) * 8;
        let mut pos = self.pos;
        // The bits from `pos` on, MSB first, of which `avail` are loaded;
        // a code is taken from the register while it fits, and the
        // register reloaded from `pos` when it does not, which keeps the
        // memory load off the chain from one code to the next.
        let mut cache = 0u64;
        let mut avail = 0u32;
        for o in out.iter_mut() {
            let mut z = cache.leading_zeros();
            if z + 1 + k > avail {
                if pos >= fast_end {
                    self.pos = pos;
                    let u = self.read_rice(k)?;
                    pos = self.pos;
                    avail = 0;
                    cache = 0;
                    *o = (u >> 1) as i64 ^ -((u & 1) as i64);
                    continue;
                }
                let byte = pos >> 3;
                cache = u64::from_be_bytes(data[byte..byte + 8].try_into().expect("8 bytes")) << (pos & 7);
                avail = 64 - (pos & 7) as u32;
                z = cache.leading_zeros();
                if z + 1 + k > avail {
                    // A quotient longer than a load: the general path.
                    self.pos = pos;
                    let u = self.read_rice(k)?;
                    pos = self.pos;
                    avail = 0;
                    cache = 0;
                    *o = (u >> 1) as i64 ^ -((u & 1) as i64);
                    continue;
                }
            }
            let len = z + 1 + k;
            let u = (u64::from(z) << k) | (((cache << z) << 1) >> 1 >> (63 - k));
            // `len` may be 64 (a 63-bit quotient in a full register).
            cache = if len < 64 { cache << len } else { 0 };
            avail -= len;
            pos += len as usize;
            *o = (u >> 1) as i64 ^ -((u & 1) as i64);
        }
        self.pos = pos;
        Ok(())
    }

    /// Count `1` bits up to the next `0` or until `limit` of them have been
    /// read, consuming the `0` when one ends the run (ALAC's unary prefix,
    /// which has an escape at `limit`).
    pub fn read_unary_ones(&mut self, limit: u32) -> Result<u32, Error> {
        debug_assert!(limit <= 56);
        let left = self.remaining();
        let ones = (!self.peek64()).leading_zeros().min(limit);
        if ones as usize >= left {
            // Ran into the end: only an exact `limit` run ending there is
            // whole; anything else wants a bit that is not there.
            if ones == limit && left == limit as usize {
                self.pos += left;
                return Ok(ones);
            }
            return Err(self.overrun(1));
        }
        // The run, and the `0` that ended it when it ended short of `limit`.
        self.pos += ones as usize + usize::from(ones < limit);
        Ok(ones)
    }

    /// Give back the last `n` bits read.
    pub fn unread(&mut self, n: usize) {
        debug_assert!(n <= self.pos);
        self.pos -= n;
    }

    pub fn skip(&mut self, n: usize) -> Result<(), Error> {
        if n > self.remaining() {
            return Err(self.overrun(n as u32));
        }
        self.pos += n;
        Ok(())
    }

    /// Skip to the next byte boundary.
    pub fn align(&mut self) {
        self.pos = self.pos.div_ceil(8) * 8;
    }

    /// The whole bytes consumed so far (after [`align`](Self::align)).
    pub fn byte_pos(&self) -> usize {
        self.pos / 8
    }
}

/// Accumulates bits MSB first into a byte vector.
#[derive(Default)]
pub(crate) struct BitWriter {
    bytes: Vec<u8>,
    /// Bits waiting to be stored, right-aligned in `acc`.
    acc: u64,
    /// How many bits of `acc` are live (always < 32 between calls, so a
    /// write of up to 32 bits always fits beside them).
    live: u32,
}

impl BitWriter {
    pub fn with_capacity(bytes: usize) -> Self {
        Self { bytes: Vec::with_capacity(bytes), acc: 0, live: 0 }
    }

    /// Write the low `n` (≤ 64) bits of `v`.
    #[inline]
    pub fn write(&mut self, v: u64, n: u32) {
        debug_assert!(n <= 64);
        if n > 32 {
            self.write(v >> 32, n - 32);
            self.write(v & 0xFFFF_FFFF, 32);
            return;
        }
        if n == 0 {
            return;
        }
        let v = v & ((1u64 << n) - 1);
        self.acc = (self.acc << n) | v;
        self.live += n;
        if self.live >= 32 {
            // Four whole bytes at once.
            self.live -= 32;
            self.bytes.extend_from_slice(&((self.acc >> self.live) as u32).to_be_bytes());
            self.acc &= (1u64 << self.live) - 1;
        }
    }

    pub fn write_bit(&mut self, b: bool) {
        self.write(u64::from(b), 1);
    }

    /// Write `v` as an `n`-bit two's-complement field.
    #[inline]
    pub fn write_signed(&mut self, v: i64, n: u32) {
        self.write(v as u64, n);
    }

    /// `n` zeros then a `1` (FLAC's unary).
    #[inline]
    pub fn write_unary_zeros(&mut self, mut n: u32) {
        while n >= 32 {
            self.write(0, 32);
            n -= 32;
        }
        self.write(1, n + 1);
    }

    /// A FLAC Rice code: the quotient `q` in unary, then the low `k` (≤ 31)
    /// bits of `low`; one write when the whole code fits in 32 bits.
    #[inline]
    pub fn write_rice(&mut self, q: u32, low: u64, k: u32) {
        debug_assert!(k <= 31);
        if q + 1 + k <= 32 {
            self.write((1u64 << k) | (low & ((1u64 << k) - 1)), q + 1 + k);
        } else {
            self.write_unary_zeros(q);
            self.write(low, k);
        }
    }

    /// Bits written so far.
    pub fn len_bits(&self) -> usize {
        self.bytes.len() * 8 + self.live as usize
    }

    /// Pad with zeros to a byte boundary.
    pub fn align(&mut self) {
        if !self.live.is_multiple_of(8) {
            self.write(0, 8 - self.live % 8);
        }
    }

    /// The bytes so far; only whole bytes (call [`align`](Self::align) first).
    pub fn bytes(&mut self) -> &[u8] {
        debug_assert!(self.live.is_multiple_of(8));
        while self.live >= 8 {
            self.live -= 8;
            self.bytes.push((self.acc >> self.live) as u8);
        }
        self.acc = 0;
        &self.bytes
    }

    pub fn into_bytes(mut self) -> Vec<u8> {
        self.align();
        self.bytes();
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_width() {
        let mut w = BitWriter::default();
        let fields: Vec<(u64, u32)> = (1..=64).map(|n| (0xA5A5_5A5A_F00F_0FF0u64.rotate_left(n), n)).collect();
        for &(v, n) in &fields {
            w.write(v, n);
        }
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes, "test");
        for &(v, n) in &fields {
            let mask = if n == 64 { u64::MAX } else { (1u64 << n) - 1 };
            assert_eq!(r.read(n).unwrap(), v & mask, "{n} bits");
        }
    }

    #[test]
    fn signed_and_unary() {
        let mut w = BitWriter::default();
        w.write_signed(-5, 7);
        w.write_unary_zeros(0);
        w.write_unary_zeros(13);
        w.write_unary_zeros(70);
        w.write(0b1110, 4);
        w.write_signed(-(1i64 << 32), 33);
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes, "test");
        assert_eq!(r.read_signed(7).unwrap(), -5);
        assert_eq!(r.read_unary_zeros().unwrap(), 0);
        assert_eq!(r.read_unary_zeros().unwrap(), 13);
        assert_eq!(r.read_unary_zeros().unwrap(), 70);
        assert_eq!(r.read_unary_ones(9).unwrap(), 3);
        assert_eq!(r.read_signed(33).unwrap(), -(1i64 << 32));
    }

    #[test]
    fn rice_codes_read_alike_one_by_one_and_in_blocks() {
        let mut seed = 99u32;
        for k in [0u32, 1, 4, 13, 20, 31, 32] {
            let mut w = BitWriter::default();
            let mut want = Vec::new();
            for i in 0..2000 {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                // Mostly small quotients, some long ones that need the slow path.
                let q = if i % 97 == 0 { u64::from(seed >> 26) + 40 } else { u64::from(seed >> 29) };
                let low = if k == 0 { 0 } else { u64::from(seed) & ((1u64 << k) - 1) };
                let u = (q << k) | low;
                w.write_unary_zeros(q as u32);
                w.write(low, k);
                want.push((u >> 1) as i64 ^ -((u & 1) as i64));
            }
            let bytes = w.into_bytes();
            let mut a = BitReader::new(&bytes, "test");
            let mut got = vec![0i64; want.len()];
            a.read_rice_block(k, &mut got).unwrap();
            assert_eq!(got, want, "k {k}");
            let mut b = BitReader::new(&bytes, "test");
            for &v in &want {
                let u = b.read_rice(k).unwrap();
                assert_eq!((u >> 1) as i64 ^ -((u & 1) as i64), v);
            }
            assert_eq!(a.pos(), b.pos());
            // One value more runs off the end.
            let mut one = [0i64; 1];
            assert!(a.read_rice_block(k, &mut one).is_err() || a.remaining() < 8);
        }
    }

    #[test]
    fn overrun_is_an_error() {
        let mut r = BitReader::new(&[0x00], "test");
        assert!(r.read_unary_zeros().is_err());
        let mut r = BitReader::new(&[0xFF], "test");
        assert!(r.read(9).is_err());
    }
}
