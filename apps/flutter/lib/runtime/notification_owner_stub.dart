typedef NotificationBootstrap =
    Future<void> Function(Map<String, dynamic> event);

/// Browser tabs use independent browser XMPP resources; Android engine ownership
/// uses the process-local implementation selected by the conditional export.
class NotificationOwner {
  static Future<bool> acquire({
    required bool headless,
    required Future<void> Function(Map<String, dynamic>) onEvent,
    required Future<void> Function() onRetire,
  }) async => true;
  static Future<void> configureBootstrap(
    NotificationBootstrap callback,
  ) async {}
  static Future<bool> deliver(Map<String, dynamic> event) async => false;
  static void release() {}
}
