//! The statistics of an A/B on pass and fail: pairs of the same task in
//! the same round, an exact sign test for "different", and exact binomial
//! bounds for "no worse". Written out here because a verdict a person acts
//! on should be checkable by hand.

#![allow(
    clippy::cast_precision_loss,
    reason = "counts of trials are far below 2^52"
)]

/// The chance of calling a difference real when there is none that a
/// verdict accepts, on one side.
pub(crate) const ALPHA: f64 = 0.05;

/// How much lower a variant's pass rate may truly be and still count as
/// "no worse", unless the command line says otherwise: ten points.
pub(crate) const MARGIN: f64 = 0.10;

/// The pairs of one variant against the baseline: the same task in the
/// same round under each.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Paired {
    /// Pairs where both passed.
    pub(crate) both_pass: u64,
    /// Pairs where both failed.
    pub(crate) both_fail: u64,
    /// Pairs where only the baseline passed: the variant did worse.
    pub(crate) only_baseline: u64,
    /// Pairs where only the variant passed: the variant did better.
    pub(crate) only_variant: u64,
}

impl Paired {
    /// All pairs.
    pub(crate) const fn pairs(&self) -> u64 {
        self.both_pass + self.both_fail + self.only_baseline + self.only_variant
    }
}

/// What the pairs say about a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// It passes less often than the baseline.
    Worse,
    /// It passes more often than the baseline.
    Better,
    /// Its pass rate is at most the margin below the baseline's.
    NoWorse,
    /// The pairs cannot tell.
    Inconclusive,
}

/// The chance of `k` or fewer successes in `n` tries that each succeed
/// with chance `p`.
pub(crate) fn binomial_cdf(k: u64, n: u64, p: f64) -> f64 {
    if k >= n || p <= 0.0 {
        return 1.0;
    }
    if p >= 1.0 {
        return 0.0;
    }
    // Summed in logarithms, so that many tries do not underflow.
    let odds = p.ln() - (1.0 - p).ln();
    let mut term = n as f64 * (1.0 - p).ln();
    let mut total = term.exp();
    for i in 0..k {
        term += ((n - i) as f64).ln() - ((i + 1) as f64).ln() + odds;
        total += term.exp();
    }
    total.min(1.0)
}

/// The exact one-sided sign test: the chance that `more` or more of
/// `more + fewer` differing pairs fall on one side when each side is
/// equally likely.
pub(crate) fn sign_test(more: u64, fewer: u64) -> f64 {
    if more == 0 {
        return 1.0;
    }
    1.0 - binomial_cdf(more - 1, more + fewer, 0.5)
}

/// The rate between 0 and 1 where `above` turns from false to true.
fn bisect(above: impl Fn(f64) -> bool) -> f64 {
    let (mut low, mut high) = (0.0_f64, 1.0_f64);
    for _ in 0..60 {
        let middle = f64::midpoint(low, high);
        if above(middle) {
            high = middle;
        } else {
            low = middle;
        }
    }
    f64::midpoint(low, high)
}

/// The exact (Clopper-Pearson) upper bound of a rate seen as `k` of `n`:
/// the true rate is above it with chance at most `alpha`.
pub(crate) fn upper_bound(k: u64, n: u64, alpha: f64) -> f64 {
    if k >= n {
        return 1.0;
    }
    bisect(|rate| binomial_cdf(k, n, rate) <= alpha)
}

/// The exact lower bound of a rate seen as `k` of `n`.
pub(crate) fn lower_bound(k: u64, n: u64, alpha: f64) -> f64 {
    if k == 0 || n == 0 {
        return 0.0;
    }
    bisect(|rate| 1.0 - binomial_cdf(k - 1, n, rate) >= alpha)
}

/// The most the variant's pass rate can truly be below the baseline's,
/// going by the pairs: the upper bound of "worse" pairs less the lower
/// bound of "better" pairs. `None` without pairs.
pub(crate) fn worst_loss(paired: &Paired) -> Option<f64> {
    let pairs = paired.pairs();
    (pairs > 0).then(|| {
        upper_bound(paired.only_baseline, pairs, ALPHA)
            - lower_bound(paired.only_variant, pairs, ALPHA)
    })
}

/// What `variant`'s pairs say, with `control` (the baseline's
/// configuration run a second time) as the measure of noise: a difference
/// counts only when it is larger than the control's. `margin` is how much
/// lower the pass rate may truly be and still count as "no worse".
pub(crate) fn verdict(variant: &Paired, control: Option<&Paired>, margin: f64) -> Verdict {
    let (worse, better) = (variant.only_baseline, variant.only_variant);
    let noise = control.map_or(0, |control| {
        control.only_baseline.abs_diff(control.only_variant)
    });
    if worse.abs_diff(better) > noise {
        if worse > better && sign_test(worse, better) <= ALPHA {
            return Verdict::Worse;
        }
        if better > worse && sign_test(better, worse) <= ALPHA {
            return Verdict::Better;
        }
    }
    match worst_loss(variant) {
        Some(loss) if loss <= margin => Verdict::NoWorse,
        _ => Verdict::Inconclusive,
    }
}

