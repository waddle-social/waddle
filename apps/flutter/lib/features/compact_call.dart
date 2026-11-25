import 'package:flutter/material.dart';

import '../bridge.dart';

class CompactCall extends StatelessWidget {
  const CompactCall({super.key, required this.bridge, required this.child});
  final WaddleBridge bridge;
  final Widget child;

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: bridge,
    child: child,
    builder: (context, content) {
      final call = bridge.call;
      final compact = call['compact'] == true;
      return Stack(
        fit: StackFit.expand,
        children: [
          ExcludeFocus(
            excluding: compact,
            child: Offstage(offstage: compact, child: content),
          ),
          if (compact)
            Material(
              color: Colors.black,
              child: Stack(
                fit: StackFit.expand,
                children: [
                  bridge.callVideo(context),
                  Align(
                    alignment: Alignment.topRight,
                    child: IconButton(
                      autofocus: true,
                      tooltip: 'Expand call',
                      color: Colors.white,
                      icon: const Icon(Icons.open_in_full),
                      onPressed: () async {
                        try {
                          await bridge.invoke('call.compact', {
                            'expectedSid': call['sid'],
                            'enabled': false,
                          });
                        } catch (_) {
                          /* The desktop or system PiP controls can also expand the call. */
                        }
                      },
                    ),
                  ),
                ],
              ),
            ),
        ],
      );
    },
  );
}
