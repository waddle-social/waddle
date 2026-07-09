use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

const PROCESS_START_TIME_METRIC: &str = "waddle_process_start_time_seconds";

/// Stable process-generation marker initialized on the first metrics
/// exposition. Fractional seconds make rapid same-pod restarts distinguishable
/// without exporting an instance, pod, or process identifier.
static PROCESS_START_TIME_SECONDS: OnceLock<f64> = OnceLock::new();

fn process_start_time_seconds() -> f64 {
    *PROCESS_START_TIME_SECONDS.get_or_init(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64()
    })
}

pub(super) fn render(out: &mut String) {
    render_value(out, process_start_time_seconds());
}

fn render_value(out: &mut String, value: f64) {
    out.push_str("# HELP ");
    out.push_str(PROCESS_START_TIME_METRIC);
    out.push_str(
        " Unix timestamp fixed at this process's first metrics exposition; a changed aggregate under a stable target count means a process generation changed.\n# TYPE ",
    );
    out.push_str(PROCESS_START_TIME_METRIC);
    out.push_str(" gauge\n");
    out.push_str(PROCESS_START_TIME_METRIC);
    out.push(' ');
    out.push_str(&value.to_string());
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_one_stable_unlabelled_process_marker() {
        let mut first = String::new();
        render(&mut first);
        let mut second = String::new();
        render(&mut second);

        assert_eq!(first, second);
        assert!(first.contains("# TYPE waddle_process_start_time_seconds gauge"));
        assert!(first.contains("\nwaddle_process_start_time_seconds "));
        assert!(!first.contains("waddle_process_start_time_seconds{"));
    }

    #[test]
    fn renderer_preserves_fractional_start_time() {
        let mut rendered = String::new();
        render_value(&mut rendered, 1_750_000_000.125);
        assert!(rendered.contains("waddle_process_start_time_seconds 1750000000.125"));
    }
}
