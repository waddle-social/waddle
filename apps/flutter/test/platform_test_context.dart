import 'package:waddle_flutter/runtime/context.dart';
import 'package:waddle_flutter/transport/transport.dart';

class TestRuntimeContext implements RuntimeContext {
  @override
  int generation = 1;
  @override
  String? jid = 'alice@example.test';
  @override
  Map<String, dynamic> session = {
    'jid': 'alice@example.test',
    'username': 'Alice',
    'fullJid': 'alice@example.test/flutter-1',
    'resource': 'flutter-1',
  };
  @override
  Map<String, dynamic> persistent = {};
  @override
  Map<String, dynamic> preferences = {};
  @override
  RustTransport? transport;
  final calls = <Map<String, dynamic>>[];
  final emitted = <Map<String, dynamic>>[];
  Future<dynamic> Function(String, Map<String, dynamic>)? handler;
  @override
  Future<dynamic> rpc(
    String method, [
    Map<String, dynamic> args = const {},
  ]) async {
    calls.add({'method': method, 'args': args});
    return handler == null ? true : await handler!(method, args);
  }

  @override
  Future<dynamic> platform(
    String method, [
    Map<String, dynamic> args = const {},
  ]) async => null;
  @override
  void emit(String topic, Map<String, dynamic> data) {
    emitted.add({'topic': topic, 'data': data});
  }

  @override
  Future<void> persist() async {}
  @override
  Future<void> refreshHome() async {}
}
