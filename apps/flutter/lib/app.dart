import 'dart:async';

import 'package:flutter/material.dart';

import 'bridge.dart';
import 'design.dart';
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
    final colors = Theme.of(context).colorScheme;
    final type = Theme.of(context).textTheme;
    final form = WaddlePanel(
      padding: const EdgeInsets.all(24),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Text('Come on in.', style: type.headlineSmall),
          const SizedBox(height: 8),
          Text(
            'Your community is one sign-in away.',
            style: type.bodyMedium?.copyWith(color: colors.onSurfaceVariant),
          ),
          const SizedBox(height: 28),
          if (state == 'verifying') ...[
            const Text('Approve sign-in in your browser. Your code:'),
            const SizedBox(height: 16),
            SelectableText('${login['code'] ?? ''}', style: type.displaySmall),
            const SizedBox(height: 24),
            const LinearProgressIndicator(),
            TextButton(
              onPressed: () =>
                  perform(context, () => widget.bridge.invoke('login.cancel')),
              child: const Text('Cancel'),
            ),
          ] else if (state == 'failed') ...[
            Text('${login['error'] ?? 'Sign-in failed. Please try again.'}'),
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
                  icon: const Icon(Icons.arrow_forward_rounded),
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
            ),
          ],
        ],
      ),
    );
    final introduction = Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        const WaddleBrand(),
        const SizedBox(height: 56),
        SizedBox(
          width: 150,
          height: 100,
          child: WaddleMotif(color: colors.primary),
        ),
        const SizedBox(height: 28),
        Text(
          'Find your\nkind of people.',
          style: type.displayLarge?.copyWith(
            fontSize: 56,
            height: 1.02,
            letterSpacing: -2.4,
          ),
        ),
        const SizedBox(height: 24),
        Text(
          'A little more conversation.\nA lot more connection.',
          style: type.titleMedium?.copyWith(
            height: 1.5,
            color: colors.onSurfaceVariant,
          ),
        ),
      ],
    );
    return Scaffold(
      body: SafeArea(
        child: LayoutBuilder(
          builder: (context, constraints) {
            final wide = constraints.maxWidth >= 900;
            return Center(
              child: SingleChildScrollView(
                padding: EdgeInsets.symmetric(
                  horizontal: wide ? 64 : 28,
                  vertical: 36,
                ),
                child: ConstrainedBox(
                  constraints: const BoxConstraints(maxWidth: 1060),
                  child: wide
                      ? Row(
                          crossAxisAlignment: CrossAxisAlignment.center,
                          children: [
                            Expanded(child: introduction),
                            const SizedBox(width: 72),
                            Expanded(child: form),
                          ],
                        )
                      : Column(
                          crossAxisAlignment: CrossAxisAlignment.stretch,
                          children: [
                            introduction,
                            const SizedBox(height: 40),
                            form,
                            const SizedBox(height: 28),
                            Text(
                              'Good company. Shared space.',
                              style: type.labelMedium?.copyWith(
                                color: colors.onSurfaceVariant,
                              ),
                              textAlign: TextAlign.center,
                            ),
                          ],
                        ),
                ),
              ),
            );
          },
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

  void _open(String jid, String name, bool group, {String? threadId}) {
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
              width: 308,
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
                    crossAxisAlignment: CrossAxisAlignment.stretch,
                    children: [
                      Padding(
                        padding: const EdgeInsets.fromLTRB(24, 20, 16, 24),
                        child: Row(
                          children: [
                            const Expanded(child: WaddleBrand(compact: true)),
                            IconButton(
                              tooltip: 'Back to community',
                              icon: const Icon(Icons.cottage_outlined),
                              onPressed: () => Navigator.maybePop(context),
                            ),
                          ],
                        ),
                      ),
                      Padding(
                        padding: const EdgeInsets.symmetric(horizontal: 20),
                        child: SegmentedButton<bool>(
                          segments: const [
                            ButtonSegment(value: false, label: Text('Rooms')),
                            ButtonSegment(value: true, label: Text('Messages')),
                          ],
                          selected: {_direct},
                          showSelectedIcon: false,
                          onSelectionChanged: (value) =>
                              setState(() => _direct = value.single),
                        ),
                      ),
                      Padding(
                        padding: const EdgeInsets.all(20),
                        child: _QuietSearch(
                          hint: 'Find a conversation',
                          onChanged: (value) => setState(() => _query = value),
                        ),
                      ),
                      Expanded(
                        child: ListView.builder(
                          padding: const EdgeInsets.symmetric(horizontal: 12),
                          itemCount: rows.length,
                          itemBuilder: (context, index) {
                            final row = rows[index];
                            final jid = row['jid'] as String;
                            final group = !_direct || row['groupDm'] == true;
                            final name = '${row['name'] ?? jid}';
                            return _CommunityRow(
                              row: row,
                              group: group,
                              compact: true,
                              selected: jid == widget.jid,
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
            VerticalDivider(
              width: 1,
              color: Theme.of(context).colorScheme.outlineVariant,
            ),
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

  void _people() => _push(
    PeopleScreen(
      bridge: bridge,
      onGroup: (jid, name) => _open(jid, name, true),
      onMessage: (jid, name) => _open(jid, name, false),
    ),
  );

  @override
  Widget build(BuildContext context) {
    final wide = MediaQuery.sizeOf(context).width >= 840;
    final home = bridge.home;
    final rooms = records(
      home['rooms'],
    ).where((room) => room['groupDm'] != true).toList();
    final peers = [
      ...records(home['dmPeers']),
      ...records(home['rooms']).where((room) => room['groupDm'] == true),
    ];
    final unread = peers.fold<int>(
      0,
      (sum, row) => sum + ((row['unread'] as num?)?.toInt() ?? 0),
    );
    final titles = ['Home', 'Messages', 'Activity', 'You'];
    final icons = [
      Icons.cottage_outlined,
      Icons.chat_bubble_outline_rounded,
      Icons.notifications_none_rounded,
      Icons.person_outline_rounded,
    ];
    final selectedIcons = [
      Icons.cottage_rounded,
      Icons.chat_bubble_rounded,
      Icons.notifications_rounded,
      Icons.person_rounded,
    ];
    final connection = bridge.topic('connection')['state'];
    final call = bridge.call['state'];
    final hasCall = call != null && call != 'idle' && call != 'ended';
    final colors = Theme.of(context).colorScheme;
    final shell = Scaffold(
      appBar: AppBar(
        toolbarHeight: 80,
        titleSpacing: 24,
        title: _tab == 0
            ? LayoutBuilder(
                builder: (context, constraints) => WaddleBrand(
                  compact:
                      constraints.maxWidth < 180 ||
                      MediaQuery.textScalerOf(context).scale(1) > 1.4,
                ),
              )
            : Text(titles[_tab], maxLines: 1, overflow: TextOverflow.ellipsis),
        actions: [
          IconButton(
            tooltip: 'People',
            icon: const Icon(Icons.people_outline_rounded),
            onPressed: _people,
          ),
          Padding(
            padding: const EdgeInsets.only(right: 16),
            child: IconButton(
              tooltip: 'Settings',
              icon: const Icon(Icons.tune_rounded),
              onPressed: () => _push(SettingsScreen(bridge: bridge)),
            ),
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
              tileColor: colors.secondaryContainer,
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
          Expanded(
            child: switch (_tab) {
              2 => ActivityScreen(bridge: bridge, onOpen: _open),
              3 => _you(context),
              _ => _conversations(
                context,
                _tab == 0 ? rooms : peers,
                rooms,
                peers,
              ),
            },
          ),
        ],
      ),
      bottomNavigationBar: wide
          ? null
          : NavigationBar(
              selectedIndex: _tab,
              destinations: List.generate(
                4,
                (index) => NavigationDestination(
                  icon: index == 1 && unread > 0
                      ? Badge(label: Text('$unread'), child: Icon(icons[index]))
                      : Icon(icons[index]),
                  selectedIcon: Icon(selectedIcons[index]),
                  label: titles[index],
                ),
              ),
              onDestinationSelected: (value) => setState(() {
                _tab = value;
                _query = '';
              }),
            ),
    );
    if (!wide) return shell;
    return Scaffold(
      body: SafeArea(
        child: Row(
          children: [
            NavigationRail(
              selectedIndex: _tab,
              labelType: NavigationRailLabelType.all,
              leading: const Padding(
                padding: EdgeInsets.fromLTRB(0, 16, 0, 32),
                child: WaddleMark(size: 40),
              ),
              onDestinationSelected: (value) => setState(() {
                _tab = value;
                _query = '';
              }),
              destinations: List.generate(
                4,
                (index) => NavigationRailDestination(
                  icon: Icon(icons[index]),
                  selectedIcon: Icon(selectedIcons[index]),
                  label: Text(titles[index]),
                ),
              ),
            ),
            VerticalDivider(width: 1, color: colors.outlineVariant),
            Expanded(child: shell),
          ],
        ),
      ),
    );
  }

  Widget _conversations(
    BuildContext context,
    List<Map<String, dynamic>> allRows,
    List<Map<String, dynamic>> rooms,
    List<Map<String, dynamic>> peers,
  ) {
    final home = bridge.home;
    final colors = Theme.of(context).colorScheme;
    final type = Theme.of(context).textTheme;
    final rows = allRows
        .where(
          (row) => '${row['name'] ?? row['jid']}'.toLowerCase().contains(
            _query.toLowerCase(),
          ),
        )
        .toList();
    final compactHeading =
        MediaQuery.sizeOf(context).width <= 360 ||
        MediaQuery.textScalerOf(context).scale(1) >= 1.5;
    final people = records(home['people']);
    final totalUnread = [
      ...rooms,
      ...peers,
    ].fold<int>(0, (sum, row) => sum + ((row['unread'] as num?)?.toInt() ?? 0));
    final name = '${bridge.session['username'] ?? ''}'.trim();
    final firstName = name.isEmpty ? '' : name.split(RegExp(r'\s+')).first;
    final greeting = firstName.isEmpty
        ? 'Make yourself\nat home.'
        : 'Hey, $firstName.\nSettle in.';
    final heading = Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        if (_tab == 0) ...[
          const SizedBox(height: 8),
          Row(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Expanded(
                child: Text(
                  greeting,
                  style: type.headlineLarge?.copyWith(
                    fontSize: compactHeading ? 26 : 36,
                    height: 1.14,
                    letterSpacing: -1.5,
                  ),
                ),
              ),
              if (MediaQuery.textScalerOf(context).scale(1) <= 1.3)
                Padding(
                  padding: const EdgeInsets.only(top: 6, left: 8),
                  child: SizedBox(
                    width: 60,
                    height: 60,
                    child: WaddleMotif(color: colors.tertiary),
                  ),
                ),
            ],
          ),
          const SizedBox(height: 16),
          Text(
            'A place for your people.',
            style: type.bodyMedium?.copyWith(
              color: colors.onSurfaceVariant,
              height: 1.5,
            ),
          ),
          const SizedBox(height: 24),
          Wrap(
            spacing: 10,
            runSpacing: 10,
            children: [
              _CommunityPill(
                icon: Icons.door_front_door_outlined,
                label:
                    '${rooms.length} ${rooms.length == 1 ? 'room' : 'rooms'}',
              ),
              if (totalUnread > 0)
                _CommunityPill(
                  icon: Icons.mark_chat_unread_outlined,
                  label: '$totalUnread unread',
                  emphasized: true,
                ),
              if (people.isNotEmpty)
                _CommunityPill(
                  icon: Icons.people_outline_rounded,
                  label: '${people.length} people',
                  onTap: _people,
                ),
            ],
          ),
        ] else ...[
          Text(
            'Pick up the\nconversation.',
            style: type.headlineLarge?.copyWith(
              fontSize: compactHeading ? 26 : 36,
              height: 1.14,
              letterSpacing: -1.5,
            ),
          ),
          const SizedBox(height: 12),
          Text(
            'A good place to say a little more.',
            style: type.bodyMedium?.copyWith(color: colors.onSurfaceVariant),
          ),
        ],
        const SizedBox(height: 28),
        _QuietSearch(
          key: ValueKey('search:$_tab'),
          hint: _tab == 0 ? 'Find a room' : 'Find a conversation',
          onChanged: (value) => setState(() => _query = value),
        ),
        const SizedBox(height: 28),
      ],
    );
    final roomList = Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        LayoutBuilder(
          builder: (context, constraints) {
            final canCreate = _tab == 1 || home['canCreate'] == true;
            final stackAction =
                constraints.maxWidth < 340 ||
                MediaQuery.textScalerOf(context).scale(1) > 1.3;
            final action = canCreate
                ? TextButton.icon(
                    style: TextButton.styleFrom(
                      minimumSize: const Size(48, 48),
                    ),
                    onPressed: _tab == 0 ? _createRoom : _people,
                    icon: Icon(
                      _tab == 0 ? Icons.add_rounded : Icons.edit_outlined,
                      size: 20,
                    ),
                    label: Text(_tab == 0 ? 'New room' : 'New message'),
                  )
                : null;
            final heading = WaddleSectionHeading(
              title: _tab == 0 ? 'Your rooms' : 'Your conversations',
              eyebrow: _tab == 0 ? 'THE COMMON ROOM' : 'JUST BETWEEN US',
              trailing: action != null && !stackAction
                  ? action
                  : Text(
                      '${rows.length}',
                      style: type.labelLarge?.copyWith(
                        color: colors.onSurfaceVariant,
                      ),
                    ),
            );
            return Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                heading,
                if (action != null && stackAction) ...[
                  const SizedBox(height: 8),
                  Align(alignment: Alignment.centerLeft, child: action),
                ],
              ],
            );
          },
        ),
        const SizedBox(height: 16),
        if (rows.isEmpty && home['settled'] != true)
          const Padding(
            padding: EdgeInsets.all(40),
            child: Center(child: CircularProgressIndicator()),
          )
        else if (rows.isEmpty)
          WaddlePanel(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Icon(
                  _tab == 0
                      ? Icons.door_front_door_outlined
                      : Icons.waving_hand_outlined,
                  size: 36,
                  color: colors.primary,
                ),
                const SizedBox(height: 20),
                Text(
                  _query.isNotEmpty
                      ? 'No matches'
                      : _tab == 0
                      ? 'Room for something good.'
                      : 'Start with hello.',
                  style: type.titleLarge,
                ),
                const SizedBox(height: 8),
                Text(
                  _query.isNotEmpty
                      ? 'Try another name.'
                      : _tab == 0
                      ? 'Your rooms will appear here when your community is ready.'
                      : 'Find someone in your community and make their day.',
                  style: type.bodyMedium,
                ),
                if (_tab == 1)
                  Padding(
                    padding: const EdgeInsets.only(top: 20),
                    child: FilledButton.tonalIcon(
                      onPressed: _people,
                      icon: const Icon(Icons.add_rounded),
                      label: const Text('Find your people'),
                    ),
                  ),
              ],
            ),
          )
        else
          WaddlePanel(
            padding: EdgeInsets.zero,
            child: Column(
              children: [
                for (var index = 0; index < rows.length; index++) ...[
                  if (index > 0)
                    Divider(
                      height: 1,
                      indent: 80,
                      endIndent: 20,
                      color: colors.outlineVariant.withValues(alpha: .5),
                    ),
                  _CommunityRow(
                    row: rows[index],
                    group: _tab == 0 || rows[index]['groupDm'] == true,
                    onTap: () => _open(
                      '${rows[index]['jid']}',
                      '${rows[index]['name'] ?? rows[index]['jid']}',
                      _tab == 0 || rows[index]['groupDm'] == true,
                    ),
                  ),
                ],
              ],
            ),
          ),
      ],
    );
    final community = _communityPanel(context, people);
    return LayoutBuilder(
      builder: (context, constraints) {
        final roomy = constraints.maxWidth >= 920;
        return ListView(
          padding: EdgeInsets.fromLTRB(
            roomy ? 48 : 24,
            8,
            roomy ? 48 : 24,
            112,
          ),
          children: [
            ConstrainedBox(
              constraints: const BoxConstraints(maxWidth: 1120),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  heading,
                  if (roomy && _tab == 0)
                    Row(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Expanded(flex: 7, child: roomList),
                        const SizedBox(width: 28),
                        Expanded(flex: 4, child: community),
                      ],
                    )
                  else ...[
                    roomList,
                    if (_tab == 0 && people.isNotEmpty) ...[
                      const SizedBox(height: 32),
                      community,
                    ],
                  ],
                ],
              ),
            ),
          ],
        );
      },
    );
  }

  Widget _communityPanel(
    BuildContext context,
    List<Map<String, dynamic>> people,
  ) {
    final colors = Theme.of(context).colorScheme;
    final type = Theme.of(context).textTheme;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        WaddleSectionHeading(
          title: 'Good company',
          eyebrow: 'THE PEOPLE MAKE THE PLACE',
          trailing: IconButton(
            tooltip: 'Browse people',
            onPressed: _people,
            icon: const Icon(Icons.arrow_outward_rounded),
          ),
        ),
        const SizedBox(height: 16),
        WaddlePanel(
          color: colors.secondaryContainer.withValues(alpha: .55),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              if (people.isEmpty) ...[
                const WaddleMark(size: 40),
                const SizedBox(height: 24),
                Text('Better together.', style: type.headlineSmall),
                const SizedBox(height: 8),
                Text(
                  'Find someone in your community to start a conversation.',
                  style: type.bodyMedium,
                ),
              ] else ...[
                for (final person in people.take(4))
                  Padding(
                    padding: const EdgeInsets.only(bottom: 16),
                    child: InkWell(
                      borderRadius: BorderRadius.circular(16),
                      onTap: () => _open(
                        '${person['jid']}',
                        '${person['name'] ?? person['jid']}',
                        false,
                      ),
                      child: ConstrainedBox(
                        constraints: const BoxConstraints(minHeight: 48),
                        child: Row(
                          children: [
                            WaddleAvatar(
                              name: '${person['name'] ?? '?'}',
                              size: 40,
                              online:
                                  person['show'] == 'available' ||
                                  person['show'] == 'chat',
                            ),
                            const SizedBox(width: 12),
                            Expanded(
                              child: Column(
                                crossAxisAlignment: CrossAxisAlignment.start,
                                children: [
                                  Text(
                                    '${person['name'] ?? person['jid']}',
                                    style: type.titleSmall,
                                  ),
                                  if (person['inHuddle'] == true)
                                    Text(
                                      'In a call',
                                      style: type.bodySmall?.copyWith(
                                        color: colors.primary,
                                      ),
                                    ),
                                ],
                              ),
                            ),
                            const Icon(Icons.arrow_outward_rounded, size: 18),
                          ],
                        ),
                      ),
                    ),
                  ),
              ],
              const SizedBox(height: 4),
              TextButton.icon(
                onPressed: _people,
                icon: const Icon(Icons.people_outline_rounded),
                label: const Text('Find your people'),
              ),
            ],
          ),
        ),
      ],
    );
  }

  Widget _you(BuildContext context) {
    final type = Theme.of(context).textTheme;
    final colors = Theme.of(context).colorScheme;
    final session = bridge.session;
    final name = '${session['username'] ?? session['jid'] ?? 'You'}';
    final profile = bridge.topic('profile');
    return ListView(
      padding: const EdgeInsets.fromLTRB(24, 12, 24, 40),
      children: [
        WaddlePanel(
          color: colors.primaryContainer,
          padding: const EdgeInsets.all(28),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              WaddleAvatar(name: name, size: 72),
              const SizedBox(height: 24),
              Text(
                name,
                style: type.headlineLarge?.copyWith(letterSpacing: -1),
              ),
              const SizedBox(height: 8),
              SelectableText(
                '${session['jid'] ?? ''}',
                style: type.bodyMedium?.copyWith(
                  color: colors.onPrimaryContainer.withValues(alpha: .7),
                ),
              ),
              if (profile['mood'] is Map &&
                  profile['mood']['text'] != null) ...[
                const SizedBox(height: 20),
                Text('${profile['mood']['text']}', style: type.titleMedium),
              ],
            ],
          ),
        ),
        const SizedBox(height: 32),
        const WaddleSectionHeading(
          title: 'Make yourself at home',
          eyebrow: 'YOUR LITTLE CORNER',
        ),
        const SizedBox(height: 16),
        WaddlePanel(
          padding: EdgeInsets.zero,
          child: Column(
            children: [
              ListTile(
                contentPadding: const EdgeInsets.symmetric(
                  horizontal: 20,
                  vertical: 10,
                ),
                leading: const Icon(Icons.person_outline_rounded),
                title: const Text('Your profile'),
                subtitle: const Text('How you show up in your community'),
                trailing: const Icon(Icons.arrow_outward_rounded),
                onTap: () => _push(ProfileScreen(bridge: bridge)),
              ),
              Divider(
                height: 1,
                indent: 64,
                color: colors.outlineVariant.withValues(alpha: .5),
              ),
              ListTile(
                contentPadding: const EdgeInsets.symmetric(
                  horizontal: 20,
                  vertical: 10,
                ),
                leading: const Icon(Icons.tune_rounded),
                title: const Text('Settings'),
                subtitle: const Text('Appearance, notifications, and delivery'),
                trailing: const Icon(Icons.arrow_outward_rounded),
                onTap: () => _push(SettingsScreen(bridge: bridge)),
              ),
            ],
          ),
        ),
        if (bridge.home['isCommunityOwner'] == true)
          Padding(
            padding: const EdgeInsets.only(top: 24),
            child: ListTile(
              leading: const Icon(Icons.admin_panel_settings_outlined),
              title: const Text('Community administration'),
              trailing: const Icon(Icons.arrow_outward_rounded),
              onTap: () => _push(CommunityAdminScreen(bridge: bridge)),
            ),
          ),
      ],
    );
  }
}

