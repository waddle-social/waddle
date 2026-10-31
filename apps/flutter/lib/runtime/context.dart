import '../transport/transport.dart';

abstract interface class RuntimeContext {
  RustTransport? get transport;
  String? get jid;
  int get generation;
  Map<String, dynamic> get preferences;
  Map<String, dynamic> get session;
  Map<String, dynamic> get persistent;
  void emit(String topic, Map<String, dynamic> data);
  Future<dynamic> rpc(String method, [Map<String, dynamic> args = const {}]);
  Future<dynamic> platform(
    String method, [
    Map<String, dynamic> args = const {},
  ]);
  Future<void> persist();
  Future<void> refreshHome();
}

Map<String, dynamic> runtimeMap(dynamic value) => value is Map
    ? value.map((key, item) => MapEntry(key.toString(), item))
    : <String, dynamic>{};

List<Map<String, dynamic>> runtimeRows(dynamic value) => value is List
    ? value.whereType<Map>().map(runtimeMap).toList()
    : <Map<String, dynamic>>[];

String bareJid(String value) => value.split('/').first.toLowerCase();

class AccountChanged implements Exception {
  const AccountChanged();
  @override
  String toString() => 'The account changed. Please try again.';
}

void requireGeneration(RuntimeContext context, int captured) {
  if (context.generation != captured) {
    throw const AccountChanged();
  }
}
