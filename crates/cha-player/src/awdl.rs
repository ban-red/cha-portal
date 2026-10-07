//! Spotting AWDL (AirDrop / Continuity) interference on a Wi-Fi link.
//!
//! macOS hops the Wi-Fi radio to another channel for AWDL about once a second,
//! which shows up in a stream as short gaps in video arrival (roughly 80 to
//! 200 ms) that recur at that rhythm while everything between them is
//! steady. A link that is just bad has gaps all over instead.

/// Gaps in this range are the AWDL kind: longer than a normal frame interval,
/// shorter than a real outage.
const GAP_MS: std::ops::RangeInclusive<u64> = 80..=200;
/// How far apart consecutive AWDL gaps are.
const PERIOD_MS: std::ops::RangeInclusive<u64> = 500..=1800;
/// Needed before saying anything: this many gaps in the rhythm.
const MIN_GAPS: usize = 3;

/// `arrivals_ms` are video frame arrival times, ascending, in ms. True when
/// they carry the AWDL signature: at least [`MIN_GAPS`] AWDL-sized gaps, each
/// about a second after the one before, and few other long gaps.
pub fn looks_like_awdl(arrivals_ms: &[u64]) -> bool {
    let mut spikes = Vec::new(); // when each AWDL-sized gap began
    let mut other_long = 0;
    for pair in arrivals_ms.windows(2) {
        let gap = pair[1] - pair[0];
        if GAP_MS.contains(&gap) {
            spikes.push(pair[0]);
        } else if gap > *GAP_MS.end() {
            other_long += 1;
        }
    }
    // A run of spikes in the rhythm.
    let mut run = 1;
    let mut best = 1;
    for pair in spikes.windows(2) {
        if PERIOD_MS.contains(&(pair[1] - pair[0])) {
            run += 1;
            best = best.max(run);
        } else {
            run = 1;
        }
    }
    best >= MIN_GAPS && other_long <= spikes.len() / 3
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Frames every `interval` ms for `secs`, with a `gap` ms stall starting at
    /// each of `stalls` (ms).
    fn timeline(interval: u64, secs: u64, stalls: &[(u64, u64)]) -> Vec<u64> {
        let mut t = 0;
        let mut out = Vec::new();
        while t < secs * 1000 {
            out.push(t);
            t += interval;
            if let Some((_, gap)) = stalls.iter().find(|(at, _)| (t - interval..t).contains(at)) {
                t += gap;
            }
        }
        out
    }

    #[test]
    fn a_steady_stream_is_clean() {
        assert!(!looks_like_awdl(&timeline(16, 10, &[])));
    }

    #[test]
    fn spikes_about_every_second_are_awdl() {
        let stalls: Vec<_> = (1..9).map(|i| (i * 1050, 120)).collect();
        assert!(looks_like_awdl(&timeline(16, 10, &stalls)));
    }

    #[test]
    fn one_or_two_spikes_are_not_enough() {
        assert!(!looks_like_awdl(&timeline(
            16,
            10,
            &[(2000, 120), (3050, 120)]
        )));
    }

    #[test]
    fn irregular_stalls_are_not_awdl() {
        let stalls = [(1000, 150), (1500, 120), (4700, 140), (4800, 90)];
        assert!(!looks_like_awdl(&timeline(16, 10, &stalls)));
    }

    #[test]
    fn a_link_full_of_outages_is_not_awdl() {
        let mut stalls: Vec<_> = (1..9).map(|i| (i * 1000, 120)).collect();
        stalls.extend((0..9).map(|i| (i * 1000 + 500, 600)));
        assert!(!looks_like_awdl(&timeline(16, 10, &stalls)));
    }
}
