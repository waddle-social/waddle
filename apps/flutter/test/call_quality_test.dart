import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:flutter_webrtc/flutter_webrtc.dart' as rtc;
import 'package:waddle_flutter/bridge.dart';
import 'package:waddle_flutter/features/call_quality_sheet.dart';
import 'package:waddle_flutter/runtime/call_quality.dart';

void main() {
  test(
    'RTP stats use the stream counter delta, matching codec and remote receiver feedback',
    () {
      final quality = CallQuality();
      List<Map<String, dynamic>> sample(int bytes, int seconds) =>
          quality.describe(
            [
              rtc.StatsReport('tx', 'outbound-rtp', 0, {
                'bytesSent': bytes,
                'codecId': 'codec',
                'remoteId': 'feedback',
                'frameWidth': 1280,
                'frameHeight': 720,
              }),
              rtc.StatsReport('codec', 'codec', 0, {'mimeType': 'video/VP9'}),
              rtc.StatsReport('feedback', 'remote-inbound-rtp', 0, {
                'packetsLost': 3,
              }),
            ],
            local: true,
            who: 'You',
            sid: 'track1',
            source: 'camera',
            time: Duration(seconds: seconds),
          );
      expect(sample(100, 1).single.containsKey('kbps'), false);
      final row = sample(125100, 2).single;
      expect(row['kbps'], 1000);
      expect(row['packetsLost'], 3);
      expect(row['codec'], 'video/VP9');
      expect(row['resolution'], '1280 × 720');
      expect(sample(12, 3).single.containsKey('kbps'), false);
    },
  );
  testWidgets(
    'voice settings target the current room and stop polling after a call change',
    (tester) async {
      var reads = 0;
      final changes = <Map<String, dynamic>>[];
      final bridge =
          WaddleBridge(
            events: const Stream.empty(),
            commandInvoker: (name, args) async {
              if (name == 'call.quality') {
                reads++;
                return {
                  'streams': [],
                  'voice': {
                    'echoCancellation': true,
                    'autoGainControl': true,
                    'noiseSuppression': true,
                  },
                  'canChangeVoice': true,
                };
              }
              changes.add(args);
            },
          )..acceptEvent({
            'topic': 'call',
            'generation': 1,
            'data': {'state': 'active', 'sid': 'alpha'},
          });
      addTearDown(bridge.dispose);
      await tester.pumpWidget(
        MaterialApp(
          home: Builder(
            builder: (context) => Scaffold(
              body: TextButton(
                onPressed: () => showCallQuality(context, bridge, 'alpha'),
                child: const Text('Show settings'),
              ),
            ),
          ),
        ),
      );
      await tester.tap(find.text('Show settings'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('Echo cancellation'));
      await tester.pumpAndSettle();
      expect(changes.single, {
        'expectedSid': 'alpha',
        'key': 'echoCancellation',
        'enabled': false,
      });
      bridge.acceptEvent({
        'topic': 'call',
        'generation': 2,
        'data': {'state': 'idle'},
      });
      await tester.pumpAndSettle();
      expect(find.text('Live media statistics'), findsNothing);
      final atEnd = reads;
      await tester.pump(const Duration(seconds: 2));
      expect(reads, atEnd);
    },
  );
}
