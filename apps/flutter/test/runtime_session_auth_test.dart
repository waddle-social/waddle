import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:waddle_flutter/runtime/session_auth.dart';

void main() {
  test(
    'device auth uses existing REST endpoints and canonical snake-case bodies',
    () async {
      final requests = <http.Request>[];
      final client = MockClient((request) async {
        requests.add(request);
        return switch (request.url.path) {
          '/api/auth/providers' => http.Response(
            '[{"id":"community","kind":"oidc"}]',
            200,
          ),
          '/api/auth/device/start' => http.Response(
            '{"device_code":"private-device","user_code":"PUBLIC-CODE"}',
            200,
          ),
          '/api/auth/device/poll' => http.Response(
            '{"status":"complete","session_id":"private-session"}',
            200,
          ),
          '/api/auth/session' => http.Response(
            '{"session_id":"private-session","jid":"me@example.org","is_expired":false}',
            200,
          ),
          '/api/auth/logout' => http.Response('', 204),
          _ => http.Response('', 404),
        };
      });
      final gateway = HttpAuthGateway(
        baseUri: Uri.parse('https://xmpp.example.org'),
        client: client,
      );
      addTearDown(gateway.close);
      expect((await gateway.providers()).single['id'], 'community');
      await gateway.startDevice('community');
      await gateway.pollDevice('private-device');
      expect(
        (await gateway.session('private-session'))?['jid'],
        'me@example.org',
      );
      await gateway.logout('private-session');
      expect(jsonDecode(requests[1].body), {'provider': 'community'});
      expect(jsonDecode(requests[2].body), {'device_code': 'private-device'});
      expect(requests[3].url.queryParameters, {
        'session_id': 'private-session',
      });
      expect(jsonDecode(requests[4].body), {'session_id': 'private-session'});
      expect(requests.map((request) => request.method), [
        'GET',
        'POST',
        'POST',
        'GET',
        'POST',
      ]);
    },
  );

  test(
    'expired sessions clear cleanly and server details never leak through auth failures',
    () async {
      final gateway = HttpAuthGateway(
        baseUri: Uri.parse('https://xmpp.example.org'),
        client: MockClient(
          (request) async => request.url.path.endsWith('session')
              ? http.Response('credential expired', 401)
              : http.Response('private-device-code and private-token', 503),
        ),
      );
      addTearDown(gateway.close);
      expect(await gateway.session('private-token'), isNull);
      try {
        await gateway.startDevice('community');
        fail(
          'A server failure must not become an empty successful auth response.',
        );
      } on AuthFailure catch (failure) {
        expect(failure.transient, true);
        expect(failure.status, 503);
        expect(failure.toString(), isNot(contains('private-token')));
        expect(failure.toString(), isNot(contains('private-device-code')));
      }
    },
  );
}
