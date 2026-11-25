part of 'surfaces.dart';

class _CallShortcuts extends StatefulWidget {
  const _CallShortcuts({
    required this.bridge,
    required this.sid,
    required this.active,
    required this.child,
  });
  final WaddleBridge bridge;
  final String sid;
  final bool active;
  final Widget child;
  @override
  State<_CallShortcuts> createState() => _CallShortcutsState();
}

class _CallShortcutsState extends State<_CallShortcuts>
    with WidgetsBindingObserver {
  final _scope = FocusNode(debugLabel: 'Call shortcuts');
  bool _held = false;
  void _release([_CallShortcuts? owner]) {
    if (!_held) return;
    _held = false;
    final call = owner ?? widget;
    unawaited(
      call.bridge
          .invoke('call.talk', {'expectedSid': call.sid, 'held': false})
          .then<void>((_) {}, onError: (Object _) {}),
    );
  }

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
  }

  @override
  void didUpdateWidget(_CallShortcuts oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.sid != widget.sid || !widget.active) _release(oldWidget);
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state != AppLifecycleState.resumed) _release();
  }

  @override
  void didChangeViewFocus(ViewFocusEvent event) {
    if (event.state == ViewFocusState.unfocused) _release();
  }

  @override
  void dispose() {
    _release();
    WidgetsBinding.instance.removeObserver(this);
    _scope.dispose();
    super.dispose();
  }

  KeyEventResult _key(FocusNode node, KeyEvent event) {
    final key = event.logicalKey, keyboard = HardwareKeyboard.instance;
    if (_held && key == LogicalKeyboardKey.space && event is KeyUpEvent) {
      _release();
      return KeyEventResult.handled;
    }
    if (!widget.active ||
        event is! KeyDownEvent ||
        keyboard.isControlPressed ||
        keyboard.isMetaPressed ||
        keyboard.isAltPressed ||
        FocusManager.instance.primaryFocus?.context
                ?.findAncestorWidgetOfExactType<EditableText>() !=
            null) {
      return KeyEventResult.ignored;
    }
    if (key == LogicalKeyboardKey.space) {
      if (!_scope.hasPrimaryFocus ||
          widget.bridge.call['microphoneEnabled'] != false) {
        return KeyEventResult.ignored;
      }
      _held = true;
      unawaited(
        _command(context, widget.bridge, 'call.talk', {
          'expectedSid': widget.sid,
          'held': true,
        }),
      );
      return KeyEventResult.handled;
    }
    if (key == LogicalKeyboardKey.keyF) {
      _showFullCall(context, widget.bridge, widget.sid);
      return KeyEventResult.handled;
    }
    final method = switch (key) {
      LogicalKeyboardKey.keyM => 'microphone',
      LogicalKeyboardKey.keyV => 'camera',
      LogicalKeyboardKey.keyS
          when widget.bridge.call['screenShareSupported'] == true =>
        'screenShare',
      LogicalKeyboardKey.keyR when widget.bridge.call['group'] == true =>
        'hand',
      _ => null,
    };
    if (method == null) return KeyEventResult.ignored;
    unawaited(
      _command(context, widget.bridge, 'call.$method', {
        'expectedSid': widget.sid,
      }),
    );
    return KeyEventResult.handled;
  }

  @override
  Widget build(BuildContext context) => Focus(
    autofocus: true,
    focusNode: _scope,
    onFocusChange: (focused) {
      if (!focused) _release();
    },
    onKeyEvent: _key,
    child: widget.child,
  );
}

Future<void> _showFullCall(
  BuildContext context,
  WaddleBridge bridge,
  String sid,
) => showDialog<void>(
  context: context,
  useRootNavigator: false,
  builder: (dialog) => Dialog.fullscreen(
    backgroundColor: Colors.black,
    child: ListenableBuilder(
      listenable: bridge,
      builder: (dialog, _) {
        if (bridge.call['state'] != 'active' || bridge.call['sid'] != sid) {
          WidgetsBinding.instance.addPostFrameCallback((_) {
            if (dialog.mounted && ModalRoute.of(dialog)?.isCurrent == true) {
              Navigator.pop(dialog);
            }
          });
          return const SizedBox.shrink();
        }
        return Stack(
          fit: StackFit.expand,
          children: [
            bridge.callVideo(dialog),
            Align(
              alignment: Alignment.topLeft,
              child: SafeArea(
                child: IconButton(
                  tooltip: 'Exit full screen',
                  color: Colors.white,
                  icon: const Icon(Icons.fullscreen_exit),
                  onPressed: () => Navigator.pop(dialog),
                ),
              ),
            ),
          ],
        );
      },
    ),
  ),
);
