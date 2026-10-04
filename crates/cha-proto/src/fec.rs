//! Forward error correction for video frames (plan §3.1 rule 6): a
//! systematic Reed-Solomon erasure code over GF(2⁸) with a Cauchy matrix.
//!
//! A frame's data fragments are cut into blocks of up to [`BLOCK`] shards;
//! each block gets `m` parity shards. Any `k` of a block's `k + m` shards
//! rebuild it, so a frame survives up to `m` lost fragments per block
//! without a keyframe. Every square submatrix of a Cauchy matrix is
//! invertible, which is what makes "any `k`" hold.
//!
//! Shards are byte strings of one length; a shorter one (a frame's last
//! fragment) counts as padded with zeros. The browser decodes the same code
//! (`@cha/player`'s worker); the tests here pin it.

/// Data shards per block, at most (a block's data and parity stay within
/// GF(2⁸)'s 256 points).
pub const BLOCK: usize = 128;
/// Parity shards per block, at most.
pub const MAX_PARITY: usize = 64;

struct Tables {
    exp: [u8; 512],
    log: [u8; 256],
}

/// x⁸ + x⁴ + x³ + x² + 1, the usual primitive polynomial.
const POLY: u16 = 0x11d;

const fn tables() -> Tables {
    let mut exp = [0u8; 512];
    let mut log = [0u8; 256];
    let mut x: u16 = 1;
    let mut i = 0;
    while i < 255 {
        exp[i] = x as u8;
        log[x as usize] = i as u8;
        x <<= 1;
        if x & 0x100 != 0 {
            x ^= POLY;
        }
        i += 1;
    }
    while i < 512 {
        exp[i] = exp[i - 255];
        i += 1;
    }
    Tables { exp, log }
}

static GF: Tables = tables();

fn mul(a: u8, b: u8) -> u8 {
    if a == 0 || b == 0 {
        0
    } else {
        GF.exp[GF.log[a as usize] as usize + GF.log[b as usize] as usize]
    }
}

fn inv(a: u8) -> u8 {
    debug_assert!(a != 0);
    GF.exp[255 - GF.log[a as usize] as usize]
}

/// The Cauchy coefficient of parity row `r` for data shard `i` (of `m`
/// parity rows): 1 / (r ⊕ (m + i)), the points distinct while m + k ≤ 256.
fn coefficient(r: usize, i: usize, m: usize) -> u8 {
    inv((r as u8) ^ ((m + i) as u8))
}

/// `acc ^= c · src`, byte by byte (`src` shorter: zeros after it).
fn mul_add(acc: &mut [u8], src: &[u8], c: u8) {
    if c == 0 {
        return;
    }
    let lc = GF.log[c as usize] as usize;
    for (a, &s) in acc.iter_mut().zip(src) {
        if s != 0 {
            *a ^= GF.exp[lc + GF.log[s as usize] as usize];
        }
    }
}

/// The `m` parity shards (each `shard_len` long) of one block's data shards.
pub fn encode(data: &[&[u8]], m: usize, shard_len: usize) -> Vec<Vec<u8>> {
    assert!(
        data.len() + m <= 256,
        "a block of {} + {m} shards",
        data.len()
    );
    (0..m)
        .map(|r| {
            let mut parity = vec![0u8; shard_len];
            for (i, shard) in data.iter().enumerate() {
                mul_add(&mut parity, shard, coefficient(r, i, m));
            }
            parity
        })
        .collect()
}

