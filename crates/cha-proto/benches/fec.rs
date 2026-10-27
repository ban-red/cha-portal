//! FEC encode and recovery speed at the sizes the streamer sends:
//!
//!     cargo bench -p cha-proto --bench fec
//!
//! Shards are 1,180 bytes (a 1,200-byte datagram less its header and the
//! parity's length). A 1440p frame on a WAN link is ~41 KB, 35 shards; a
//! keyframe fills a block of 128. Each line is the median of 25 samples.

use std::hint::black_box;
use std::time::{Duration, Instant};

use cha_proto::fec;

const SHARD: usize = 1180;

fn shards(k: usize, seed: u32) -> Vec<Vec<u8>> {
    let mut x = seed.wrapping_mul(2_654_435_761) | 1;
    (0..k)
        .map(|_| {
            (0..SHARD)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 17;
                    x ^= x << 5;
                    x as u8
                })
                .collect()
        })
        .collect()
}

/// Times `f`, reporting the median per call and the frame's data bytes
/// (`bytes`) per second at that rate.
fn bench(name: &str, bytes: usize, mut f: impl FnMut()) {
    let start = Instant::now();
    let mut calls = 0u32;
    while start.elapsed() < Duration::from_millis(100) {
        f();
        calls += 1;
    }
    // Samples of about 20 ms each.
    let per_sample = (calls / 5).max(1);
    let mut samples: Vec<f64> = (0..25)
        .map(|_| {
            let t = Instant::now();
            for _ in 0..per_sample {
                f();
            }
            t.elapsed().as_secs_f64() / f64::from(per_sample)
        })
        .collect();
    samples.sort_by(f64::total_cmp);
    let median = samples[samples.len() / 2];
    println!(
        "{name:<34} {:>9.2} µs  {:>7.2} GB/s",
        median * 1e6,
        bytes as f64 / median / 1e9
    );
}

fn main() {
    println!("fec ({})", fec::backend());
    for (k, m) in [(35, 2), (35, 6), (128, 8), (128, 32)] {
        let data = shards(k, 7);
        let refs: Vec<&[u8]> = data.iter().map(Vec::as_slice).collect();
        bench(&format!("encode k={k} m={m}"), k * SHARD, || {
            black_box(fec::encode(black_box(&refs), m, SHARD));
        });

        let parity: Vec<Option<Vec<u8>>> =
            fec::encode(&refs, m, SHARD).into_iter().map(Some).collect();
        let lost: Vec<usize> = (0..m).map(|i| i * k / m).collect();
        let mut got: Vec<Option<Vec<u8>>> = data.iter().cloned().map(Some).collect();
        bench(&format!("recover k={k} m={m}, {m} lost"), k * SHARD, || {
            for &i in &lost {
                got[i] = None;
            }
            assert!(fec::recover(black_box(&mut got), &parity, SHARD));
        });
    }
}
