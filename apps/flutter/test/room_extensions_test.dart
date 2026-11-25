import 'package:flutter_test/flutter_test.dart';
import 'package:waddle_flutter/runtime/surfaces_runtime.dart';
import 'platform_test_context.dart';

void main() {
  test(
    'signed actions stay with the discovered service, room, and selected item',
    () async {
      final context = TestRuntimeContext();
      const room = 'lounge@muc.example.test';
      const service = 'extensions.example.test';
      const route = {
        'serviceJid': service,
        'pluginId': 'polls',
        'routeId': 'agenda',
        'scope': 'channel',
        'surface': 'list',
        'label': 'Agenda',
      };
      final launch = {
        'id': 'choose',
        'pluginId': 'polls',
        'actionId': 'vote',
        'commandNode': 'urn:waddle:extension:1:invoke',
        'label': 'Vote',
        'waddleId': 'community',
        'expiresAt': DateTime.now()
            .toUtc()
            .add(const Duration(minutes: 5))
            .toIso8601String(),
        'launchToken': 'fixture-signature',
        'roomJid': room,
      };
      context.handler = (method, args) async => switch (method) {
        'discover_extension_routes' => [route],
        'fetch_extension_route_items' => [
          {
            'id': 'item',
            'title': 'Lunch',
            'actions': [
              {'launch': launch},
              {
                'launch': {...launch, 'roomJid': 'other@muc.example.test'},
              },
            ],
          },
        ],
        'submit_extension_command_form' => {'status': 'completed'},
        _ => throw StateError(method),
      };
      final runtime = SurfaceRuntime(context);
      addTearDown(runtime.dispose);
      final views =
          await runtime.invoke('extensions.list', {'jid': room}) as List;
      final routeKey = views.single['key'];
      final request = {'jid': room, 'route': routeKey};
      final items = await runtime.invoke('extensions.items', request) as List;
      final action = (items.single['actions'] as List).single;
      expect(action.keys, unorderedEquals(['key', 'label']));
      final args = {
        ...request,
        'action': action['key'],
        'serviceJid': 'wrong.example.test',
      };
      await expectLater(
        runtime.invoke('extensions.invoke', {
          ...args,
          'jid': 'other@muc.example.test',
        }),
        throwsStateError,
      );
      await runtime.invoke('extensions.invoke', args);
      final rpc = context.calls.last['args'];
      expect(rpc['service_jid'], service);
      expect(rpc['room_jid'], room);
      expect(
        (rpc['fields'] as List).singleWhere(
          (row) => row['name'] == 'launch-token',
        )['values'],
        ['fixture-signature'],
      );
      await expectLater(
        runtime.invoke('extensions.invoke', args),
        throwsStateError,
      );
      expect(
        context.calls.where(
          (x) => x['method'] == 'submit_extension_command_form',
        ),
        hasLength(1),
      );
    },
  );
}
