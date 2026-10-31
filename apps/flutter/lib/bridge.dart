import 'dart:async';
import 'dart:typed_data';

import 'package:flutter/widgets.dart';
import 'package:flutter/services.dart';

final waddleRoutes = RouteObserver<ModalRoute<void>>();

typedef CommandInvoker =
    Future<dynamic> Function(String method, Map<String, dynamic> arguments);

/// Presentation snapshots from the independent Dart/Rust application runtime.
class WaddleBridge extends ChangeNotifier {
  WaddleBridge({
    required CommandInvoker commandInvoker,
    required Stream<dynamic> events,
    VoidCallback? onDispose,
    Widget Function(BuildContext context, bool local)? callVideoBuilder,
  }) : _commandInvoker = commandInvoker,
       _onDispose = onDispose,
       _callVideoBuilder = callVideoBuilder {
    _subscription = events.listen(
      acceptEvent,
      onError: (Object error) {
        _error = describeError(error);
        notifyListeners();
      },
    );
  }

  final VoidCallback? _onDispose;
  final Widget Function(BuildContext context, bool local)? _callVideoBuilder;

  Widget callVideo(BuildContext context, {bool local = false}) =>
      _callVideoBuilder?.call(context, local) ?? const SizedBox.expand();
  final CommandInvoker _commandInvoker;
  late final StreamSubscription<dynamic> _subscription;
  final Map<String, Map<String, dynamic>> _topics = {};
  final Map<String, Map<String, dynamic>> _conversations = {};
  final StreamController<Map<String, dynamic>> _navigation =
      StreamController<Map<String, dynamic>>.broadcast();
  String? _error;
  bool _disposed = false;
  int _generation = -1;

  Map<String, dynamic> topic(String name) => _topics[name] ?? const {};
  Map<String, dynamic> get session => topic('session');
  Map<String, dynamic> get preferences => topic('preferences');
  Map<String, dynamic> get home => topic('home');
  Map<String, dynamic> get login => topic('login');
  Map<String, dynamic> get call => topic('call');
  Map<String, dynamic> conversation(String jid) =>
      _conversations[jid] ?? const {};
  Stream<Map<String, dynamic>> get navigation => _navigation.stream;
  String? get error => _error;

  /// Normalize platform and Rust boundary maps while preserving binary data.
  static dynamic normalize(dynamic value) {
    if (value is TypedData) {
      return value;
    }
    if (value is Map) {
      return value.map<String, dynamic>(
        (key, value) => MapEntry(key.toString(), normalize(value)),
      );
    }
    if (value is List) {
      return value.map(normalize).toList();
    }
    return value;
  }

  @visibleForTesting
  void acceptEvent(dynamic event) {
    if (_disposed || event is! Map) {
      return;
    }
    final envelope = normalize(event) as Map<String, dynamic>;
    final generation = envelope['generation'] as int?;
    if (generation != null) {
      if (generation < _generation) {
        return;
      }
      if (generation > _generation) {
        final appearance = _topics['preferences'];
        final palette = _topics['palette'];
        _topics.clear();
        if (appearance != null) {
          _topics['preferences'] = appearance;
        }
        if (palette != null) {
          _topics['palette'] = palette;
        }
        _conversations.clear();
        _generation = generation;
      }
    }
    final name = envelope['topic'];
    final raw = envelope['data'];
    final data = raw is Map<String, dynamic>
        ? raw
        : <String, dynamic>{'items': raw};
    if (name is! String) {
      return;
    }
    if (name == 'session' && data['state'] == 'signedOut') {
      final appearance = _topics['preferences'];
      final palette = _topics['palette'];
      _topics.clear();
      if (appearance != null) {
        _topics['preferences'] = appearance;
      }
      if (palette != null) {
        _topics['palette'] = palette;
      }
      _conversations.clear();
      _error = null;
    }
    if (name == 'navigate') {
      _topics[name] = data;
      _navigation.add(data);
    } else if (name == 'conversation' && data['jid'] is String) {
      _conversations[data['jid'] as String] = data;
    } else {
      _topics[name] = data;
    }
    if (name == 'error') {
      _error =
          data['message'] as String? ?? 'The action could not be completed.';
    }
    notifyListeners();
  }

  Future<dynamic> invoke(
    String method, [
    Map<String, dynamic> arguments = const {},
  ]) async {
    if (_disposed) {
      throw StateError('Waddle bridge is closed');
    }
    try {
      return normalize(await _commandInvoker(method, arguments));
    } catch (error) {
      _error = describeError(error);
      if (!_disposed) {
        notifyListeners();
      }
      rethrow;
    }
  }

  void dismissError() {
    _error = null;
    notifyListeners();
  }

  void consumeNavigation() {
    final jid = topic('navigate')['jid'];
    _topics.remove('navigate');
    unawaited(
      invoke('navigation.consume', {'jid': jid}).catchError((Object _) => null),
    );
  }

  static String describeError(Object error) {
    if (error is PlatformException) {
      return error.message ?? 'The action could not be completed. Try again.';
    }
    return error.toString();
  }

  @override
  void dispose() {
    if (_disposed) return;
    _disposed = true;
    _onDispose?.call();
    unawaited(_subscription.cancel());
    unawaited(_navigation.close());
    super.dispose();
  }
}
