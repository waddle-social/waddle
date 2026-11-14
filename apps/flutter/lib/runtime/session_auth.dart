import 'dart:convert';

import 'package:http/http.dart' as http;

import 'context.dart';

abstract interface class AuthGateway {
  Future<List<Map<String, dynamic>>> providers();
  Future<Map<String, dynamic>> startDevice(String provider);
  Future<Map<String, dynamic>> pollDevice(String deviceCode);
  Future<Map<String, dynamic>?> session(String sessionId);
  Future<void> logout(String sessionId);
}

abstract interface class ClosableAuthGateway {
  void close();
}

class AuthFailure implements Exception {
  const AuthFailure({this.status, this.transient = false});
  final int? status;
  final bool transient;
  @override
  String toString() => 'Sign-in could not be completed. Please try again.';
}

/// Authentication uses the same sanctioned device-flow endpoints as the other
/// Waddle clients. All application/protocol operations stay on XMPP.
class HttpAuthGateway implements AuthGateway, ClosableAuthGateway {
  static final defaultBaseUri = Uri.parse('https://xmpp.waddle.social');
  HttpAuthGateway({required Uri baseUri, http.Client? client})
    : _base = baseUri.replace(
        path: '${baseUri.path.replaceAll(RegExp(r'/+$'), '')}/',
        query: null,
        fragment: null,
      ),
      _client = client ?? http.Client() {
    if (!['https', 'http'].contains(baseUri.scheme) || baseUri.host.isEmpty) {
      throw ArgumentError('A valid authentication server URL is required');
    }
  }
  final Uri _base;
  final http.Client _client;
  Uri get baseUri => _base;

  Future<dynamic> _request(
    String method,
    String path, {
    Map<String, dynamic>? body,
    Map<String, String>? query,
    Set<int> accepted = const {},
  }) async {
    try {
      final uri = _base.resolve(path).replace(queryParameters: query);
      final response =
          await (method == 'GET'
                  ? _client.get(uri)
                  : _client.post(
                      uri,
                      headers: {'content-type': 'application/json'},
                      body: jsonEncode(body ?? {}),
                    ))
              .timeout(const Duration(seconds: 20));
      if (!(response.statusCode >= 200 && response.statusCode < 300) &&
          !accepted.contains(response.statusCode)) {
        throw AuthFailure(
          status: response.statusCode,
          transient: response.statusCode >= 500 || response.statusCode == 429,
        );
      }
      if (accepted.contains(response.statusCode) || response.body.isEmpty) {
        return null;
      }
      return jsonDecode(response.body);
    } on AuthFailure {
      rethrow;
    } on FormatException {
      throw const AuthFailure();
    } catch (_) {
      throw const AuthFailure(transient: true);
    }
  }

  @override
  Future<List<Map<String, dynamic>>> providers() async {
    final value = await _request('GET', 'api/auth/providers');
    if (value is! List) {
      throw const AuthFailure();
    }
    return runtimeRows(value);
  }

  @override
  Future<Map<String, dynamic>> startDevice(String provider) async => runtimeMap(
    await _request(
      'POST',
      'api/auth/device/start',
      body: {'provider': provider},
    ),
  );
  @override
  Future<Map<String, dynamic>> pollDevice(String deviceCode) async =>
      runtimeMap(
        await _request(
          'POST',
          'api/auth/device/poll',
          body: {'device_code': deviceCode},
        ),
      );
  @override
  Future<Map<String, dynamic>?> session(String sessionId) async {
    final value = await _request(
      'GET',
      'api/auth/session',
      query: {'session_id': sessionId},
      accepted: {401, 404},
    );
    return value == null ? null : runtimeMap(value);
  }

  @override
  Future<void> logout(String sessionId) async {
    await _request(
      'POST',
      'api/auth/logout',
      body: {'session_id': sessionId},
      accepted: {204, 401, 404},
    );
  }

  @override
  void close() => _client.close();
}
