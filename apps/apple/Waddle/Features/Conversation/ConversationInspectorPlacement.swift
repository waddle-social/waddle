import SwiftUI

/// Whether threads, details and pins open in the inspector (iPad at
/// regular width, and Mac) or push onto the phone stack. Mirrors the
/// shell choice in `RootView`.
struct ConversationInspectorPlacement: DynamicProperty {
    #if os(iOS)
    @Environment(\.horizontalSizeClass) private var sizeClass
    #endif

    var usesInspector: Bool {
        #if os(iOS)
        return sizeClass == .regular
        #else
        return true
        #endif
    }
}
