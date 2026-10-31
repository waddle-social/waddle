import 'dart:async';

import 'package:flutter/material.dart';

import 'bridge.dart';
import 'features/conversation.dart';
import 'features/surfaces.dart';

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

class WaddleShell extends StatelessWidget {
  const WaddleShell({super.key, required this.bridge});
  final WaddleBridge bridge;

  @override
  Widget build(BuildContext context) => AnimatedBuilder(
    animation: bridge,
    builder: (context, _) => switch (bridge.session['state']) {
      'ready' => _AccountNavigator(
        key: ValueKey('account:${bridge.session['jid']}'),
        bridge: bridge,
      ),
      'signedOut' => _Login(bridge: bridge),
      'error' => Scaffold(
        body: Center(
          child: Padding(
            padding: const EdgeInsets.all(32),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              children: [
                const Icon(Icons.cloud_off_outlined, size: 48),
                const SizedBox(height: 24),
                Text(
                  'Couldn’t restore your session',
                  style: Theme.of(context).textTheme.headlineSmall,
                ),
                const SizedBox(height: 12),
                Text(
                  '${bridge.session['message'] ?? 'Check your connection and try again.'}',
                ),
                const SizedBox(height: 24),
                FilledButton.icon(
                  onPressed: () =>
                      perform(context, () => bridge.invoke('session.retry')),
                  icon: const Icon(Icons.refresh),
                  label: const Text('Try again'),
                ),
                TextButton(
                  onPressed: () =>
                      perform(context, () => bridge.invoke('session.logout')),
                  child: const Text('Sign out'),
                ),
              ],
            ),
          ),
        ),
      ),
      _ => const Scaffold(body: Center(child: CircularProgressIndicator())),
    },
  );
}

class _Login extends StatefulWidget {
  const _Login({required this.bridge});
  final WaddleBridge bridge;
  @override
  State<_Login> createState() => _LoginState();
}

class _LoginState extends State<_Login> {
  @override
  void initState() {
    super.initState();
    unawaited(
      widget.bridge.invoke('login.providers').catchError((Object _) => null),
    );
  }

