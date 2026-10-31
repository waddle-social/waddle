import 'notification_backend.dart';

NotificationBackend createBackend() => UnsupportedNotifications();
Future<NotificationBackend> initializeForwarder() async => createBackend();

class UnsupportedNotifications implements NotificationBackend {
  @override
  bool get supported => false;
  @override
  bool get supportsBackground => false;
  @override
  Future<void> initialize(
    void Function(NotificationPlatformEvent) emit,
  ) async {}
  @override
  Future<void> account(
    String? owner,
    String? instance, {
    required bool enabled,
    required bool visible,
    bool sound = true,
  }) async {}
  @override
  Future<bool> requestPermission() async => false;
  @override
  Future<bool> ensurePermission({bool requestOnce = false}) async => false;
  @override
  Future<List<String>> distributors() async => [];
  @override
  Future<String?> distributor() async => null;
  @override
  Future<void> selectDistributor(String value) async {}
  @override
  Future<void> subscribe(String instance, String vapid) async {}
  @override
  Future<void> unsubscribe(String instance) async {}
  @override
  Future<void> show(NotificationNotice notice) async {}
  @override
  Future<void> clear() async {}
  @override
  Future<void> cancel(int id) async {}
  @override
  Future<void> settings() async {}
  @override
  Future<bool> background(
    bool enabled, {
    bool requestPermission = false,
  }) async => false;
  @override
  Future<void> dispose() async {}
  @override
  Future<void> callForeground(bool active, {bool video = false}) async {}
}
