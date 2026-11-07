import 'dart:async';
import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:waddle_flutter/runtime/context.dart';
import 'package:waddle_flutter/runtime/surfaces_runtime.dart';

import 'platform_test_context.dart';

const _production = 'https://xmpp.waddle.social';

http.Response _response() => http.Response(
  jsonEncode({
    'data': [
      {
        'id': 'penguin',
        'title': 'Penguin dance',
        'images': {
          'fixed_height_small': {'url': 'https://media.example/preview.gif'},
          'original': {'url': 'https://media.example/original.gif'},
        },
      },
      {'id': 'invalid', 'images': {}},
    ],
  }),
  200,
);

void main() {
  test(
    'native production search uses the chat proxy without credentials',
    () async {
      final context = TestRuntimeContext()
        ..session['serverOrigin'] = _production;
      final requests = <http.Request>[];
      final surface = SurfaceRuntime(
        context,
        isWeb: false,
        httpClient: MockClient((request) async {
          requests.add(request);
          return _response();
        }),
      );
      addTearDown(surface.dispose);

      final trending = await surface.invoke('gif.search', {'query': '  '});
      expect(
        requests.single.url.toString(),
        'https://waddle.chat/api/giphy?limit=24',
      );
      expect(requests.single.headers, isNot(contains('authorization')));
      expect(requests.single.headers, isNot(contains('cookie')));
      expect(trending, [
        {
          'id': 'penguin',
          'title': 'Penguin dance',
          'previewUrl': 'https://media.example/preview.gif',
          'originalUrl': 'https://media.example/original.gif',
        },
      ]);

      await surface.invoke('gif.search', {'query': '  cats & dogs 🐈  '});
      expect(requests.last.url.queryParameters, {
        'limit': '24',
        'q': 'cats & dogs 🐈',
      });
    },
  );

  test('native custom auth origins remain the default proxy', () async {
    final context = TestRuntimeContext()
      ..session['serverOrigin'] = 'https://community.example';
    Uri? requested;
    final surface = SurfaceRuntime(
      context,
      isWeb: false,
      httpClient: MockClient((request) async {
        requested = request.url;
        return _response();
      }),
    );
    addTearDown(surface.dispose);
    await surface.invoke('gif.search', {'query': 'hello'});
    expect(
      requested.toString(),
      'https://community.example/api/giphy?limit=24&q=hello',
    );
  });

  test('web uses its page origin by default', () async {
    final context = TestRuntimeContext()
      ..session['serverOrigin'] = 'https://community.example';
    Uri? requested;
    final surface = SurfaceRuntime(
      context,
      isWeb: true,
      webUri: Uri.parse('https://app.example/client/index.html?x=1'),
      httpClient: MockClient((request) async {
        requested = request.url;
        return _response();
      }),
    );
    addTearDown(surface.dispose);
    await surface.invoke('gif.search', {});
    expect(requested.toString(), 'https://app.example/api/giphy?limit=24');
  });

  test('explicit origin overrides the production and web defaults', () async {
    final context = TestRuntimeContext()..session['serverOrigin'] = _production;
    Uri? requested;
    final surface = SurfaceRuntime(
      context,
      isWeb: true,
      webUri: Uri.parse('https://app.example/'),
      gifProxyOrigin: 'https://gif.example/',
      httpClient: MockClient((request) async {
        requested = request.url;
        return _response();
      }),
    );
    addTearDown(surface.dispose);
    await surface.invoke('gif.search', {});
    expect(requested.toString(), 'https://gif.example/api/giphy?limit=24');
  });

  test(
    'uses the build-time origin when no constructor override is supplied',
    () async {
      const configured = String.fromEnvironment('WADDLE_GIF_PROXY_ORIGIN');
      final context = TestRuntimeContext()
        ..session['serverOrigin'] = _production;
      Uri? requested;
      final surface = SurfaceRuntime(
        context,
        isWeb: false,
        httpClient: MockClient((request) async {
          requested = request.url;
          return _response();
        }),
      );
      addTearDown(surface.dispose);
      await surface.invoke('gif.search', {});
      expect(
        requested?.origin,
        configured.isEmpty
            ? 'https://waddle.chat'
            : Uri.parse(configured).origin,
      );
    },
  );

  test(
    'rejects unsafe or malformed configured origins before any request',
    () async {
      for (final origin in [
        'https://user:secret@gif.example',
        'https://gif.example/path',
        'https://gif.example?key=secret',
        'https://gif.example/#fragment',
        'file:///tmp/gifs',
        'http://gif.example',
        'not-a-url',
      ]) {
        final context = TestRuntimeContext()
          ..session['serverOrigin'] = _production;
        var requests = 0;
        final surface = SurfaceRuntime(
          context,
          isWeb: false,
          gifProxyOrigin: origin,
          httpClient: MockClient((request) async {
            requests++;
            return _response();
          }),
        );
        await expectLater(
          surface.invoke('gif.search', {}),
          throwsA(
            isA<StateError>().having(
              (error) => error.message,
              'message',
              'GIF search is not configured for this account.',
            ),
          ),
        );
        expect(requests, 0, reason: origin);
        surface.dispose();
      }
    },
  );

  test(
    'maps proxy failures and invalid payloads to actionable errors',
    () async {
      final outcomes = <(int, String, String)>[
        (404, '', 'GIF search is not configured on this server.'),
        (503, '', 'GIF search is not configured on this server.'),
        (429, '', 'GIF search is busy. Please try again shortly.'),
        (502, '', 'GIF search is unavailable.'),
        (200, 'not json', 'The GIF search response was invalid.'),
        (200, '{}', 'The GIF search response was invalid.'),
      ];
      for (final (status, body, message) in outcomes) {
        final context = TestRuntimeContext()
          ..session['serverOrigin'] = _production;
        final surface = SurfaceRuntime(
          context,
          isWeb: false,
          httpClient: MockClient((_) async => http.Response(body, status)),
        );
        await expectLater(
          surface.invoke('gif.search', {}),
          throwsA(
            isA<StateError>().having(
              (error) => error.message,
              'message',
              message,
            ),
          ),
        );
        surface.dispose();
      }
    },
  );

  test('network failures are presented as unavailable', () async {
    final context = TestRuntimeContext()..session['serverOrigin'] = _production;
    final surface = SurfaceRuntime(
      context,
      isWeb: false,
      httpClient: MockClient(
        (_) async => throw http.ClientException('offline'),
      ),
    );
    addTearDown(surface.dispose);
    await expectLater(
      surface.invoke('gif.search', {}),
      throwsA(
        isA<StateError>().having(
          (error) => error.message,
          'message',
          'GIF search is unavailable.',
        ),
      ),
    );
  });

  testWidgets('a timed-out request is presented as unavailable', (
    tester,
  ) async {
    final context = TestRuntimeContext()..session['serverOrigin'] = _production;
    final response = Completer<http.Response>();
    final surface = SurfaceRuntime(
      context,
      isWeb: false,
      httpClient: MockClient((_) => response.future),
    );
    addTearDown(surface.dispose);
    final pending = surface.invoke('gif.search', {});
    final assertion = expectLater(
      pending,
      throwsA(
        isA<StateError>().having(
          (error) => error.message,
          'message',
          'GIF search is unavailable.',
        ),
      ),
    );
    await tester.pump();
    await tester.pump(const Duration(seconds: 16));
    await assertion;
  });

  for (final fails in [false, true]) {
    test(
      'account change fences a pending ${fails ? 'failed' : 'successful'} search',
      () async {
        final context = TestRuntimeContext()
          ..session['serverOrigin'] = _production;
        final response = Completer<http.Response>();
        final started = Completer<void>();
        final surface = SurfaceRuntime(
          context,
          isWeb: false,
          httpClient: MockClient((_) {
            started.complete();
            return response.future;
          }),
        );
        addTearDown(surface.dispose);
        final pending = surface.invoke('gif.search', {
          'query': 'private search',
        });
        final assertion = expectLater(pending, throwsA(isA<AccountChanged>()));
        await started.future;
        context.generation++;
        if (fails) {
          response.completeError(http.ClientException('offline'));
        } else {
          response.complete(_response());
        }
        await assertion;
      },
    );
  }
}
