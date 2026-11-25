import 'dart:async';

import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';

import '../bridge.dart';

class PresenceInput extends StatefulWidget {
  const PresenceInput({super.key, required this.bridge, required this.child});
  final WaddleBridge bridge;
  final Widget child;
  @override
  State<PresenceInput> createState() => _PresenceInputState();
}

class _PresenceInputState extends State<PresenceInput> {
  final _since = Stopwatch();
  void _active() {
    if (_since.isRunning && _since.elapsed < const Duration(seconds: 1)) return;
    _since
      ..reset()
      ..start();
    unawaited(
      widget.bridge
          .invokeQuiet('presence.active')
          .catchError((Object _) => null),
    );
  }

  bool _key(KeyEvent event) {
    if (event is KeyDownEvent || event is KeyRepeatEvent) _active();
    return false;
  }

  @override
  void initState() {
    super.initState();
    HardwareKeyboard.instance.addHandler(_key);
  }

  @override
  void dispose() {
    HardwareKeyboard.instance.removeHandler(_key);
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => Listener(
    behavior: HitTestBehavior.translucent,
    onPointerDown: (_) => _active(),
    onPointerMove: (_) => _active(),
    onPointerHover: (_) => _active(),
    onPointerSignal: (_) => _active(),
    child: widget.child,
  );
}
