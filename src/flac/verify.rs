//! The STREAMINFO MD5 check, off the decoding thread.
//!
//! MD5 is one serial chain over the whole stream's audio, and on 24-bit
//! audio it costs about a third of the decode. The frames themselves are
//! decoded in order, so the hash need not hold the decoder up: each decoded
//! frame's samples are handed to one helper thread, which turns them into
//! the signature's little-endian bytes and hashes them in the order they
//! came. Asking for the digest waits until every frame handed over so far
//! is hashed, so the result is the hash of exactly the audio decoded — the
//! same check as hashing inline, frame for frame.
//!
//! When no thread can be started, the frames are hashed inline instead.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

use super::format::md5_bytes;
use crate::md5::Md5;

/// Frames that may wait for the hash thread before the decoder waits for
/// it: enough to ride out scheduling jitter, few enough that the memory
/// they hold stays small (at most 64 frames of 64 Ki samples × 8 channels).
const QUEUE: usize = 64;

enum Msg {
    /// A decoded frame's interleaved samples, at this bit depth.
    Frame(Vec<i32>, u32),
    /// Reply with the digest of every frame sent before this.
    Digest(SyncSender<[u8; 16]>),
}

/// A running MD5 of decoded audio, frame by frame in decode order.
pub(super) struct Md5Verifier {
    mode: Mode,
}

enum Mode {
    /// Nothing hashed yet. The thread starts with the first frame, so a
    /// decoder that never decodes starts none.
    Idle,
    Thread {
        tx: SyncSender<Msg>,
        abandoned: Arc<AtomicBool>,
    },
    Inline {
        ctx: Md5,
        scratch: Vec<u8>,
    },
}

impl Md5Verifier {
    pub(super) fn new() -> Self {
        Self { mode: Mode::Idle }
    }

    /// The same as [`Self::new`], hashing on the calling thread.
    #[cfg(test)]
    pub(super) fn inline() -> Self {
        Self {
            mode: Mode::Inline {
                ctx: Md5::new(),
                scratch: Vec::new(),
            },
        }
    }

    /// Hash `samples` (interleaved, `bits` per sample) after every frame
    /// pushed before.
    pub(super) fn push(&mut self, samples: Vec<i32>, bits: u32) {
        if let Mode::Idle = self.mode {
            self.mode = spawn().unwrap_or(Mode::Inline {
                ctx: Md5::new(),
                scratch: Vec::new(),
            });
        }
        match &mut self.mode {
            Mode::Thread { tx, .. } => {
                // The thread only returns once every sender is gone, and
                // this one is still here.
                tx.send(Msg::Frame(samples, bits))
                    .expect("the FLAC MD5 thread stopped");
            }
            Mode::Inline { ctx, scratch } => {
                scratch.clear();
                md5_bytes(&samples, bits, scratch);
                ctx.consume(scratch);
            }
            Mode::Idle => unreachable!("started above"),
        }
    }

    /// The MD5 of every frame pushed so far.
    pub(super) fn digest(&self) -> [u8; 16] {
        match &self.mode {
            Mode::Idle => Md5::new().compute(),
            Mode::Inline { ctx, .. } => ctx.compute(),
            Mode::Thread { tx, .. } => {
                let (reply, answer) = sync_channel(1);
                tx.send(Msg::Digest(reply))
                    .expect("the FLAC MD5 thread stopped");
                answer.recv().expect("the FLAC MD5 thread stopped")
            }
        }
    }
}

impl Drop for Md5Verifier {
    fn drop(&mut self) {
        // The thread ends once the sender is gone; the frames it still
        // holds need not be hashed.
        if let Mode::Thread { abandoned, .. } = &self.mode {
            abandoned.store(true, Ordering::Relaxed);
        }
    }
}

fn spawn() -> Option<Mode> {
    let (tx, rx) = sync_channel(QUEUE);
    let abandoned = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&abandoned);
    std::thread::Builder::new()
        .name("flac-md5".into())
        .spawn(move || run(&rx, &flag))
        .ok()?;
    Some(Mode::Thread { tx, abandoned })
}

fn run(rx: &Receiver<Msg>, abandoned: &AtomicBool) {
    let mut ctx = Md5::new();
    let mut scratch = Vec::new();
    while let Ok(msg) = rx.recv() {
        match msg {
            Msg::Frame(samples, bits) => {
                if abandoned.load(Ordering::Relaxed) {
                    continue;
                }
                scratch.clear();
                md5_bytes(&samples, bits, &mut scratch);
                ctx.consume(&scratch);
            }
            Msg::Digest(reply) => {
                // The asker may be gone (its decoder dropped): nothing to do then.
                let _ = reply.send(ctx.compute());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames() -> Vec<(Vec<i32>, u32)> {
        let mut seed = 0x9e37_79b9u32;
        (0..200)
            .map(|i| {
                let bits = [8u32, 16, 20, 24, 32][i % 5];
                let n = 1 + (i * 37) % 4096;
                let v = (0..n)
                    .map(|_| {
                        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                        (seed as i32) >> (32 - bits)
                    })
                    .collect();
                (v, bits)
            })
            .collect()
    }

    #[test]
    fn the_thread_hashes_what_inline_hashing_does_at_every_point() {
        let mut threaded = Md5Verifier::new();
        let mut inline = Md5Verifier::inline();
        assert_eq!(threaded.digest(), inline.digest(), "nothing hashed");
        for (i, (v, bits)) in frames().into_iter().enumerate() {
            threaded.push(v.clone(), bits);
            inline.push(v, bits);
            if i % 17 == 0 {
                assert_eq!(threaded.digest(), inline.digest(), "after frame {i}");
            }
        }
        assert_eq!(threaded.digest(), inline.digest());
        assert!(
            matches!(threaded.mode, Mode::Thread { .. }),
            "the hash ran on its thread"
        );
    }

    #[test]
    fn a_known_digest() {
        // RFC 1321's "abc", as three 8-bit samples.
        let mut v = Md5Verifier::new();
        v.push(vec![i32::from(b'a'), i32::from(b'b'), i32::from(b'c')], 8);
        assert_eq!(
            v.digest(),
            [
                0x90, 0x01, 0x50, 0x98, 0x3c, 0xd2, 0x4f, 0xb0, 0xd6, 0x96, 0x3f, 0x7d, 0x28, 0xe1,
                0x7f, 0x72
            ]
        );
    }

    #[test]
    fn dropping_mid_stream_does_not_wait_or_panic() {
        let mut v = Md5Verifier::new();
        for (s, bits) in frames() {
            v.push(s, bits);
        }
        drop(v);
    }
}