  @override
  Widget build(BuildContext context) {
    final login = widget.bridge.login;
    final state = login['state'];
    final scheme = Theme.of(context).colorScheme;
    return Scaffold(
      body: SafeArea(
        child: Center(
          child: SingleChildScrollView(
            padding: const EdgeInsets.all(32),
            child: ConstrainedBox(
              constraints: const BoxConstraints(maxWidth: 420),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  Align(
                    child: CircleAvatar(
                      radius: 44,
                      backgroundColor: scheme.primaryContainer,
                      foregroundColor: scheme.onPrimaryContainer,
                      child: const Icon(Icons.forum_rounded, size: 44),
                    ),
                  ),
                  const SizedBox(height: 32),
                  Text(
                    'A place for your people.',
                    textAlign: TextAlign.center,
                    style: Theme.of(context).textTheme.headlineLarge,
                  ),
                  const SizedBox(height: 12),
                  Text(
                    'Welcome to Waddle',
                    textAlign: TextAlign.center,
                    style: Theme.of(context).textTheme.titleMedium,
                  ),
                  const SizedBox(height: 40),
                  if (state == 'verifying') ...[
                    const Text(
                      'Approve sign-in in your browser. Your code:',
                      textAlign: TextAlign.center,
                    ),
                    const SizedBox(height: 12),
                    SelectableText(
                      '${login['code'] ?? ''}',
                      textAlign: TextAlign.center,
                      style: Theme.of(context).textTheme.displaySmall,
                    ),
                    const SizedBox(height: 24),
                    const LinearProgressIndicator(),
                    TextButton(
                      onPressed: () => perform(
                        context,
                        () => widget.bridge.invoke('login.cancel'),
                      ),
                      child: const Text('Cancel'),
                    ),
                  ] else if (state == 'failed') ...[
                    Text(
                      '${login['error'] ?? 'Sign-in failed. Please try again.'}',
                      textAlign: TextAlign.center,
                    ),
                    const SizedBox(height: 16),
                    FilledButton(
                      onPressed: () => perform(
                        context,
                        () => widget.bridge.invoke('login.providers'),
                      ),
                      child: const Text('Try again'),
                    ),
                  ] else if (state == 'providers') ...[
                    for (final provider in records(login['providers']))
                      Padding(
                        padding: const EdgeInsets.only(bottom: 12),
                        child: FilledButton.icon(
                          style: FilledButton.styleFrom(
                            minimumSize: const Size.fromHeight(56),
                          ),
                          onPressed: () => perform(
                            context,
                            () => widget.bridge.invoke('login.select', {
                              'id': provider['id'],
                            }),
                          ),
                          icon: const Icon(Icons.login),
                          label: Text(
                            'Continue with ${provider['name'] ?? provider['id']}',
                          ),
                        ),
                      ),
                    if (records(login['providers']).isEmpty)
                      const Text(
                        'No sign-in providers are available. Contact your community administrator.',
                      ),
                  ] else ...[
                    const Center(child: CircularProgressIndicator()),
                    const SizedBox(height: 16),
                    Text(
                      state == 'completing'
                          ? 'Finishing sign-in…'
                          : 'Finding your community…',
                      textAlign: TextAlign.center,
                    ),
                  ],
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }
}

class _AccountNavigator extends StatefulWidget {
  const _AccountNavigator({super.key, required this.bridge});
  final WaddleBridge bridge;
  @override
  State<_AccountNavigator> createState() => _AccountNavigatorState();
}

class _AccountNavigatorState extends State<_AccountNavigator> {
  final _navigator = GlobalKey<NavigatorState>();
  final _observer = _AccountObserver();
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

  void _open(String jid, String name, bool group) {
    final top = _observer.top;
    final current = top?.settings.arguments;
    if (top?.settings.name == 'conversation' &&
        current is Map &&
        current['jid'] == jid &&
        current['threadId'] == null) {
      return;
    }
    _navigator.currentState?.restorablePushNamed(
      'conversation',
      arguments: {'jid': jid, 'title': name, 'group': group},
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
    );
    widget.bridge.consumeNavigation();
  }

  Route<void> _route(RouteSettings settings) {
    final bridge = widget.bridge;
    final arguments = settings.arguments is Map
        ? Map<String, dynamic>.from(settings.arguments! as Map)
        : <String, dynamic>{};
    final Widget page = switch (settings.name) {
      'conversation' => _ConversationPage(
        bridge: bridge,
        jid: arguments['jid'] as String,
        title: arguments['title'] as String? ?? arguments['jid'] as String,
        group: arguments['group'] == true,
        threadId: arguments['threadId'] as String?,
      ),
      'settings' => SettingsScreen(bridge: bridge),
      'profile' => ProfileScreen(
        bridge: bridge,
        jid: arguments['jid'] as String?,
      ),
      'admin' => CommunityAdminScreen(bridge: bridge),
      'people' => PeopleScreen(
        bridge: bridge,
        onMessage: (jid, name) => _open(jid, name, false),
        onGroup: (jid, name) => _open(jid, name, true),
      ),
      'call' => ActiveCallScreen(bridge: bridge),
      _ => _Community(bridge: bridge),
    };
    return MaterialPageRoute<void>(settings: settings, builder: (_) => page);
  }

  @override
  Widget build(BuildContext context) => NavigatorPopHandler<void>(
    onPopWithResult: (_) => _navigator.currentState?.pop(),
    child: Navigator(
      key: _navigator,
      restorationScopeId: 'account:${widget.bridge.session['jid']}',
      observers: [waddleRoutes, _observer],
      onGenerateRoute: _route,
    ),
  );
}

/// One conversation state moves between compact and list/detail layouts.
class _ConversationPage extends StatefulWidget {
  const _ConversationPage({
    required this.bridge,
    required this.jid,
    required this.title,
    required this.group,
    this.threadId,
  });
  final WaddleBridge bridge;
  final String jid;
  final String title;
  final bool group;
  final String? threadId;
  @override
  State<_ConversationPage> createState() => _ConversationPageState();
}

class _ConversationPageState extends State<_ConversationPage> {
  final _conversationKey = GlobalKey();
  late bool _direct = !widget.group;
  String _query = '';

  @override
  Widget build(BuildContext context) {
    final conversation = ConversationScreen(
      key: _conversationKey,
      bridge: widget.bridge,
      jid: widget.jid,
      title: widget.title,
      group: widget.group,
      threadId: widget.threadId,
    );
    if (MediaQuery.sizeOf(context).width < 840) {
      return conversation;
    }
    return Scaffold(
      body: SafeArea(
        child: Row(
          children: [
            SizedBox(
              width: 360,
              child: AnimatedBuilder(
                animation: widget.bridge,
                builder: (context, _) {
                  final home = widget.bridge.home;
                  final rooms = records(home['rooms']);
                  final rows =
                      (_direct
                              ? [
                                  ...records(home['dmPeers']),
                                  ...rooms.where(
                                    (room) => room['groupDm'] == true,
                                  ),
                                ]
                              : rooms
                                    .where((room) => room['groupDm'] != true)
                                    .toList())
                          .where(
                            (row) => '${row['name'] ?? row['jid']}'
                                .toLowerCase()
                                .contains(_query.toLowerCase()),
                          )
                          .toList();
                  return Column(
                    children: [
                      ListTile(
                        leading: IconButton(
                          tooltip: 'Back to community',
                          icon: const Icon(Icons.arrow_back),
                          onPressed: () => Navigator.maybePop(context),
                        ),
                        title: const Text('Conversations'),
                      ),
                      Padding(
                        padding: const EdgeInsets.fromLTRB(12, 4, 12, 12),
                        child: SegmentedButton<bool>(
                          segments: const [
                            ButtonSegment(
                              value: false,
                              icon: Icon(Icons.tag),
                              label: Text('Rooms'),
                            ),
                            ButtonSegment(
                              value: true,
                              icon: Icon(Icons.chat_bubble_outline),
                              label: Text('Messages'),
                            ),
                          ],
                          selected: {_direct},
                          onSelectionChanged: (value) =>
                              setState(() => _direct = value.single),
                        ),
                      ),
                      Padding(
                        padding: const EdgeInsets.fromLTRB(12, 0, 12, 12),
                        child: SearchBar(
                          hintText: 'Find a conversation',
                          leading: const Icon(Icons.search),
                          onChanged: (value) => setState(() => _query = value),
                        ),
                      ),
                      Expanded(
                        child: ListView.builder(
                          itemCount: rows.length,
                          itemBuilder: (context, index) {
                            final row = rows[index];
                            final jid = row['jid'] as String;
                            final group = !_direct || row['groupDm'] == true;
                            final name = '${row['name'] ?? jid}';
                            final unread =
                                (row['unread'] as num?)?.toInt() ?? 0;
                            return ListTile(
                              selected: jid == widget.jid,
                              leading: CircleAvatar(
                                child: group
                                    ? const Icon(Icons.tag)
                                    : Text(name.characters.first.toUpperCase()),
                              ),
                              title: Text(
                                name,
                                maxLines: 1,
                                overflow: TextOverflow.ellipsis,
                              ),
                              subtitle: Text(
                                '${row['preview'] ?? row['description'] ?? jid}',
                                maxLines: 1,
                                overflow: TextOverflow.ellipsis,
                              ),
                              trailing: unread > 0
                                  ? Badge(label: Text('$unread'))
                                  : null,
                              onTap: () {
                                if (jid == widget.jid &&
                                    widget.threadId == null) {
                                  return;
                                }
                                Navigator.of(
                                  context,
                                ).restorablePushReplacementNamed(
                                  'conversation',
                                  arguments: {
                                    'jid': jid,
                                    'title': name,
                                    'group': group,
                                  },
                                );
                              },
                            );
                          },
                        ),
                      ),
                    ],
                  );
                },
              ),
            ),
            const VerticalDivider(width: 1),
            Expanded(child: conversation),
          ],
        ),
      ),
    );
  }
}

class _AccountObserver extends NavigatorObserver {
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

class _Community extends StatefulWidget {
  const _Community({required this.bridge});
  final WaddleBridge bridge;
  @override
  State<_Community> createState() => _CommunityState();
}

class _CommunityState extends State<_Community> with RestorationMixin {
  final _tabIndex = RestorableInt(0);
  int get _tab => _tabIndex.value;
  set _tab(int value) => _tabIndex.value = value;
  String _query = '';
  WaddleBridge get bridge => widget.bridge;

  @override
  String get restorationId => 'community';

  @override
  void restoreState(RestorationBucket? oldBucket, bool initialRestore) {
    registerForRestoration(_tabIndex, 'tab');
  }

  @override
  void initState() {
    super.initState();
    bridge.addListener(_updated);
  }

  @override
  void dispose() {
    bridge.removeListener(_updated);
    _tabIndex.dispose();
    super.dispose();
  }

  void _updated() {
    if (mounted) {
      setState(() {});
    }
  }

  void _push(Widget screen) {
    final (name, arguments) = switch (screen) {
      ConversationScreen page => (
        'conversation',
        <String, dynamic>{
          'jid': page.jid,
          'title': page.title,
          'group': page.group,
          if (page.threadId != null) 'threadId': page.threadId,
        },
      ),
      ProfileScreen page => (
        'profile',
        <String, dynamic>{if (page.jid != null) 'jid': page.jid},
      ),
      SettingsScreen() => ('settings', <String, dynamic>{}),
      PeopleScreen() => ('people', <String, dynamic>{}),
      CommunityAdminScreen() => ('admin', <String, dynamic>{}),
      ActiveCallScreen() => (
        'call',
        <String, dynamic>{'sid': bridge.call['sid']},
      ),
      _ => throw ArgumentError('Unknown destination'),
    };
    Navigator.of(context).restorablePushNamed(name, arguments: arguments);
  }

  void _open(String jid, String title, bool group) {
    _push(
      ConversationScreen(bridge: bridge, jid: jid, title: title, group: group),
    );
  }

  Future<void> _createRoom() async {
    final result = await showDialog<Map<String, dynamic>>(
      context: context,
      builder: (_) => const _CreateRoomDialog(),
    );
    if (result == null || !mounted) {
      return;
    }
    await perform(context, () async {
      final created = await bridge.invoke('room.create', result);
      if (mounted && created is Map && created['jid'] is String) {
        _open(created['jid'] as String, result['name'] as String, true);
      }
    });
  }

  @override
  Widget build(BuildContext context) {
    final wide = MediaQuery.sizeOf(context).width >= 840;
    final home = bridge.home;
    final rooms = records(
      home['rooms'],
    ).where((r) => r['groupDm'] != true).toList();
    final peers = [
      ...records(home['dmPeers']),
      ...records(home['rooms']).where((r) => r['groupDm'] == true),
    ];
    final unread = peers.fold<int>(
      0,
      (sum, p) => sum + ((p['unread'] as num?)?.toInt() ?? 0),
    );
    final titles = ['Home', 'Messages', 'Activity', 'You'];
    final icons = [
      Icons.home_outlined,
      Icons.chat_bubble_outline,
      Icons.notifications_none,
      Icons.person_outline,
    ];
    final selectedIcons = [
      Icons.home,
      Icons.chat_bubble,
      Icons.notifications,
      Icons.person,
    ];
    final connection = bridge.topic('connection')['state'];
    final call = bridge.call['state'];
    final hasCall = call != null && call != 'idle' && call != 'ended';
    final destinations = List.generate(
      4,
      (index) => NavigationDestination(
        icon: index == 1 && unread > 0
            ? Badge(label: Text('$unread'), child: Icon(icons[index]))
            : Icon(icons[index]),
        selectedIcon: Icon(selectedIcons[index]),
        label: titles[index],
      ),
    );

    Widget content;
    if (_tab == 2) {
      content = ActivityScreen(bridge: bridge, onOpen: _open);
    } else if (_tab == 3) {
      content = _you(context);
    } else {
      final rows = (_tab == 0 ? rooms : peers)
          .where(
            (row) => '${row['name'] ?? row['jid']}'.toLowerCase().contains(
              _query.toLowerCase(),
            ),
          )
          .toList();
      content = CustomScrollView(
        slivers: [
          SliverToBoxAdapter(
            child: Padding(
              padding: const EdgeInsets.fromLTRB(16, 8, 16, 20),
              child: SearchBar(
                hintText: _tab == 0 ? 'Find a room' : 'Find a conversation',
                leading: const Icon(Icons.search),
                onChanged: (value) => setState(() => _query = value),
              ),
            ),
          ),
          if (_tab == 0 && _query.isEmpty)
            SliverToBoxAdapter(
              child: Padding(
                padding: const EdgeInsets.fromLTRB(20, 0, 20, 24),
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(
                      'Good to see you.',
                      style: Theme.of(context).textTheme.headlineMedium,
                    ),
                    const SizedBox(height: 6),
                    Text(
                      'Your people. Your conversations.',
                      style: Theme.of(context).textTheme.bodyLarge,
                    ),
                    const SizedBox(height: 20),
                    ActionChip(
                      avatar: const Icon(Icons.people_outline),
                      label: const Text('People'),
                      onPressed: () => _push(
                        PeopleScreen(
                          bridge: bridge,
                          onGroup: (jid, name) => _open(jid, name, true),
                          onMessage: (jid, name) => _open(jid, name, false),
                        ),
                      ),
                    ),
                  ],
                ),
              ),
            ),
          SliverToBoxAdapter(
            child: Padding(
              padding: const EdgeInsets.fromLTRB(20, 0, 20, 12),
              child: Text(
                _tab == 0 ? 'Your rooms' : 'Conversations',
                style: Theme.of(context).textTheme.titleMedium,
              ),
            ),
          ),
          if (rows.isEmpty && home['settled'] != true)
            const SliverToBoxAdapter(
              child: Padding(
                padding: EdgeInsets.all(32),
                child: Center(child: CircularProgressIndicator()),
              ),
            ),
          if (rows.isEmpty && home['settled'] == true)
            SliverFillRemaining(
              hasScrollBody: false,
              child: Center(
                child: Padding(
                  padding: const EdgeInsets.all(32),
                  child: Column(
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      Icon(
                        _tab == 0
                            ? Icons.meeting_room_outlined
                            : Icons.forum_outlined,
                        size: 48,
                      ),
                      const SizedBox(height: 16),
                      Text(
                        _query.isNotEmpty
                            ? 'No matches'
                            : _tab == 0
                            ? 'No rooms yet'
                            : 'Start a conversation',
                        style: Theme.of(context).textTheme.titleLarge,
                      ),
                      const SizedBox(height: 8),
                      Text(
                        _tab == 0
                            ? 'Rooms will appear here when your community is ready.'
                            : 'Find someone in your community to say hello.',
                      ),
                    ],
                  ),
                ),
              ),
            ),
          SliverList.builder(
            itemCount: rows.length,
            itemBuilder: (context, index) {
              final row = rows[index];
              final jid = '${row['jid']}';
              final name = '${row['name'] ?? jid.split('@').first}';
              final group = _tab == 0 || row['groupDm'] == true;
              final count = (row['unread'] as num?)?.toInt() ?? 0;
              return Padding(
                padding: const EdgeInsets.symmetric(
                  horizontal: 12,
                  vertical: 3,
                ),
                child: Card.filled(
                  margin: EdgeInsets.zero,
                  child: ListTile(
                    contentPadding: const EdgeInsets.symmetric(
                      horizontal: 16,
                      vertical: 8,
                    ),
                    leading: CircleAvatar(
                      child: group
                          ? Icon(
                              row['forum'] == true
                                  ? Icons.forum_outlined
                                  : Icons.tag,
                            )
                          : Text(
                              name.isEmpty
                                  ? '?'
                                  : name.characters.first.toUpperCase(),
                            ),
                    ),
                    title: Text(
                      name,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                    ),
                    subtitle: Text(
                      '${row['preview'] ?? row['description'] ?? jid}',
                      maxLines: 2,
                      overflow: TextOverflow.ellipsis,
                    ),
                    trailing: count > 0
                        ? Badge(label: Text('$count'))
                        : row['live'] == true
                        ? const Icon(Icons.graphic_eq)
                        : const Icon(Icons.chevron_right),
                    onTap: () => _open(jid, name, group),
                  ),
                ),
              );
            },
          ),
          const SliverPadding(padding: EdgeInsets.only(bottom: 96)),
        ],
      );
    }

    final shell = Scaffold(
      appBar: AppBar(
        title: Text(titles[_tab]),
        actions: [
          IconButton(
            tooltip: 'People',
            icon: const Icon(Icons.people_outline),
            onPressed: () => _push(
              PeopleScreen(
                bridge: bridge,
                onGroup: (jid, name) => _open(jid, name, true),
                onMessage: (jid, name) => _open(jid, name, false),
              ),
            ),
          ),
          IconButton(
            tooltip: 'Settings',
            icon: const Icon(Icons.settings_outlined),
            onPressed: () => _push(SettingsScreen(bridge: bridge)),
          ),
        ],
      ),
      body: Column(
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
              leading: const Icon(Icons.cloud_off_outlined),
              actions: [
                TextButton(
                  onPressed: () =>
                      perform(context, () => bridge.invoke('session.retry')),
                  child: const Text('Retry'),
                ),
              ],
            ),
          if (hasCall)
            ListTile(
              tileColor: Theme.of(context).colorScheme.secondaryContainer,
              leading: Icon(
                call == 'incoming' ? Icons.phone_in_talk : Icons.graphic_eq,
              ),
              title: Text(
                call == 'incoming' ? 'Incoming call' : 'Return to call',
              ),
              subtitle: Text(
                '${bridge.call['name'] ?? bridge.call['peer'] ?? bridge.call['peerJid'] ?? ''}',
              ),
              onTap: () => _push(ActiveCallScreen(bridge: bridge)),
            ),
          Expanded(child: content),
        ],
      ),
      floatingActionButton:
          (_tab == 1 || _tab == 0 && home['canCreate'] == true)
          ? FloatingActionButton.extended(
              onPressed: _tab == 0
                  ? _createRoom
                  : () => _push(
                      PeopleScreen(
                        bridge: bridge,
                        onGroup: (jid, name) => _open(jid, name, true),
                        onMessage: (jid, name) => _open(jid, name, false),
                      ),
                    ),
              icon: Icon(_tab == 0 ? Icons.add : Icons.edit_outlined),
              label: Text(_tab == 0 ? 'New room' : 'New message'),
            )
          : null,
      bottomNavigationBar: wide
          ? null
          : NavigationBar(
              selectedIndex: _tab,
              destinations: destinations,
              onDestinationSelected: (value) => setState(() {
                _tab = value;
                _query = '';
              }),
            ),
    );

    if (!wide) {
      return shell;
    }
    return Scaffold(
      body: SafeArea(
        child: Row(
          children: [
            NavigationRail(
              selectedIndex: _tab,
              labelType: NavigationRailLabelType.all,
              onDestinationSelected: (value) => setState(() {
                _tab = value;
                _query = '';
              }),
              destinations: List.generate(
                4,
                (i) => NavigationRailDestination(
                  icon: Icon(icons[i]),
                  selectedIcon: Icon(selectedIcons[i]),
                  label: Text(titles[i]),
                ),
              ),
            ),
            const VerticalDivider(width: 1),
            SizedBox(width: 360, child: shell),
            const VerticalDivider(width: 1),
            Expanded(
              child: Center(
                child: Column(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    const Icon(Icons.forum_outlined, size: 64),
                    const SizedBox(height: 20),
                    Text(
                      'Make yourself at home',
                      style: Theme.of(context).textTheme.headlineSmall,
                    ),
                    const SizedBox(height: 8),
                    const Text('Choose a conversation to get started.'),
                  ],
                ),
              ),
            ),
          ],
        ),
      ),
    );
  }

