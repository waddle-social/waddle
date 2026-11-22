import SwiftUI

/// Whether threads, details and pins open in the inspector (iPad at
/// regular width, and Mac) or push onto the phone stack. Mirrors the
/// shell choice in `RootView`. A screen inside the inspector (a thread
/// opening its sub-thread) stays there, whatever size class the
/// inspector column reports.
struct ConversationInspectorPlacement: DynamicProperty {
    #if os(iOS)
    @Environment(\.isInInspector) private var isInInspector
    @Environment(\.horizontalSizeClass) private var sizeClass
    #endif

    var usesInspector: Bool {
        #if os(iOS)
        return isInInspector || sizeClass == .regular
        #else
        return true
        #endif
    }
}

extension EnvironmentValues {
    /// Set on the split shell's inspector column.
    @Entry var isInInspector = false
}
