import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import 'bridge.dart';
import 'design.dart';
import 'features/conversation.dart';
import 'features/login.dart';
import 'features/session_landing.dart';
import 'features/surfaces.dart';
import 'features/threads.dart';
import 'features/presence_input.dart';
import 'features/trip_plan.dart';
import 'features/community.dart';
import 'features/remote_communities.dart';
import 'features/calendar_occurrences.dart';

part 'features/account_sidebar.dart';
part 'features/account_photos.dart';
part 'features/home_dashboard.dart';
part 'features/home_context.dart';
part 'features/quick_jump.dart';
part 'features/room_directory.dart';

List<Map<String, dynamic>> records(dynamic value) => value is List
    ? value
          .whereType<Map>()
          .map((row) => Map<String, dynamic>.from(row))
          .toList()
    : const [];

Future<void> perform(
  BuildContext context,
  Future<dynamic> Function() action,
) async {
  try {
    await action();
  } catch (error) {
    if (context.mounted) {
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text(WaddleBridge.describeError(error))),
      );
    }
  }
}

String _accountScope(WaddleBridge bridge) =>
    'account:${bridge.session['serverOrigin'] ?? ''}/${bridge.session['jid']}';

class WaddleShell extends StatelessWidget {
  const WaddleShell({super.key, required this.bridge});
  final WaddleBridge bridge;

  @override
  Widget build(BuildContext context) => AnimatedBuilder(
    animation: bridge,
    builder: (context, _) => switch (bridge.session['state']) {
      'ready' => PresenceInput(
        key: ValueKey(_accountScope(bridge)),
        bridge: bridge,
        child: _AccountNavigator(bridge: bridge),
      ),
      'signedOut' => LoginScreen(bridge: bridge),
      'error' => SessionLanding(
        title: 'Couldn’t restore your session',
        message:
            '${bridge.session['message'] ?? 'Check your connection and try again.'}',
        onRetry: () => perform(context, () => bridge.invoke('session.retry')),
        onSignOut: () =>
            perform(context, () => bridge.invoke('session.logout')),
      ),
      _ => const SessionLanding(title: 'Checking session.'),
    },
  );
}

class _AccountNavigator extends StatefulWidget {
  const _AccountNavigator({required this.bridge});
  final WaddleBridge bridge;
  @override
  State<_AccountNavigator> createState() => _AccountNavigatorState();
}

class _AccountNavigatorState extends State<_AccountNavigator> {
  final _navigator = GlobalKey<NavigatorState>();
  late var _photos = _AccountPhotos(widget.bridge);
  bool _wasOffline = false;
  late final _observer = _AccountObserver(_routeChanged);
  String _page = '/';
  String? _selectedJid;
  late final StreamSubscription<Map<String, dynamic>> _navigation;
  String? _presentedCall;
  bool _scheduled = false;

