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
//!
//! The work is in [`mul_add`]: a shard times a coefficient, added into
//! another, for every byte of every shard. Multiplying by a constant is linear
//! over GF(2), so a byte's product is the XOR of the products of its two
//! nibbles, which two 16-entry tables per coefficient hold. SIMD shuffles look
//! up 16 or 32 bytes' worth at once (`pshufb` on x86, `tbl` on ARM); where the
//! CPU has GFNI (Intel from Ice Lake, AMD from Zen 4), one instruction
//! multiplies 32 bytes by the constant as an 8 × 8 bit matrix. The fastest
//! kernel the CPU has is picked at first use ([`backend`]); every kernel gives
//! the same bytes. (A 64-byte AVX-512 GFNI kernel measured no faster on
//! 1,180-byte shards, so there is none.) `cargo bench -p cha-proto` times them.

use std::sync::OnceLock;

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

/// The product of two field elements, without tables, so it can build them
/// at compile time.
const fn gf_mul(mut a: u8, mut b: u8) -> u8 {
    let mut product = 0;
    while b != 0 {
        if b & 1 != 0 {
            product ^= a;
        }
        let carry = a & 0x80 != 0;
        a <<= 1;
        if carry {
            a ^= (POLY & 0xff) as u8;
        }
        b >>= 1;
    }
    product
}

/// Multiplication by one coefficient, ready for each kernel: the products of
/// the 16 low nibbles and of the 16 high ones, and the same map as GFNI's bit
/// matrix (byte `7 − i` is the row giving output bit `i`).
struct Coef {
    lo: [u8; 16],
    hi: [u8; 16],
    matrix: u64,
}

const fn coefs() -> [Coef; 256] {
    let mut out = [const {
        Coef {
            lo: [0; 16],
            hi: [0; 16],
            matrix: 0,
        }
    }; 256];
    let mut c = 0;
    while c < 256 {
        let mut n = 0;
        while n < 16 {
            out[c].lo[n] = gf_mul(c as u8, n as u8);
            out[c].hi[n] = gf_mul(c as u8, (n as u8) << 4);
            n += 1;
        }
        let mut matrix = 0u64;
        let mut i = 0;
        while i < 8 {
            let mut row = 0u64;
            let mut j = 0;
            while j < 8 {
                if gf_mul(c as u8, 1 << j) >> i & 1 != 0 {
                    row |= 1 << j;
                }
                j += 1;
            }
            matrix |= row << (8 * (7 - i));
            i += 1;
        }
        out[c].matrix = matrix;
        c += 1;
    }
    out
}

static COEFS: [Coef; 256] = coefs();

/// `acc ^= c · src` over the shorter of the two (`src` shorter: zeros after it).
fn mul_add(acc: &mut [u8], src: &[u8], c: u8) {
    let n = acc.len().min(src.len());
    let (acc, src) = (&mut acc[..n], &src[..n]);
    match c {
        0 => {}
        1 => acc.iter_mut().zip(src).for_each(|(a, s)| *a ^= s),
        _ => {
            let kernel = chosen().kernel;
            // SAFETY: `choose` returns a kernel only once it has checked that
            // this CPU has the features it was compiled for, and the slices
            // are the same length, as every kernel requires.
            unsafe { kernel(acc, src, &COEFS[usize::from(c)]) }
        }
    }
}

/// A kernel for [`mul_add`]: `acc` and `src` are the same length, and the CPU
/// has the target features the kernel is compiled with.
type Kernel = unsafe fn(&mut [u8], &[u8], &Coef);

struct Backend {
    kernel: Kernel,
    name: &'static str,
}

fn chosen() -> &'static Backend {
    static CHOSEN: OnceLock<Backend> = OnceLock::new();
    CHOSEN.get_or_init(choose)
}

/// The arithmetic this machine uses ("avx2-gfni", "avx2", "ssse3", "neon"
/// or "scalar"), picked at first use.
pub fn backend() -> &'static str {
    chosen().name
}

fn choose() -> Backend {
    #[cfg(target_arch = "x86_64")]
    {
        if std::arch::is_x86_feature_detected!("gfni")
            && std::arch::is_x86_feature_detected!("avx2")
        {
            return Backend {
                kernel: x86::avx2_gfni,
                name: "avx2-gfni",
            };
        }
        if std::arch::is_x86_feature_detected!("avx2") {
            return Backend {
                kernel: x86::avx2,
                name: "avx2",
            };
        }
        if std::arch::is_x86_feature_detected!("ssse3") {
            return Backend {
                kernel: x86::ssse3,
                name: "ssse3",
            };
        }
    }
    #[cfg(target_arch = "aarch64")]
    if std::arch::is_aarch64_feature_detected!("neon") {
        return Backend {
            kernel: arm::neon,
            name: "neon",
        };
    }
    Backend {
        kernel: scalar,
        name: "scalar",
    }
}

