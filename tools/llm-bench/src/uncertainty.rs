//! How far a place can be trusted. Each model's rounds are drawn again with replacement, many times over, and the
//! models are placed on every draw; a model's interval is where it lands in most of them, so overlapping intervals are
//! places this sample cannot tell apart. Two questions, two intervals:
//!
//!   rerun     the same cases, rounds redrawn: where a model lands if the run is repeated.
//!   case mix  the cases redrawn too, the same draw for every model: where it lands on a similar set of cases.
//!
//! Reporting only: composites, places and standings are unchanged.

/// Draws per interval.
pub const RESAMPLES: usize = 2000;
/// Share of the draws an interval covers.
pub const COVERAGE: f64 = 0.9;
/// Fixed, so the same results always give the same intervals.
const SEED: u64 = 0x11b_e7c4;

/// SplitMix64: enough randomness for resampling, without a dependency.
struct Draw(u64);

impl Draw {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// `rounds[model][case]` holds that model's composite for each round of the case; every model has the same cases.
/// Returns each model's place interval, 1 = best. Places are shared on equal composites.
pub fn place_intervals(rounds: &[Vec<Vec<f64>>], redraw_cases: bool) -> Vec<(usize, usize)> {
    let Some(first) = rounds.first() else { return Vec::new() };
    let cases = first.len();
    assert!(cases > 0 && rounds.iter().all(|m| m.len() == cases && m.iter().all(|r| !r.is_empty())),
        "every model needs rounds for the same cases");
    let mut draw = Draw(SEED);
    let mut places: Vec<Vec<usize>> = vec![Vec::with_capacity(RESAMPLES); rounds.len()];
    let mut means = vec![0.0; rounds.len()];
    for _ in 0..RESAMPLES {
        let picked: Vec<usize> = (0..cases).map(|c| if redraw_cases { draw.below(cases) } else { c }).collect();
        for (model, mean) in rounds.iter().zip(means.iter_mut()) {
            let total: f64 = picked
                .iter()
                .map(|&c| {
                    let r = &model[c];
                    (0..r.len()).map(|_| r[draw.below(r.len())]).sum::<f64>() / r.len() as f64
                })
                .sum();
            *mean = total / cases as f64;
        }
        for (m, place) in places.iter_mut().enumerate() {
            place.push(1 + means.iter().filter(|&&other| other > means[m]).count());
        }
    }
    let low = ((1.0 - COVERAGE) / 2.0 * RESAMPLES as f64).floor() as usize;
    let high = ((1.0 + COVERAGE) / 2.0 * RESAMPLES as f64).ceil() as usize - 1;
    places
        .into_iter()
        .map(|mut p| {
            p.sort_unstable();
            (p[low], p[high])
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clear_lead_holds_its_place_and_a_close_pair_shares_a_range() {
        let model = |base: f64, spread: f64| -> Vec<Vec<f64>> {
            (0..6).map(|c| (0..3).map(|r| base + spread * (((c * 3 + r) % 5) as f64 - 2.0)).collect()).collect()
        };
        // a leads by far; b and c differ by less than their own round-to-round spread
        for redraw_cases in [false, true] {
            let intervals = place_intervals(&[model(95.0, 0.5), model(80.0, 4.0), model(79.5, 4.0)], redraw_cases);
            assert_eq!(intervals[0], (1, 1));
            assert_eq!(intervals[1], (2, 3));
            assert_eq!(intervals[2], (2, 3));
        }
        let pair = [model(95.0, 0.5), model(80.0, 4.0)];
        assert_eq!(place_intervals(&pair, true), place_intervals(&pair, true), "a fixed seed");
    }

    #[test]
    fn a_case_where_models_trade_places_widens_only_the_case_mix_interval() {
        // steady rounds; a wins one case by far, b another by a little, so the case mix decides the place
        let mut a = vec![vec![80.0; 3]; 6];
        let mut b = a.clone();
        a[5] = vec![100.0; 3];
        b[0] = vec![81.0; 3];
        let models = [a, b];
        assert_eq!(place_intervals(&models, false), vec![(1, 1), (2, 2)]);
        assert_eq!(place_intervals(&models, true), vec![(1, 2), (1, 2)]);
    }
}