/// Rebuilds a block's missing data shards in place from its parity shards
/// (`parity[r]` is row `r`, if it arrived). Data shards come back
/// `shard_len` long (zero-padded). False if too few shards arrived.
pub fn recover(data: &mut [Option<Vec<u8>>], parity: &[Option<Vec<u8>>], shard_len: usize) -> bool {
    let k = data.len();
    let m = parity.len();
    let missing: Vec<usize> = (0..k).filter(|&i| data[i].is_none()).collect();
    if missing.is_empty() {
        return true;
    }
    let rows: Vec<usize> = (0..m)
        .filter(|&r| parity[r].is_some())
        .take(missing.len())
        .collect();
    if rows.len() < missing.len() {
        return false;
    }
    let e = missing.len();
    // Each row: the parity minus what the shards we have contribute, which
    // leaves the missing shards' share.
    let mut syndromes: Vec<Vec<u8>> = rows
        .iter()
        .map(|&r| {
            let mut s = parity[r].clone().expect("chosen rows arrived");
            s.resize(shard_len, 0);
            for (i, shard) in data.iter().enumerate() {
                if let Some(shard) = shard {
                    mul_add(&mut s, shard, coefficient(r, i, m));
                }
            }
            s
        })
        .collect();
    // Solve the e × e Cauchy system by Gauss–Jordan elimination, applying
    // each row operation to the syndromes too.
    let mut a: Vec<Vec<u8>> = rows
        .iter()
        .map(|&r| missing.iter().map(|&i| coefficient(r, i, m)).collect())
        .collect();
    for col in 0..e {
        let Some(pivot) = (col..e).find(|&row| a[row][col] != 0) else {
            return false;
        };
        a.swap(col, pivot);
        syndromes.swap(col, pivot);
        let scale = inv(a[col][col]);
        for v in a[col].iter_mut() {
            *v = mul(*v, scale);
        }
        let row = std::mem::take(&mut syndromes[col]);
        let scaled: Vec<u8> = row.iter().map(|&v| mul(v, scale)).collect();
        syndromes[col] = scaled;
        for other in 0..e {
            let factor = a[other][col];
            if other == col || factor == 0 {
                continue;
            }
            let pivot_row = a[col].clone();
            for (v, p) in a[other].iter_mut().zip(&pivot_row) {
                *v ^= mul(factor, *p);
            }
            let pivot_syndrome = syndromes[col].clone();
            mul_add(&mut syndromes[other], &pivot_syndrome, factor);
        }
    }
    for (j, &i) in missing.iter().enumerate() {
        data[i] = Some(std::mem::take(&mut syndromes[j]));
    }
    true
}

/// The parity a block of `k` data shards needs so that, with each datagram
/// lost on its own with probability `loss`, more of the block is lost than
/// its parity can rebuild with probability under `failure`. At most
/// [`MAX_PARITY`]; none without loss.
pub fn parity_for(k: usize, loss: f64, failure: f64) -> usize {
    if loss <= 0.0 {
        return 0;
    }
    let p = loss.min(0.5);
    (1..=MAX_PARITY)
        .find(|&m| beyond(k + m, m, p) < failure)
        .unwrap_or(MAX_PARITY)
}

/// The chance that more than `m` of `n` datagrams are lost, each with
/// probability `p`.
fn beyond(n: usize, m: usize, p: f64) -> f64 {
    let q = 1.0 - p;
    // P(i lost), each from the last: P(0) = qⁿ, P(i+1) = P(i)·(n−i)/(i+1)·p/q.
    let mut exactly = q.powi(n as i32);
    let mut at_most = 0.0;
    for i in 0..=m.min(n) {
        at_most += exactly;
        exactly *= (n - i) as f64 / (i + 1) as f64 * p / q;
    }
    (1.0 - at_most).max(0.0)
}

