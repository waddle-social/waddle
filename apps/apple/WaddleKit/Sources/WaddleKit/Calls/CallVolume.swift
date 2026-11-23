/// The gain range used by the call mixer, with 1 as unity.
public enum CallVolume {
    public static func clamp(_ gain: Double) -> Double {
        gain.isFinite ? min(2, max(0, gain)) : 1
    }

    /// Snap to unity when dragging toward it from just outside the detent.
    /// An adjacent step from unity lets the person move away again.
    public static func gain(forPercent percent: Double, currentGain: Double = 1) -> Double {
        if percent == 99, currentGain >= 0.98, currentGain < 0.99 { return 1 }
        if percent == 101, currentGain > 1.01, currentGain <= 1.02 { return 1 }
        return clamp(percent / 100)
    }
}
