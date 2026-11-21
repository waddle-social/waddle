import SwiftUI

/// A one-line field for a web address.
struct ProfileLinkField: View {
    @Binding var text: String

    var body: some View {
        TextField("Link", text: $text)
            .autocorrectionDisabled()
            #if os(iOS)
            .textInputAutocapitalization(.never)
            .keyboardType(.URL)
            #endif
    }
}