class _QuietSearch extends StatelessWidget {
  const _QuietSearch({super.key, required this.hint, required this.onChanged});
  final String hint;
  final ValueChanged<String> onChanged;
  @override
  Widget build(BuildContext context) => SearchBar(
    elevation: const WidgetStatePropertyAll(0),
    backgroundColor: WidgetStatePropertyAll(
      Theme.of(context).colorScheme.surfaceContainer,
    ),
    padding: const WidgetStatePropertyAll(EdgeInsets.symmetric(horizontal: 16)),
    leading: Icon(
      Icons.search_rounded,
      color: Theme.of(context).colorScheme.onSurfaceVariant,
      size: 22,
    ),
    hintText: hint,
    onChanged: onChanged,
  );
}

class _CommunityPill extends StatelessWidget {
  const _CommunityPill({
    required this.icon,
    required this.label,
    this.emphasized = false,
    this.onTap,
  });
  final IconData icon;
  final String label;
  final bool emphasized;
  final VoidCallback? onTap;
  @override
  Widget build(BuildContext context) {
    final colors = Theme.of(context).colorScheme;
    return Material(
      color: emphasized ? colors.primaryContainer : colors.surfaceContainer,
      borderRadius: BorderRadius.circular(24),
      child: InkWell(
        onTap: onTap,
        borderRadius: BorderRadius.circular(24),
        child: ConstrainedBox(
          constraints: const BoxConstraints(minHeight: 48),
          child: Padding(
            padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 10),
            child: Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                Icon(icon, size: 16, color: colors.primary),
                const SizedBox(width: 7),
                Text(
                  label,
                  style: Theme.of(context).textTheme.labelMedium?.copyWith(
                    fontWeight: FontWeight.w700,
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

class _CommunityRow extends StatelessWidget {
  const _CommunityRow({
    required this.row,
    required this.group,
    required this.onTap,
    this.compact = false,
    this.selected = false,
  });
  final Map<String, dynamic> row;
  final bool group, compact, selected;
  final VoidCallback onTap;
  @override
  Widget build(BuildContext context) {
    final colors = Theme.of(context).colorScheme;
    final type = Theme.of(context).textTheme;
    final jid = '${row['jid'] ?? ''}';
    final name = '${row['name'] ?? jid.split('@').first}';
    final unread = (row['unread'] as num?)?.toInt() ?? 0;
    final variants = [
      colors.primaryContainer,
      colors.secondaryContainer,
      colors.tertiaryContainer,
    ];
    final tone =
        name.codeUnits.fold<int>(0, (sum, code) => sum + code) %
        variants.length;
    final icon = row['forum'] == true
        ? Icons.forum_outlined
        : name.toLowerCase().contains('design')
        ? Icons.palette_outlined
        : name.toLowerCase().contains('music')
        ? Icons.music_note_outlined
        : name.toLowerCase().contains('weekend')
        ? Icons.wb_sunny_outlined
        : Icons.tag_rounded;
    return Material(
      color: selected
          ? colors.primaryContainer.withValues(alpha: .5)
          : Colors.transparent,
      borderRadius: BorderRadius.circular(20),
      child: InkWell(
        onTap: onTap,
        borderRadius: BorderRadius.circular(20),
        child: Padding(
          padding: EdgeInsets.symmetric(
            horizontal: compact ? 12 : 20,
            vertical: compact ? 14 : 20,
          ),
          child: Row(
            crossAxisAlignment: CrossAxisAlignment.center,
            children: [
              group
                  ? Container(
                      width: 44,
                      height: 44,
                      decoration: BoxDecoration(
                        color: variants[tone],
                        borderRadius: BorderRadius.circular(15),
                      ),
                      child: Icon(icon, size: 22, color: colors.onSurface),
                    )
                  : WaddleAvatar(name: name, size: 44),
              const SizedBox(width: 14),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(
                      name,
                      style: type.titleMedium?.copyWith(
                        fontWeight: unread > 0
                            ? FontWeight.w800
                            : FontWeight.w600,
                      ),
                      maxLines: 2,
                      overflow: TextOverflow.ellipsis,
                    ),
                    const SizedBox(height: 5),
                    Text(
                      '${row['preview'] ?? row['description'] ?? jid}',
                      style: type.bodySmall?.copyWith(
                        color: colors.onSurfaceVariant,
                        height: 1.45,
                      ),
                      maxLines: compact ? 1 : 2,
                      overflow: TextOverflow.ellipsis,
                    ),
                  ],
                ),
              ),
              const SizedBox(width: 10),
              if (unread > 0)
                Container(
                  constraints: const BoxConstraints(
                    minWidth: 24,
                    minHeight: 24,
                  ),
                  padding: const EdgeInsets.symmetric(
                    horizontal: 7,
                    vertical: 4,
                  ),
                  decoration: BoxDecoration(
                    color: colors.primary,
                    borderRadius: BorderRadius.circular(10),
                  ),
                  child: Text(
                    '$unread',
                    textAlign: TextAlign.center,
                    style: type.labelSmall?.copyWith(
                      color: colors.onPrimary,
                      fontWeight: FontWeight.w800,
                    ),
                  ),
                )
              else if (row['live'] == true)
                Icon(Icons.graphic_eq_rounded, size: 20, color: colors.tertiary)
              else
                Icon(
                  Icons.chevron_right_rounded,
                  size: 20,
                  color: colors.outline,
                ),
            ],
          ),
        ),
      ),
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