  Widget _you(BuildContext context) {
    final session = bridge.session;
    final name = '${session['username'] ?? session['jid'] ?? 'You'}';
    return ListView(
      padding: const EdgeInsets.all(20),
      children: [
        const SizedBox(height: 16),
        CircleAvatar(
          radius: 40,
          child: Text(
            name.isEmpty ? '?' : name.characters.first.toUpperCase(),
            style: Theme.of(context).textTheme.headlineLarge,
          ),
        ),
        const SizedBox(height: 20),
        Text(
          name,
          textAlign: TextAlign.center,
          style: Theme.of(context).textTheme.headlineMedium,
        ),
        const SizedBox(height: 8),
        Text('${session['jid'] ?? ''}', textAlign: TextAlign.center),
        const SizedBox(height: 32),
        Card.filled(
          child: Column(
            children: [
              ListTile(
                leading: const Icon(Icons.person_outline),
                title: const Text('Your profile'),
                subtitle: const Text('How you show up in your community'),
                trailing: const Icon(Icons.chevron_right),
                onTap: () => _push(ProfileScreen(bridge: bridge)),
              ),
              const Divider(height: 1, indent: 56),
              ListTile(
                leading: const Icon(Icons.tune),
                title: const Text('Settings'),
                subtitle: const Text('Appearance, notifications, and delivery'),
                trailing: const Icon(Icons.chevron_right),
                onTap: () => _push(SettingsScreen(bridge: bridge)),
              ),
            ],
          ),
        ),
        const SizedBox(height: 16),
        if (bridge.home['isCommunityOwner'] == true)
          ListTile(
            leading: const Icon(Icons.admin_panel_settings_outlined),
            title: const Text('Community administration'),
            onTap: () => _push(CommunityAdminScreen(bridge: bridge)),
          ),
      ],
    );
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
