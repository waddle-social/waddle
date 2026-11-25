import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../bridge.dart';
import '../runtime/context.dart';

enum _SubscriptionAction { copy, revoke }

class CalendarSubscriptionMenu extends StatefulWidget {
  const CalendarSubscriptionMenu({super.key, required this.bridge});
  final WaddleBridge bridge;
  @override
  State<CalendarSubscriptionMenu> createState() => _CalendarSubscriptionState();
}

class _CalendarSubscriptionState extends State<CalendarSubscriptionMenu> {
  bool _busy = false;

  bool _current(int owner) => mounted && widget.bridge.generation == owner;
  void _notice(String message) {
    ScaffoldMessenger.of(
      context,
    ).showSnackBar(SnackBar(content: Text(message)));
  }

  Future<bool> _confirmRevocation() async =>
      await showDialog<bool>(
        context: context,
        useRootNavigator: false,
        builder: (dialog) => AlertDialog(
          title: const Text('Revoke calendar link?'),
          content: const Text(
            'Calendar apps using this link will stop receiving updates.',
          ),
          actions: [
            TextButton(
              onPressed: () => Navigator.pop(dialog, false),
              child: const Text('Cancel'),
            ),
            FilledButton(
              onPressed: () => Navigator.pop(dialog, true),
              child: const Text('Revoke'),
            ),
          ],
        ),
      ) ??
      false;

  Future<void> _copy(String url, int owner) async {
    final bridge = widget.bridge;
    final navigator = Navigator.of(context);
    final route = DialogRoute<bool>(
      context: context,
      builder: (dialog) => AlertDialog(
        title: const Text('Copy private calendar link'),
        content: const Text(
          'Paste this link into your calendar app to receive updates. Anyone with the link can read the feed.',
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(dialog, false),
            child: const Text('Cancel'),
          ),
          FilledButton(
            onPressed: () async {
              if (!_current(owner)) return;
              try {
                await Clipboard.setData(ClipboardData(text: url));
                if (dialog.mounted) Navigator.pop(dialog, true);
              } catch (_) {
                if (dialog.mounted) Navigator.pop(dialog, false);
                if (_current(owner)) _notice('The clipboard is unavailable.');
              }
            },
            child: const Text('Copy link'),
          ),
        ],
      ),
    );
    void discardOnAccountChange() {
      if (bridge.generation != owner && route.isActive) {
        Future.microtask(() {
          if (route.isActive && navigator.mounted) navigator.removeRoute(route);
        });
      }
    }

    bridge.addListener(discardOnAccountChange);
    try {
      final copied = await navigator.push(route);
      if (copied == true && _current(owner)) {
        _notice('Private calendar link copied.');
      }
    } finally {
      bridge.removeListener(discardOnAccountChange);
    }
  }

  Future<void> _run(_SubscriptionAction action) async {
    if (_busy) return;
    final owner = widget.bridge.generation;
    setState(() => _busy = true);
    try {
      final revoke = action == _SubscriptionAction.revoke;
      if (revoke && !await _confirmRevocation()) return;
      if (!_current(owner)) return;
      final reply = runtimeMap(
        await widget.bridge.invokeQuiet(
          revoke ? 'calendar.revokeSubscription' : 'calendar.issueSubscription',
        ),
      );
      if (!_current(owner)) return;
      if (reply['status'] == 'unsupported') {
        _notice(
          'This server does not offer live calendar subscriptions. You can still export individual events.',
        );
      } else if (revoke && reply['status'] == 'revoked') {
        _notice('Calendar link revoked.');
      } else if (!revoke &&
          reply['status'] == 'available' &&
          reply['url'] is String) {
        await _copy(reply['url'] as String, owner);
      } else {
        _notice('The server returned an invalid calendar response.');
      }
    } catch (_) {
      if (_current(owner)) {
        _notice('Could not change the calendar subscription. Try again.');
      }
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) => PopupMenuButton<_SubscriptionAction>(
    tooltip: 'Calendar subscription',
    enabled: !_busy,
    onSelected: _run,
    icon: const Icon(Icons.calendar_month_outlined),
    itemBuilder: (_) => const [
      PopupMenuItem(
        value: _SubscriptionAction.copy,
        child: Text('Copy private subscription link'),
      ),
      PopupMenuItem(
        value: _SubscriptionAction.revoke,
        child: Text('Revoke subscription link'),
      ),
    ],
  );
}
