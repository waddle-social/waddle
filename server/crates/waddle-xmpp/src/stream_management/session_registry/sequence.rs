/// Check if sequence a > b, handling wrap-around.
pub(super) fn sequence_gt(a: u32, b: u32) -> bool {
    if a == b {
        return false;
    }
    let diff = a.wrapping_sub(b);
    diff < 0x8000_0000
}
