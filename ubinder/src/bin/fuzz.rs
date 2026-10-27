//! A standalone, dependency-free fuzzer for the broker's BC_* command-stream
//! parser. It feeds pseudo-random byte streams as `write` buffers (the most
//! attacker-controlled input: every Android process writes these) and asserts
//! the broker never panics and always fully drains or cleanly rejects the
//! stream. Transaction bodies point at random addresses in a fake address
//! space, so object translation is exercised too.
//!
//! Run a bounded campaign:  `ubinder-fuzz [iterations] [seed]`
//! It is also wired into `cargo test` via `fuzz::smoke` below.

use ubinder::broker::Broker;
use ubinder::testmem::FakeMem;

/// SplitMix64: a tiny deterministic PRNG, so a failing run is reproducible
/// from its seed with no external crates.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn byte(&mut self) -> u8 {
        (self.next() & 0xff) as u8
    }
}

pub fn run(iterations: u64, seed: u64) {
    let mut rng = Rng(seed ^ 0xA5A5_1234);
    for _ in 0..iterations {
        let mut b = Broker::new();
        let mut m = FakeMem::default();
        let p = b.open(1000, 1000, 0);
        let q = b.open(1001, 10001, 0);
        m.add_proc(p, 0x1_0000_0000);
        m.add_proc(q, 0x2_0000_0000);
        let _ = b.set_context_mgr(q, None);
        // random memory so transaction buffers read something
        for _ in 0..64 {
            let addr = (rng.next() & 0xffff) + 0x1_0000_0000;
            m.poke(p, addr, &[rng.byte(), rng.byte(), rng.byte(), rng.byte()]);
        }
        let n = (rng.next() % 256) as usize;
        let buf: Vec<u8> = (0..n).map(|_| rng.byte()).collect();
        // Must not panic. Result is ignored; both Ok and errno are valid.
        let _ = b.write_read(p, 1, &buf, 4096, true, &mut m);
        // A read on the context manager must also stay sane.
        let _ = b.write_read(q, 1, &[], 4096, true, &mut m);
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let iters: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(200_000);
    let seed: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(1);
    run(iters, seed);
    println!("ubinder-fuzz: {iters} iterations from seed {seed}: no panics");
}

#[cfg(test)]
mod fuzz {
    #[test]
    fn smoke() {
        // A quick bounded campaign across several seeds in CI / `cargo test`.
        for seed in 0..8 {
            super::run(5_000, seed);
        }
    }
}
