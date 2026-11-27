import SwiftUI
import WaddleKit
import WebKit

#if os(macOS)
import AppKit

typealias LinkPreviewWebPlayerRepresentable = NSViewRepresentable
#else
import UIKit

typealias LinkPreviewWebPlayerRepresentable = UIViewRepresentable
#endif

/// The player owns an ephemeral WebKit session for the lifetime of the card's playback.
struct LinkPreviewWebPlayer: LinkPreviewWebPlayerRepresentable {
    let embed: LinkPreviewEmbed
    let onFailure: () -> Void

    func makeCoordinator() -> NavigationDelegate {
        NavigationDelegate(embed: embed, onFailure: onFailure)
    }

    #if os(macOS)
    func makeNSView(context: Context) -> WKWebView { makeWebView(delegate: context.coordinator) }
    func updateNSView(_ webView: WKWebView, context: Context) {}
    static func dismantleNSView(_ webView: WKWebView, coordinator: NavigationDelegate) { stop(webView) }
    #else
    func makeUIView(context: Context) -> WKWebView { makeWebView(delegate: context.coordinator) }
    func updateUIView(_ webView: WKWebView, context: Context) {}
    static func dismantleUIView(_ webView: WKWebView, coordinator: NavigationDelegate) { stop(webView) }
    #endif

    private func makeWebView(delegate: NavigationDelegate) -> WKWebView {
        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = .nonPersistent()
        configuration.preferences.javaScriptCanOpenWindowsAutomatically = false
        configuration.defaultWebpagePreferences.allowsContentJavaScript = true
        configuration.mediaTypesRequiringUserActionForPlayback = []
        #if os(iOS)
        configuration.allowsInlineMediaPlayback = true
        #endif

        let webView = WKWebView(frame: .zero, configuration: configuration)
        webView.navigationDelegate = delegate
        webView.allowsLinkPreview = false
        var request = URLRequest(url: embed.url)
        // YouTube uses the app's bundle identifier as the referrer for native embeds.
        if embed.provider == .youtube, let identifier = Bundle.main.bundleIdentifier {
            request.setValue("https://\(identifier)/", forHTTPHeaderField: "Referer")
        }
        delegate.load(request, in: webView)
        return webView
    }

    private static func stop(_ webView: WKWebView) {
        webView.navigationDelegate = nil
        webView.stopLoading()
        webView.pauseAllMediaPlayback(completionHandler: nil)
    }

    @MainActor
    final class NavigationDelegate: NSObject, WKNavigationDelegate {
        private var initialNavigation: WKNavigation?
        private var initialDocumentCommitted = false
        private var policy: LinkPreviewEmbedNavigation
        private let onFailure: () -> Void

        init(embed: LinkPreviewEmbed, onFailure: @escaping () -> Void) {
            policy = LinkPreviewEmbedNavigation(embed: embed)
            self.onFailure = onFailure
        }

        func load(_ request: URLRequest, in webView: WKWebView) {
            initialNavigation = webView.load(request)
            if initialNavigation == nil {
                // Let SwiftUI finish creating the view before switching to the fallback.
                Task { @MainActor [weak self, weak webView] in
                    guard let self, webView?.navigationDelegate === self else { return }
                    fail()
                }
            }
        }

        func webView(
            _ webView: WKWebView,
            decidePolicyFor action: WKNavigationAction,
            decisionHandler: @escaping @MainActor (WKNavigationActionPolicy) -> Void
        ) {
            let frame: LinkPreviewEmbedNavigation.Frame = if let target = action.targetFrame {
                target.isMainFrame ? .main : .subframe
            } else {
                .newWindow
            }
            let decision = policy.decide(
                url: action.request.url,
                frame: frame,
                isLink: action.navigationType == .linkActivated,
                hasTrustedButton: hasTrustedButton(action)
            )
            if decision == .allow {
                decisionHandler(.allow)
                return
            }
            failIfCancellingInitialDocument(frame)
            decisionHandler(.cancel)
            if case let .openExternally(url) = decision {
                // Bypass SwiftUI's environment action, which can route room links within Waddle.
                #if os(macOS)
                NSWorkspace.shared.open(url)
                #else
                UIApplication.shared.open(url)
                #endif
            }
        }

        func webView(
            _ webView: WKWebView,
            decidePolicyFor response: WKNavigationResponse,
            decisionHandler: @escaping @MainActor (WKNavigationResponsePolicy) -> Void
        ) {
            let frame: LinkPreviewEmbedNavigation.Frame = response.isForMainFrame ? .main : .subframe
            let status = (response.response as? HTTPURLResponse)?.statusCode
            guard policy.allowsResponse(url: response.response.url, frame: frame),
                  frame != .main || (response.canShowMIMEType && status != 204 && status != 205) else {
                failIfCancellingInitialDocument(frame)
                decisionHandler(.cancel)
                return
            }
            decisionHandler(.allow)
        }

        func webView(_ webView: WKWebView, didCommit navigation: WKNavigation!) {
            if let navigation, navigation === initialNavigation { initialDocumentCommitted = true }
        }

        func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
            if let navigation, navigation === initialNavigation { initialNavigation = nil }
        }

        func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
            failIfInitialNavigation(navigation)
        }

        func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
            failIfInitialNavigation(navigation)
        }

        func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
            fail()
        }

        private func hasTrustedButton(_ action: WKNavigationAction) -> Bool {
            // WebKit reports a button only for trusted mouse events. A link activation alone can come from script.
            #if os(macOS)
            return action.buttonNumber != 0
            #else
            if #available(iOS 18.4, *) { return !action.buttonNumber.isEmpty }
            return false
            #endif
        }

        private func failIfCancellingInitialDocument(_ frame: LinkPreviewEmbedNavigation.Frame) {
            // Before the first commit, a rejected main-frame policy can leave the view empty
            // without WebKit reporting a navigation error.
            if frame == .main, !initialDocumentCommitted, initialNavigation != nil { fail() }
        }

        private func failIfInitialNavigation(_ navigation: WKNavigation?) {
            if let initialNavigation, navigation == nil || navigation === initialNavigation { fail() }
        }

        private func fail() {
            initialNavigation = nil
            onFailure()
        }
    }
}
