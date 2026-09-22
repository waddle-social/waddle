import SwiftUI

/// Destructive sign-out with a confirmation step.
struct AccountSignOutButton: View {
    @Environment(AppState.self) private var app
    @State private var isConfirming = false
    @State private var isSigningOut = false

    var body: some View {
        Button(role: .destructive) {
            isConfirming = true
        } label: {
            HStack {
                Label("Sign out", systemImage: "rectangle.portrait.and.arrow.right")
                if isSigningOut {
                    Spacer()
                    ProgressView().controlSize(.small)
                }
            }
        }
        .disabled(isSigningOut)
        .confirmationDialog("Sign out of Waddle?", isPresented: $isConfirming, titleVisibility: .visible) {
            Button("Sign out", role: .destructive, action: signOut)
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("You will stop receiving messages on this device until you sign in again.")
        }
    }

    private func signOut() {
        isSigningOut = true
        Task {
            await app.signOut()
            isSigningOut = false
        }
    }
}
