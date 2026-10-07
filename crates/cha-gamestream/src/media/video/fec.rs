// Ported from Moonshine (https://github.com/hgaiser/moonshine), BSD-2-Clause, © 2024 Hans Gaiser; see LICENSE-MOONSHINE.
// Changed: the block plan is its own function and agrees with how Moonlight counts parity
// (ceil(data * percent / 100)), and a frame too big for four blocks lowers the FEC instead of overrunning.

//! Forward error correction for video: how a frame's packets split into FEC
//! blocks, and the Reed-Solomon codecs for them.
//!
//! A block holds at most 255 shards, data and parity together; a frame has at
//! most four blocks. Moonlight does not receive the parity count: it computes
//! `ceil(data * percent / 100)` from the percentage in each packet's header,
//! so the host must send exactly that many.

use std::collections::HashMap;

use fec_rs::ReedSolomon;

pub(crate) const MAX_SHARDS: usize = 255;
pub(crate) const MAX_BLOCKS: usize = 4;

/// Parity shards Moonlight expects for `data` shards at `percent`.
pub(crate) fn parity_for(data: usize, percent: usize) -> usize {
    (data * percent).div_ceil(100)
}

/// The percentage to put in a block's headers, and the parity it implies,
/// for `data` shards when the stream wants `percent` and at least `min`
/// parity shards.
fn block_fec(data: usize, percent: usize, min: usize) -> (usize, usize) {
    let mut percent = percent;
    if parity_for(data, percent) < min {
        // 255 is all the header's 8 bits hold; a one-packet frame can't have
        // many parity packets, and that is acceptable.
        percent = (100 * min).div_ceil(data).min(255);
    }
    (percent, parity_for(data, percent))
}

/// The most data shards a block can hold at this FEC setting.
fn max_data_per_block(percent: usize, min: usize) -> usize {
    (1..=MAX_SHARDS)
        .rev()
        .find(|&d| d + block_fec(d, percent, min).1 <= MAX_SHARDS)
        .unwrap_or(1)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BlockPlan {
    pub data: usize,
    pub parity: usize,
    /// What the packet headers say.
    pub percent: usize,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("a frame of {data_shards} packets doesn't fit {MAX_BLOCKS} FEC blocks")]
pub(crate) struct TooLarge {
    pub data_shards: usize,
}

/// Splits `data_shards` packets into at most four blocks. When the stream's
/// FEC setting would need more, the percentage drops (to nothing, if need be)
/// until it fits: a big keyframe with less protection beats no keyframe.
pub(crate) fn plan(
    data_shards: usize,
    percent: usize,
    min: usize,
) -> Result<Vec<BlockPlan>, TooLarge> {
    let (mut percent, mut min) = (percent, min);
    loop {
        let per_block = max_data_per_block(percent, min);
        if data_shards.div_ceil(per_block) <= MAX_BLOCKS {
            let blocks = data_shards.div_ceil(per_block);
            // Spread evenly so the last block isn't a stub.
            let (base, extra) = (data_shards / blocks, data_shards % blocks);
            return Ok((0..blocks)
                .map(|i| {
                    let data = base + usize::from(i < extra);
                    let (percent, parity) = block_fec(data, percent, min);
                    BlockPlan {
                        data,
                        parity,
                        percent,
                    }
                })
                .collect());
        }
        if percent == 0 {
            if min == 0 {
                return Err(TooLarge { data_shards });
            }
            min = 0;
        } else {
            percent = percent.saturating_sub(5);
            if percent == 0 {
                min = 0;
            }
        }
    }
}

/// Reed-Solomon codecs by shape; building one costs a matrix inversion, so
/// they are kept.
#[derive(Default)]
pub(crate) struct Codecs {
    by_shape: HashMap<(usize, usize), ReedSolomon>,
}

impl Codecs {
    pub fn get(&mut self, data: usize, parity: usize) -> Result<&ReedSolomon, fec_rs::Error> {
        use std::collections::hash_map::Entry;
        Ok(match self.by_shape.entry((data, parity)) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => e.insert(ReedSolomon::new(data, parity)?),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parity_counts_the_way_moonlight_does() {
        assert_eq!(parity_for(10, 20), 2);
        assert_eq!(parity_for(7, 20), 2);
        assert_eq!(parity_for(1, 20), 1);
        assert_eq!(parity_for(100, 0), 0);
    }

    #[test]
    fn small_frame_is_one_block() {
        let p = plan(10, 20, 0).unwrap();
        assert_eq!(
            p,
            vec![BlockPlan {
                data: 10,
                parity: 2,
                percent: 20
            }]
        );
    }

    #[test]
    fn minimum_parity_raises_the_percentage() {
        let p = plan(1, 20, 2).unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].data, 1);
        assert_eq!(p[0].parity, 2);
        assert_eq!(parity_for(1, p[0].percent), 2);
    }

    #[test]
    fn big_frame_splits_into_blocks_that_fit() {
        let p = plan(500, 20, 0).unwrap();
        assert_eq!(p.len(), 3);
        assert_eq!(p.iter().map(|b| b.data).sum::<usize>(), 500);
        for b in &p {
            assert!(b.data + b.parity <= MAX_SHARDS);
            assert_eq!(parity_for(b.data, b.percent), b.parity);
        }
    }

    #[test]
    fn a_frame_too_big_for_the_fec_setting_sheds_protection() {
        // 4 blocks of 20% hold ~848 data shards; 900 needs less FEC.
        let p = plan(900, 20, 0).unwrap();
        assert_eq!(p.len(), 4);
        assert_eq!(p.iter().map(|b| b.data).sum::<usize>(), 900);
        assert!(p.iter().all(|b| b.data + b.parity <= MAX_SHARDS));
        assert!(p.iter().all(|b| b.percent < 20));
    }

    #[test]
    fn beyond_four_full_blocks_is_an_error() {
        assert_eq!(plan(1021, 20, 0), Err(TooLarge { data_shards: 1021 }));
        assert!(plan(1020, 20, 0).is_ok());
    }

    #[test]
    fn every_plan_is_consistent_for_all_sizes() {
        for data in 1..=1020 {
            for (pct, min) in [(0, 0), (20, 0), (20, 2), (50, 4), (5, 1)] {
                let blocks = plan(data, pct, min).unwrap();
                assert!(blocks.len() <= MAX_BLOCKS);
                assert_eq!(blocks.iter().map(|b| b.data).sum::<usize>(), data);
                for b in &blocks {
                    assert!(
                        b.data >= 1 && b.data + b.parity <= MAX_SHARDS,
                        "{data} {pct} {min}: {b:?}"
                    );
                    assert_eq!(parity_for(b.data, b.percent), b.parity);
                }
            }
        }
    }
}
