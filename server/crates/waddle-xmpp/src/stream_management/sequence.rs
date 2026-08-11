//! Wrap-aware XEP-0198 sequence comparisons.
//!
//! XEP-0198 counters wrap modulo `2^32`. Exactly half a counter space apart
//! is a degenerate case: neither value is strictly greater than the other, so
//! `sequence_gt` is false in both directions and its derived `sequence_lte`
//! is true in both directions. Real SM acknowledgement windows are far
//! smaller than that ambiguous distance.

/// Returns whether `a` is strictly after `b` in the unambiguous half of the
/// XEP-0198 wrapping sequence space.
pub(crate) fn sequence_gt(a: u32, b: u32) -> bool {
    if a == b {
        return false;
    }
    let diff = a.wrapping_sub(b);
    diff < 0x8000_0000
}

/// Returns whether `a` is at or behind `b` in the XEP-0198 wrapping sequence
/// space.
pub(crate) fn sequence_lte(a: u32, b: u32) -> bool {
    !sequence_gt(a, b)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::{sequence_gt, sequence_lte};

    proptest! {
        #[test]
        fn sequence_gt_is_irreflexive(a in any::<u32>()) {
            prop_assert!(!sequence_gt(a, a));
        }

        #[test]
        fn sequence_gt_is_total_outside_the_antipode(a in any::<u32>(), b in any::<u32>()) {
            prop_assume!(a != b);
            prop_assume!(a.wrapping_sub(b) != 0x8000_0000);
            prop_assert_ne!(sequence_gt(a, b), sequence_gt(b, a));
        }

        #[test]
        fn antipodes_are_not_strictly_ordered(a in any::<u32>()) {
            let b = a.wrapping_add(0x8000_0000);
            prop_assert!(!sequence_gt(a, b));
            prop_assert!(!sequence_gt(b, a));
            prop_assert!(sequence_lte(a, b));
            prop_assert!(sequence_lte(b, a));
        }

        #[test]
        fn adjacent_values_remain_ordered_across_wrap(base in any::<u32>()) {
            prop_assert!(sequence_gt(base.wrapping_add(1), base));
        }

        #[test]
        fn sequence_gt_is_transitive_inside_a_quarter_space_window(
            base in any::<u32>(),
            first in 0u32..0x2000_0000,
            second in 0u32..0x2000_0000,
        ) {
            let c = base;
            let b = c.wrapping_add(first);
            let a = b.wrapping_add(second);

            prop_assert!(a.wrapping_sub(c) < 0x4000_0000);
            if sequence_gt(a, b) && sequence_gt(b, c) {
                prop_assert!(sequence_gt(a, c));
            }
        }

        #[test]
        fn sequence_lte_is_the_negation_of_sequence_gt(a in any::<u32>(), b in any::<u32>()) {
            prop_assert_eq!(sequence_lte(a, b), !sequence_gt(a, b));
        }
    }

    #[test]
    fn zero_is_after_u32_max() {
        assert!(sequence_gt(0, u32::MAX));
    }
}
