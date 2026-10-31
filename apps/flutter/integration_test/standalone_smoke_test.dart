import 'dart:io';
import 'dart:ui';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:path_provider/path_provider.dart';
import 'package:waddle_flutter/bootstrap.dart';
import 'package:waddle_flutter/main.dart' as application;
import 'package:waddle_flutter/runtime/notification_owner.dart';
import 'package:waddle_flutter/runtime/secure_storage.dart';
import 'package:waddle_flutter/transport/native_bindings.dart';
import 'package:waddle_flutter/transport/transport.dart';
import 'package:waddle_flutter/transport/transport_native.dart';

const _ownerPort = 'social.waddle.flutter.account-owner';

Future<void> _pumpUntil(
  WidgetTester tester,
  bool Function() ready,
  String reason,
) async {
  for (var attempt = 0; attempt < 150; attempt++) {
    if (ready()) {
      return;
    }
    await tester.pump(const Duration(milliseconds: 100));
  }
  fail(reason);
}

Future<void> _exerciseBundledRust() async {
  // No override and no fake bindings: Android must load its packaged Rust .so.
  final transport = NativeRustTransport(FfiRustBindings.load());
  final events = <Map<String, dynamic>>[];
  final subscription = transport.events.listen(events.add);
  try {
    final parsed = await transport.rpc('parse_jid', {
      'input': 'alice@example.test/flutter-smoke',
    });
    expect(parsed, {
      'localpart': 'alice',
      'domain': 'example.test',
      'resource': 'flutter-smoke',
    });
    final initial = await transport.rpc('timeline_snapshot', {
      'conversation_jid': 'bob@example.test',
    });
    expect(initial['items'], isEmpty);
    final echo = await transport.rpc('timeline_local_echo', {
      'conversation_jid': 'bob@example.test',
      'account_jid': 'alice@example.test',
      'group': false,
      'origin_id': 'device-smoke-message',
      'body': 'Local smoke message; never transmitted',
      'timestamp_ms': 1800000000000,
    });
    final items = echo['items'] as List;
    expect(items, hasLength(1));
    expect(items.single['body'], 'Local smoke message; never transmitted');
    expect(items.single['own'], isTrue);
    // This validates typed client dispatch without connecting to any server.
    await expectLater(
      transport.rpc('send_chat_message', {
        'peer_jid': 'bob@example.test',
        'body': 'Must remain offline',
        'options': null,
      }),
      throwsA(
        isA<RustTransportException>().having(
          (error) => error.code,
          'code',
          'not_connected',
        ),
      ),
    );
    expect(events.where((event) => event['type'] == 'connected'), isEmpty);
  } finally {
    await subscription.cancel();
    await transport.close();
  }
  await expectLater(
    transport.rpc('parse_jid', {'input': 'alice@example.test'}),
    throwsA(
      isA<RustTransportException>().having(
        (error) => error.code,
        'code',
        'closed',
      ),
    ),
  );
}

Future<void> _captureMountedApp(WidgetTester tester) async {
  const token = String.fromEnvironment('WADDLE_SMOKE_CAPTURE_TOKEN');
  expect(
    token,
    matches(RegExp(r'^[0-9a-f]{32}$')),
    reason: 'Run this test through scripts/flutter-device-smoke.sh.',
  );
  final cache = await getTemporaryDirectory();
  final ready = File('${cache.path}/standalone-smoke.ready');
  final acknowledged = File('${cache.path}/standalone-smoke.ack');
  for (final marker in [ready, acknowledged]) {
    if (await marker.exists()) {
      await marker.delete();
    }
  }
  try {
    await tester.pump();
    await ready.writeAsString(token, flush: true);
    final deadline = DateTime.now().add(const Duration(seconds: 60));
    while (DateTime.now().isBefore(deadline)) {
      if (await acknowledged.exists() &&
          await acknowledged.readAsString() == token) {
        return;
      }
      await tester.pump(const Duration(milliseconds: 100));
    }
    fail(
      'The host did not acknowledge a verified ADB screenshot within 60 seconds.',
    );
  } finally {
    for (final marker in [ready, acknowledged]) {
      if (await marker.exists()) {
        await marker.delete();
      }
    }
  }
}

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  testWidgets('standalone Android bootstrap, ownership, secure storage and Rust', (
    tester,
  ) async {
    expect(
      Platform.isAndroid,
      isTrue,
      reason: 'Run on the dedicated Android emulator.',
    );
    const storage = SecureRuntimeStorage();
    final hasSavedAccount = (await storage.read('session'))?.isNotEmpty == true;
    expect(
      hasSavedAccount,
      isFalse,
      reason:
          'Use a clean emulator; this smoke test never restores a real account.',
    );
    final scratch = 'device-smoke.${DateTime.now().microsecondsSinceEpoch}';
    try {
      await storage.write(scratch, 'standalone-keystore-round-trip');
      expect(await storage.read(scratch), 'standalone-keystore-round-trip');
    } finally {
      await storage.write(scratch, null);
    }
    expect(await storage.read(scratch), isNull);

    await _exerciseBundledRust();
    await application.main(const []);
    await _pumpUntil(
      tester,
      () => find.byType(application.WaddleApp).evaluate().isNotEmpty,
      'The production Flutter application did not mount.',
    );
    expect(find.byType(MaterialApp), findsOneWidget);
    expect(find.byType(Scaffold), findsAtLeastNWidgets(1));
    final widget = tester.widget<application.WaddleApp>(
      find.byType(application.WaddleApp),
    );
    final bridge = widget.bridge;
    await _pumpUntil(
      tester,
      () => bridge.session['state'] == 'signedOut',
      'A clean emulator did not reach the signed-out production screen.',
    );
    expect(bridge.session['state'], 'signedOut');
    expect(IsolateNameServer.lookupPortByName(_ownerPort), isNotNull);

    // Exercise the real headless entry point while the UI already owns runtime.
    // The harmless event cannot cause an account restore or protocol mutation.
    await notificationBootstrap({'type': 'standalone_smoke'});
    expect(
      identical(
        tester
            .widget<application.WaddleApp>(find.byType(application.WaddleApp))
            .bridge,
        bridge,
      ),
      isTrue,
    );
    final acquiredSecondOwner = await NotificationOwner.acquire(
      headless: true,
      onEvent: (_) async {},
      onRetire: () async =>
          fail('A headless wake must not retire the UI owner.'),
    );
    expect(acquiredSecondOwner, isFalse);
    expect(tester.takeException(), isNull);

    // Keep the normal rendered app mounted until the host captures its display.
    await _captureMountedApp(tester);

    await tester.pumpWidget(const SizedBox.shrink());
    await _pumpUntil(
      tester,
      () => IsolateNameServer.lookupPortByName(_ownerPort) == null,
      'Disposing the production app did not release its runtime owner.',
    );
  }, timeout: const Timeout(Duration(minutes: 3)));
}