/// The fewest pairs that give "no worse" at `margin` when no pair differs.
pub(crate) fn pairs_needed(margin: f64) -> u64 {
    (1..=100_000)
        .find(|pairs| upper_bound(0, *pairs, ALPHA) <= margin)
        .unwrap_or(100_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(got: f64, want: f64) -> bool {
        (got - want).abs() < 5e-4
    }

    fn paired(both_pass: u64, both_fail: u64, only_baseline: u64, only_variant: u64) -> Paired {
        Paired {
            both_pass,
            both_fail,
            only_baseline,
            only_variant,
        }
    }

    #[test]
    fn the_binomial_sums_to_known_values() {
        assert!(close(binomial_cdf(0, 5, 0.5), 1.0 / 32.0));
        assert!(close(binomial_cdf(2, 5, 0.5), 0.5));
        assert!(close(binomial_cdf(5, 5, 0.3), 1.0));
        assert!(close(binomial_cdf(3, 10, 0.2), 0.8791));
        assert!(close(binomial_cdf(0, 3, 0.0), 1.0));
        assert!(close(binomial_cdf(2, 3, 1.0), 0.0));
        // Large counts do not underflow.
        assert!(close(binomial_cdf(1000, 2000, 0.5), 0.5089));
    }

    #[test]
    fn the_sign_test_is_exact() {
        assert!(close(sign_test(5, 0), 1.0 / 32.0));
        assert!(close(sign_test(5, 1), 7.0 / 64.0));
        assert!(close(sign_test(6, 0), 1.0 / 64.0));
        assert!(close(sign_test(0, 0), 1.0));
        assert!(close(sign_test(3, 3), 0.65625));
    }

    #[test]
    fn the_bounds_match_published_values() {
        // 0 of 29: the rule of three's exact form, 1 - 0.05^(1/29).
        assert!(close(upper_bound(0, 29, 0.05), 0.0981));
        assert!(close(lower_bound(0, 29, 0.05), 0.0));
        assert!(close(upper_bound(29, 29, 0.05), 1.0));
        assert!(close(lower_bound(29, 29, 0.05), 0.9019));
        // 5 of 20 at 2.5 % a side: the textbook interval 0.0866 to 0.4910.
        assert!(close(lower_bound(5, 20, 0.025), 0.0866));
        assert!(close(upper_bound(5, 20, 0.025), 0.4910));
        assert!(close(upper_bound(0, 0, 0.05), 1.0));
    }

    #[test]
    fn a_variant_that_fails_where_the_baseline_passes_is_worse() {
        assert_eq!(
            verdict(&paired(6, 0, 6, 0), Some(&paired(12, 0, 0, 0)), MARGIN),
            Verdict::Worse
        );
        assert_eq!(
            verdict(&paired(5, 0, 0, 7), Some(&paired(11, 0, 1, 0)), MARGIN),
            Verdict::Better
        );
        assert_eq!(verdict(&paired(6, 0, 6, 0), None, MARGIN), Verdict::Worse);
    }

    #[test]
    fn a_difference_no_larger_than_the_controls_is_not_one() {
        assert_eq!(
            verdict(&paired(6, 0, 6, 0), Some(&paired(6, 0, 6, 0)), MARGIN),
            Verdict::Inconclusive
        );
        assert_eq!(
            verdict(&paired(6, 0, 6, 0), Some(&paired(6, 0, 0, 6)), MARGIN),
            Verdict::Inconclusive
        );
    }

    #[test]
    fn no_worse_needs_enough_pairs_that_agree() {
        assert_eq!(
            verdict(&paired(25, 5, 0, 0), Some(&paired(30, 0, 0, 0)), MARGIN),
            Verdict::NoWorse
        );
        assert_eq!(
            verdict(&paired(24, 5, 0, 0), None, MARGIN),
            Verdict::NoWorse
        );
        // 28 agreeing pairs are one too few for a ten-point margin.
        assert_eq!(
            verdict(&paired(28, 0, 0, 0), None, MARGIN),
            Verdict::Inconclusive
        );
        assert_eq!(
            verdict(&paired(9, 0, 2, 1), None, MARGIN),
            Verdict::Inconclusive
        );
        assert_eq!(
            verdict(&paired(0, 0, 0, 0), None, MARGIN),
            Verdict::Inconclusive
        );
        // Many pairs, a few each way: still within the margin.
        assert_eq!(
            verdict(&paired(180, 10, 5, 5), None, MARGIN),
            Verdict::NoWorse
        );
    }

    #[test]
    fn a_wider_margin_needs_fewer_pairs() {
        assert_eq!(verdict(&paired(14, 0, 0, 0), None, 0.20), Verdict::NoWorse);
        assert_eq!(
            verdict(&paired(13, 0, 0, 0), None, 0.20),
            Verdict::Inconclusive
        );
        assert_eq!(
            verdict(&paired(14, 0, 0, 0), None, MARGIN),
            Verdict::Inconclusive
        );
        // A wider margin does not turn a real loss into "no worse".
        assert_eq!(verdict(&paired(6, 0, 6, 0), None, 0.50), Verdict::Worse);
    }

    #[test]
    fn the_pairs_a_margin_needs_when_nothing_differs() {
        assert_eq!(pairs_needed(0.10), 29);
        assert_eq!(pairs_needed(0.20), 14);
        assert_eq!(pairs_needed(0.05), 59);
        assert_eq!(pairs_needed(0.50), 5);
    }

    #[test]
    fn the_worst_loss_is_the_gap_between_the_bounds() {
        assert_eq!(worst_loss(&paired(0, 0, 0, 0)), None);
        let loss = worst_loss(&paired(29, 0, 0, 0)).expect("pairs");
        assert!(close(loss, 0.0981), "{loss}");
    }
}
