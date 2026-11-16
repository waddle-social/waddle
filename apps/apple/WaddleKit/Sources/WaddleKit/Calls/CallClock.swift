/// The in-call clock, as Android shows it: `m:ss`, then `h:mm:ss` from the
/// first hour. A started second is not counted until it is over.
public enum CallClock {
    public static func string(for elapsed: Duration) -> String {
        let total = max(0, elapsed.components.seconds)
        let (hours, minutes, seconds) = (total / 3600, total % 3600 / 60, total % 60)
        return hours > 0 ? "\(hours):\(padded(minutes)):\(padded(seconds))" : "\(minutes):\(padded(seconds))"
    }

    private static func padded(_ value: Int64) -> String {
        value < 10 ? "0\(value)" : "\(value)"
    }
}