/// How a frame of `k` data shards splits into blocks: (first shard, count).
pub fn blocks(k: usize) -> impl Iterator<Item = (usize, usize)> {
    (0..k.div_ceil(BLOCK)).map(move |b| (b * BLOCK, (k - b * BLOCK).min(BLOCK)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shards(k: usize, len: usize, seed: u32) -> Vec<Vec<u8>> {
        let mut x = seed.wrapping_mul(2654435761) | 1;
        (0..k)
            .map(|i| {
                // The last shard shorter, as a frame's last fragment is.
                let n = if i == k - 1 { len / 3 } else { len };
                (0..n)
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

    #[test]
    fn field_inverses() {
        for a in 1..=255u8 {
            assert_eq!(mul(a, inv(a)), 1);
        }
    }

    #[test]
    fn any_k_of_k_plus_m_rebuild_the_block() {
        let (k, m, len) = (40, 6, 1200);
        let data = shards(k, len, 7);
        let refs: Vec<&[u8]> = data.iter().map(Vec::as_slice).collect();
        let parity = encode(&refs, m, len);
        // Lose 6 data shards, keep all parity.
        let mut got: Vec<Option<Vec<u8>>> = data.iter().cloned().map(Some).collect();
        for i in [0, 3, 17, 18, 38, 39] {
            got[i] = None;
        }
        let par: Vec<Option<Vec<u8>>> = parity.iter().cloned().map(Some).collect();
        assert!(recover(&mut got, &par, len));
        for (i, shard) in got.iter().enumerate() {
            let shard = shard.as_ref().unwrap();
            assert_eq!(&shard[..data[i].len()], &data[i][..], "shard {i}");
            assert!(shard[data[i].len()..].iter().all(|&b| b == 0));
        }
    }

    #[test]
    fn some_parity_lost_too() {
        let (k, m, len) = (20, 4, 300);
        let data = shards(k, len, 11);
        let refs: Vec<&[u8]> = data.iter().map(Vec::as_slice).collect();
        let parity = encode(&refs, m, len);
        let mut got: Vec<Option<Vec<u8>>> = data.iter().cloned().map(Some).collect();
        got[5] = None;
        got[9] = None;
        let mut par: Vec<Option<Vec<u8>>> = parity.iter().cloned().map(Some).collect();
        par[0] = None;
        par[2] = None;
        assert!(recover(&mut got, &par, len));
        assert_eq!(got[5].as_deref().unwrap(), &data[5][..]);
        assert_eq!(&got[9].as_deref().unwrap()[..data[9].len()], &data[9][..]);
        // One more lost than parity left: can't.
        let mut got: Vec<Option<Vec<u8>>> = data.iter().cloned().map(Some).collect();
        got[1] = None;
        got[2] = None;
        got[3] = None;
        assert!(!recover(&mut got, &par, len));
    }

    #[test]
    fn blocks_cover_the_frame() {
        assert_eq!(
            blocks(300).collect::<Vec<_>>(),
            [(0, 128), (128, 128), (256, 44)]
        );
        assert_eq!(blocks(5).collect::<Vec<_>>(), [(0, 5)]);
    }

    #[test]
    fn parity_for_the_loss() {
        assert_eq!(parity_for(28, 0.0, 1e-4), 0);
        // 0.5 %: two parity for 28 fail 4.5 in 10⁴ (three of 30 lost);
        // three, 1.7 in 10⁵.
        assert_eq!(parity_for(28, 0.005, 1e-4), 3);
        assert!(beyond(30, 2, 0.005) > 1e-4 && beyond(31, 3, 0.005) < 1e-4);
        // A one-fragment frame: both lost is 9 in 10⁶.
        assert_eq!(parity_for(1, 0.003, 1e-4), 1);
        // More loss, a stricter target or a bigger block: more parity.
        assert!(parity_for(28, 0.03, 1e-4) > parity_for(28, 0.005, 1e-4));
        assert!(parity_for(28, 0.005, 1e-6) > parity_for(28, 0.005, 1e-4));
        assert!(parity_for(128, 0.005, 1e-4) > parity_for(28, 0.005, 1e-4));
        assert_eq!(parity_for(128, 0.5, 1e-6), MAX_PARITY);
    }

    /// Pins the code for the browser's decoder: parity of three short shards.
    #[test]
    fn known_vector() {
        let data: [&[u8]; 3] = [&[1, 2, 3], &[4, 5, 6], &[7, 8]];
        assert_eq!(encode(&data, 2, 3), [vec![177, 0, 141], vec![164, 40, 2]]);
    }
}