/// The portable kernel, and the tail after the SIMD ones' last whole vector.
fn scalar(acc: &mut [u8], src: &[u8], coef: &Coef) {
    for (a, &s) in acc.iter_mut().zip(src) {
        *a ^= coef.lo[usize::from(s & 0x0f)] ^ coef.hi[usize::from(s >> 4)];
    }
}

#[cfg(target_arch = "x86_64")]
mod x86 {
    use std::arch::x86_64::*;

    use super::{Coef, scalar};

    #[target_feature(enable = "ssse3")]
    pub(super) fn ssse3(acc: &mut [u8], src: &[u8], coef: &Coef) {
        let (acc, acc_tail) = acc.as_chunks_mut::<16>();
        let (src, src_tail) = src.as_chunks::<16>();
        // SAFETY: the tables are 16 bytes, an unaligned 128-bit load.
        let (lo, hi) = unsafe {
            (
                _mm_loadu_si128(coef.lo.as_ptr().cast()),
                _mm_loadu_si128(coef.hi.as_ptr().cast()),
            )
        };
        let nibble = _mm_set1_epi8(0x0f);
        for (a, s) in acc.iter_mut().zip(src) {
            // SAFETY: each chunk is 16 bytes, read and written unaligned.
            unsafe {
                let s = _mm_loadu_si128(s.as_ptr().cast());
                let low = _mm_shuffle_epi8(lo, _mm_and_si128(s, nibble));
                let high = _mm_shuffle_epi8(hi, _mm_and_si128(_mm_srli_epi64(s, 4), nibble));
                let a_ptr = a.as_mut_ptr().cast();
                let sum = _mm_xor_si128(_mm_loadu_si128(a_ptr), _mm_xor_si128(low, high));
                _mm_storeu_si128(a_ptr, sum);
            }
        }
        scalar(acc_tail, src_tail, coef);
    }

    #[target_feature(enable = "avx2")]
    pub(super) fn avx2(acc: &mut [u8], src: &[u8], coef: &Coef) {
        let (acc, acc_tail) = acc.as_chunks_mut::<32>();
        let (src, src_tail) = src.as_chunks::<32>();
        // SAFETY: the tables are 16 bytes, an unaligned 128-bit load; the
        // shuffle looks up within each 128-bit lane, so both lanes get them.
        let (lo, hi) = unsafe {
            (
                _mm256_broadcastsi128_si256(_mm_loadu_si128(coef.lo.as_ptr().cast())),
                _mm256_broadcastsi128_si256(_mm_loadu_si128(coef.hi.as_ptr().cast())),
            )
        };
        let nibble = _mm256_set1_epi8(0x0f);
        for (a, s) in acc.iter_mut().zip(src) {
            // SAFETY: each chunk is 32 bytes, read and written unaligned.
            unsafe {
                let s = _mm256_loadu_si256(s.as_ptr().cast());
                let low = _mm256_shuffle_epi8(lo, _mm256_and_si256(s, nibble));
                let high =
                    _mm256_shuffle_epi8(hi, _mm256_and_si256(_mm256_srli_epi64(s, 4), nibble));
                let a_ptr = a.as_mut_ptr().cast();
                let sum = _mm256_xor_si256(_mm256_loadu_si256(a_ptr), _mm256_xor_si256(low, high));
                _mm256_storeu_si256(a_ptr, sum);
            }
        }
        scalar(acc_tail, src_tail, coef);
    }

    #[target_feature(enable = "gfni,avx2")]
    pub(super) fn avx2_gfni(acc: &mut [u8], src: &[u8], coef: &Coef) {
        let (acc, acc_tail) = acc.as_chunks_mut::<32>();
        let (src, src_tail) = src.as_chunks::<32>();
        let matrix = _mm256_set1_epi64x(coef.matrix as i64);
        for (a, s) in acc.iter_mut().zip(src) {
            // SAFETY: each chunk is 32 bytes, read and written unaligned.
            unsafe {
                let product = _mm256_gf2p8affine_epi64_epi8::<0>(
                    _mm256_loadu_si256(s.as_ptr().cast()),
                    matrix,
                );
                let a_ptr = a.as_mut_ptr().cast();
                _mm256_storeu_si256(a_ptr, _mm256_xor_si256(_mm256_loadu_si256(a_ptr), product));
            }
        }
        scalar(acc_tail, src_tail, coef);
    }
}

