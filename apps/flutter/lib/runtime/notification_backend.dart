typedef NotificationPlatformEvent = Map<String, dynamic>;

class NotificationNotice {
  const NotificationNotice({
    required this.id,
    required this.title,
    required this.body,
    required this.payload,
    this.sound = true,
    this.incomingCall = false,
  });
  final int id;
  final String title, body;
  final Map<String, dynamic> payload;
  final bool sound;
  final bool incomingCall;
}

abstract interface class NotificationBackend {
  bool get supported;
  bool get supportsBackground;
  Future<void> initialize(void Function(NotificationPlatformEvent) emit);
  Future<void> account(
    String? owner,
    String? instance, {
    required bool enabled,
    required bool visible,
    bool sound = true,
  });
  Future<bool> requestPermission();
  Future<bool> ensurePermission({bool requestOnce = false});
  Future<List<String>> distributors();
  Future<String?> distributor();
  Future<void> selectDistributor(String value);
  Future<void> subscribe(String instance, String vapid);
  Future<void> unsubscribe(String instance);
  Future<void> show(NotificationNotice notice);
  Future<void> clear();
  Future<void> cancel(int id);
  Future<void> settings();
  Future<bool> background(bool enabled, {bool requestPermission = false});
  Future<void> callForeground(bool active, {bool video = false});
  Future<void> dispose();
}