  @override
  void initState() {
    super.initState();
    widget.bridge.addListener(_checkCall);
    _navigation = widget.bridge.navigation.listen(_notification);
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (mounted) {
        _notification(widget.bridge.topic('navigate'));
      }
    });
    _checkCall();
  }

  @override
  void dispose() {
    _photos.active = false;
    widget.bridge.removeListener(_checkCall);
    unawaited(_navigation.cancel());
    super.dispose();
  }

  void _checkCall() {
    final call = widget.bridge.call;
    final sid = call['sid'] as String?;
    if (_scheduled ||
        sid == null ||
        sid == _presentedCall ||
        ![
          'incoming',
          'outgoing',
          'connecting',
          'active',
        ].contains(call['state'])) {
      return;
    }
    _scheduled = true;
    WidgetsBinding.instance.addPostFrameCallback((_) {
      _scheduled = false;
      if (!mounted) {
        return;
      }
      final current = widget.bridge.call;
      final latestSid = current['sid'] as String?;
      if (latestSid == null ||
          latestSid == _presentedCall ||
          ![
            'incoming',
            'outgoing',
            'connecting',
            'active',
          ].contains(current['state'])) {
        return;
      }
      _presentedCall = latestSid;
      if (!_observer.showingCall) {
        // Keep conversations above a previous call when a successor rings.
        for (final route in _observer.callRoutes) {
          _navigator.currentState?.removeRoute(route);
        }
        _navigator.currentState?.restorablePushNamed(
          'call',
          arguments: {'sid': latestSid},
        );
      }
    });
    WidgetsBinding.instance.ensureVisualUpdate();
  }

  void _open(
    String jid,
    String name,
    bool group, {
    String? threadId,
    bool compose = false,
  }) {
    final top = _observer.top;
    final current = top?.settings.arguments;
    if (top?.settings.name == 'conversation' &&
        current is Map &&
        current['jid'] == jid &&
        current['threadId'] == threadId) {
      return;
    }
    _navigator.currentState?.restorablePushNamed(
      'conversation',
      arguments: {
        'jid': jid,
        'title': name,
        'group': group,
        'threadId': ?threadId,
        if (compose) 'compose': true,
      },
    );
  }

  void _notification(Map<String, dynamic> event) {
    if (!mounted || event['jid'] is! String) {
      return;
    }
    _open(
      event['jid'] as String,
      '${event['title'] ?? event['jid']}',
      event['group'] == true,
      threadId: event['threadId'] as String?,
      compose: event['compose'] == true,
    );
    widget.bridge.consumeNavigation();
  }

  void _routeChanged() {
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted) return;
      final route = _observer.page;
      final name = route?.settings.name ?? '/';
      final args = route?.settings.arguments;
      final jid = name == 'conversation' && args is Map
          ? args['jid'] as String?
          : null;
      if (name != _page || jid != _selectedJid) {
        setState(() {
          _page = name;
          _selectedJid = jid;
        });
      }
    });
  }

  void _select(String name, {Map<String, dynamic>? arguments}) {
    final navigator = _navigator.currentState;
    if (navigator == null) return;
    if (name == '/') {
      navigator.popUntil((route) => route.isFirst);
      return;
    }
    final current = _observer.page;
    if (current?.settings.name == name &&
        (name != 'conversation' ||
            (current?.settings.arguments as Map?)?['jid'] ==
                    arguments?['jid'] &&
                (current?.settings.arguments as Map?)?['threadId'] == null)) {
      return;
    }
    navigator.restorablePushNamedAndRemoveUntil(
      name,
      (route) => route.isFirst,
      arguments: arguments,
    );
  }

  void _selectEntry(Map<String, dynamic> row, bool group) => _select(
    'conversation',
    arguments: {'jid': row['jid'], 'title': _entryName(row), 'group': group},
  );

  void _jump() async {
    final overlay = _navigator.currentState?.overlay;
    if (overlay == null) return;
    final entry = await showDialog<_JumpEntry>(
      context: overlay.context,
      useRootNavigator: false,
      builder: (_) => _QuickJump(bridge: widget.bridge),
    );
    if (!mounted || entry == null) return;
    if (entry.row case final row?) {
      _selectEntry(row, entry.group);
    } else {
      _select(entry.route);
    }
  }

  Widget _sidebar() => _AccountSidebar(
    bridge: widget.bridge,
    page: _page,
    jid: _selectedJid,
    select: _select,
    open: _selectEntry,
    jump: _jump,
  );

  Route<void> _route(RouteSettings settings) {
    final bridge = widget.bridge;
    final args = settings.arguments is Map
        ? Map<String, dynamic>.from(settings.arguments! as Map)
        : <String, dynamic>{};
    final Widget page = switch (settings.name) {
      'conversation' => ConversationScreen(
        bridge: bridge,
        jid: args['jid'] as String,
        title: args['title'] as String? ?? args['jid'] as String,
        group: args['group'] == true,
        threadId: args['threadId'] as String?,
        focusComposer: args['compose'] == true,
      ),
      'inbox' => Scaffold(
        appBar: AppBar(
          title: const Text('Inbox'),
          actions: [
            IconButton(
              tooltip: 'Threads',
              icon: const Icon(Icons.forum_outlined),
              onPressed: () =>
                  _navigator.currentState?.restorablePushNamed('threads'),
            ),
          ],
        ),
        body: ActivityScreen(bridge: bridge, onOpen: _open),
      ),
      'rooms' => _RoomDirectory(bridge: bridge, onOpen: _selectEntry),
      'threads' => ThreadsScreen(bridge: bridge, onOpen: _open),
      'settings' => SettingsScreen(bridge: bridge),
      'profile' => ProfileScreen(bridge: bridge, jid: args['jid'] as String?),
      'admin' => CommunityAdminScreen(bridge: bridge),
      'people' => PeopleScreen(
        bridge: bridge,
        onMessage: (jid, name) => _open(jid, name, false),
        onGroup: (jid, name) => _open(jid, name, true),
      ),
      'call' => ActiveCallScreen(bridge: bridge),
      'events' || 'feed' || 'stories' => CommunityScreen(
        bridge: bridge,
        section: CommunitySection.values.byName(settings.name!),
      ),
      'trips' => TripPlansScreen(bridge: bridge),
      'remote' => RemoteCommunitiesScreen(bridge: bridge),
      _ => Builder(
        builder: (context) =>
            MediaQuery.sizeOf(context).width < _sidebarBreakpoint
            ? _sidebar()
            : _HomeDashboard(
                bridge: bridge,
                select: _select,
                open: _selectEntry,
              ),
      ),
    };
    return MaterialPageRoute<void>(settings: settings, builder: (_) => page);
  }

  @override
  Widget build(BuildContext context) {
    final wide = MediaQuery.sizeOf(context).width >= _sidebarBreakpoint;
    final connection = widget.bridge.topic('connection')['state'];
    if (!_photos._current || (connection == 'ready' && _wasOffline)) {
      _photos.active = false;
      _photos = _AccountPhotos(widget.bridge);
    }
    _wasOffline = connection != null && connection != 'ready';
    return _PhotoScope(
      photos: _photos,
      child: CallbackShortcuts(
        bindings: {
          const SingleActivator(LogicalKeyboardKey.keyK, control: true): _jump,
          const SingleActivator(LogicalKeyboardKey.keyK, meta: true): _jump,
        },
        child: Scaffold(
          body: SafeArea(
            child: Column(
              children: [
                if (connection != null &&
                    connection != 'ready' &&
                    connection != 'paused')
                  MaterialBanner(
                    content: Text(
                      connection == 'offline'
                          ? 'You’re offline. Messages will send when you reconnect.'
                          : 'Connecting to your community…',
                    ),
                    actions: [
                      TextButton(
                        onPressed: () => perform(
                          context,
                          () => widget.bridge.invoke('session.retry'),
                        ),
                        child: const Text('Retry'),
                      ),
                    ],
                  ),
                Expanded(
                  child: Row(
                    children: [
                      if (wide && _page != 'call') ...[
                        SizedBox(width: 280, child: _sidebar()),
                        const VerticalDivider(width: 1, thickness: 1),
                      ],
                      Expanded(
                        key: const ValueKey('account-pages'),
                        child: NavigatorPopHandler<void>(
                          onPopWithResult: (_) =>
                              _navigator.currentState?.pop(),
                          child: Navigator(
                            key: _navigator,
                            restorationScopeId: _accountScope(widget.bridge),
                            observers: [waddleRoutes, _observer],
                            onGenerateRoute: _route,
                          ),
                        ),
                      ),
                    ],
                  ),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

class _AccountObserver extends NavigatorObserver {
  _AccountObserver(this.changed);
  final VoidCallback changed;
  Route<dynamic>? get page =>
      _routes.whereType<PageRoute<dynamic>>().lastOrNull;
  @override
  void didChangeTop(
    Route<dynamic> topRoute,
    Route<dynamic>? previousTopRoute,
  ) => changed();
  final _routes = <Route<dynamic>>[];
  Route<dynamic>? get top => _routes.isEmpty ? null : _routes.last;
  bool get showingCall =>
      _routes.isNotEmpty && _routes.last.settings.name == 'call';
  List<Route<dynamic>> get callRoutes =>
      _routes.where((route) => route.settings.name == 'call').toList();
  @override
  void didPush(Route<dynamic> route, Route<dynamic>? previousRoute) {
    _routes.add(route);
  }

  @override
  void didPop(Route<dynamic> route, Route<dynamic>? previousRoute) {
    _routes.remove(route);
  }

  @override
  void didRemove(Route<dynamic> route, Route<dynamic>? previousRoute) {
    _routes.remove(route);
  }

  @override
  void didReplace({Route<dynamic>? newRoute, Route<dynamic>? oldRoute}) {
    _routes.remove(oldRoute);
    if (newRoute != null) {
      _routes.add(newRoute);
    }
  }
}

class _CreateRoomDialog extends StatefulWidget {
  const _CreateRoomDialog();

  @override
  State<_CreateRoomDialog> createState() => _CreateRoomDialogState();
}

class _CreateRoomDialogState extends State<_CreateRoomDialog> {
  final _name = TextEditingController();
  final _description = TextEditingController();
  bool _forum = false;

  @override
  void dispose() {
    _name.dispose();
    _description.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => AlertDialog(
    title: const Text('New room'),
    content: SingleChildScrollView(
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          TextField(
            controller: _name,
            autofocus: true,
            decoration: const InputDecoration(labelText: 'Room name'),
          ),
          const SizedBox(height: 16),
          TextField(
            controller: _description,
            decoration: const InputDecoration(labelText: 'Description'),
            maxLines: 3,
          ),
          SwitchListTile.adaptive(
            title: const Text('Forum'),
            subtitle: const Text('Organize conversations into threads'),
            value: _forum,
            onChanged: (value) => setState(() => _forum = value),
          ),
        ],
      ),
    ),
    actions: [
      TextButton(
        onPressed: () => Navigator.pop(context),
        child: const Text('Cancel'),
      ),
      FilledButton(
        onPressed: () {
          if (_name.text.trim().isNotEmpty) {
            Navigator.pop(context, {
              'name': _name.text.trim(),
              'description': _description.text.trim(),
              'forum': _forum,
            });
          }
        },
        child: const Text('Create'),
      ),
    ],
  );
}