#[cfg(target_arch = "aarch64")]
mod arm {
    use std::arch::aarch64::*;

    use super::{Coef, scalar};

    #[target_feature(enable = "neon")]
    pub(super) fn neon(acc: &mut [u8], src: &[u8], coef: &Coef) {
        let (acc, acc_tail) = acc.as_chunks_mut::<16>();
        let (src, src_tail) = src.as_chunks::<16>();
        // SAFETY: the tables are 16 bytes.
        let (lo, hi) = unsafe { (vld1q_u8(coef.lo.as_ptr()), vld1q_u8(coef.hi.as_ptr())) };
        let nibble = vdupq_n_u8(0x0f);
        for (a, s) in acc.iter_mut().zip(src) {
            // SAFETY: each chunk is 16 bytes.
            unsafe {
                let s = vld1q_u8(s.as_ptr());
                let low = vqtbl1q_u8(lo, vandq_u8(s, nibble));
                let high = vqtbl1q_u8(hi, vshrq_n_u8::<4>(s));
                let sum = veorq_u8(vld1q_u8(a.as_ptr()), veorq_u8(low, high));
                vst1q_u8(a.as_mut_ptr(), sum);
            }
        }
        scalar(acc_tail, src_tail, coef);
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
        // The pivot row, scaled, is set aside while it's added into the others.
        let pivot_row = a[col].clone();
        let mut pivot_syndrome = vec![0; shard_len];
        mul_add(&mut pivot_syndrome, &syndromes[col], scale);
        for other in 0..e {
            let factor = a[other][col];
            if other == col || factor == 0 {
                continue;
            }
            for (v, p) in a[other].iter_mut().zip(&pivot_row) {
                *v ^= mul(factor, *p);
            }
            mul_add(&mut syndromes[other], &pivot_syndrome, factor);
        }
        syndromes[col] = pivot_syndrome;
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
    fn the_tables_built_at_compile_time_match_the_log_tables() {
        for a in 0..=255u8 {
            for b in 0..=255u8 {
                assert_eq!(gf_mul(a, b), mul(a, b), "{a} · {b}");
            }
        }
    }

    /// Every kernel this CPU can run, on lengths around each vector width and
    /// a shard's, for every coefficient: the same bytes as the scalar one.
    #[test]
    fn every_kernel_gives_the_same_bytes() {
        let mut kernels: Vec<(&str, Kernel)> = vec![("scalar", scalar)];
        #[cfg(target_arch = "x86_64")]
        {
            use std::arch::is_x86_feature_detected as has;
            if has!("ssse3") {
                kernels.push(("ssse3", x86::ssse3));
            }
            if has!("avx2") {
                kernels.push(("avx2", x86::avx2));
            }
            if has!("gfni") && has!("avx2") {
                kernels.push(("avx2-gfni", x86::avx2_gfni));
            }
        }
        #[cfg(target_arch = "aarch64")]
        if std::arch::is_aarch64_feature_detected!("neon") {
            kernels.push(("neon", arm::neon));
        }
        println!(
            "kernels tested: {:?}",
            kernels.iter().map(|k| k.0).collect::<Vec<_>>()
        );
        let lengths = [0, 1, 15, 16, 17, 31, 32, 33, 63, 64, 65, 127, 129, 1180];
        // The first of two: `shards` makes a frame's last one shorter.
        let src = &shards(2, 1200, 3)[0];
        let start = &shards(2, 1200, 5)[0];
        for c in 0..=255u8 {
            for &n in &lengths {
                // Unaligned starts too: shards sit at any offset in a datagram.
                for offset in [0, 1, 7] {
                    let (src, start) = (&src[offset..offset + n], &start[offset..offset + n]);
                    let want: Vec<u8> = start
                        .iter()
                        .zip(src)
                        .map(|(&a, &s)| a ^ mul(c, s))
                        .collect();
                    for (name, kernel) in &kernels {
                        let mut got = start.to_vec();
                        // SAFETY: the kernel was added only if the CPU has its
                        // features; the slices are the same length.
                        unsafe { kernel(&mut got, src, &COEFS[usize::from(c)]) };
                        assert_eq!(got, want, "{name}, c = {c}, {n} bytes at +{offset}");
                    }
                }
            }
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
