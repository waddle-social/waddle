//! Wall-clock time helpers shared across the server crate.

/// Current Unix time in milliseconds.
pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_ms_is_monotonic_within_call() {
        let a = now_ms();
        let b = now_ms();
        assert!(b >= a, "expected b >= a, got a={a} b={b}");
    }

    #[test]
    fn now_ms_is_in_the_present() {
        let value = now_ms();
        // Anything before 2020-01-01 or after 2100-01-01 is wrong.
        assert!(value > 1_577_836_800_000, "before 2020: {value}");
        assert!(value < 4_102_444_800_000, "after 2100: {value}");
    }
}
