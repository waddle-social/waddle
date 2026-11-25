part of '../app.dart';

const _sidebarBreakpoint = 768.0;
typedef _OpenEntry = void Function(Map<String, dynamic> row, bool group);
typedef _Destination = ({String route, String label, IconData icon});
const List<_Destination> _views = [
  (route: '/', label: 'Home', icon: Icons.home_outlined),
  (route: 'inbox', label: 'Inbox', icon: Icons.inbox_outlined),
  (route: 'events', label: 'Calendar', icon: Icons.calendar_today_outlined),
  (route: 'people', label: 'People', icon: Icons.people_outline),
];
const List<_Destination> _more = [
  (route: 'threads', label: 'Discussions', icon: Icons.forum_outlined),
  (route: 'feed', label: 'Feed', icon: Icons.article_outlined),
  (route: 'stories', label: 'Stories', icon: Icons.photo_library_outlined),
  (route: 'trips', label: 'Trips', icon: Icons.map_outlined),
  (route: 'remote', label: 'Remote communities', icon: Icons.public),
];
String _entryName(Map row) =>
    (row['name'] as String?) ?? '${row['jid'] ?? ''}'.split('@').first;
int _count(Map row, String key) => (row[key] as num?)?.toInt() ?? 0;
bool _around(Map row) =>
    const {'available', 'chat', 'online', 'dnd'}.contains(row['show']);
bool _hasCall(WaddleBridge bridge) => const {
  'incoming',
  'outgoing',
  'connecting',
  'active',
}.contains(bridge.call['state']);
String _presence(Map row) => switch (row['show']) {
  'available' || 'online' || 'chat' => 'Online',
  'away' || 'xa' => 'Away',
  'dnd' => 'Busy',
  _ => 'Offline',
};
IconData _roomIcon(Map row) => row['groupDm'] == true
    ? Icons.group_outlined
    : row['forum'] == true
    ? Icons.forum_outlined
    : Icons.tag;

class _AccountSidebar extends StatelessWidget {
  const _AccountSidebar({
    required this.bridge,
    required this.page,
    this.jid,
    required this.select,
    required this.open,
    required this.jump,
  });
  final WaddleBridge bridge;
  final String page;
  final String? jid;
  final ValueChanged<String> select;
  final _OpenEntry open;
  final VoidCallback jump;

  @override
  Widget build(BuildContext context) {
    final desktop = MediaQuery.sizeOf(context).width >= _sidebarBreakpoint;
    final theme = Theme.of(context);
    return Theme(
      data: desktop && theme.brightness == Brightness.light
          ? waddleTheme(waddleScheme(Brightness.dark))
          : theme,
      child: Builder(
        builder: (context) => AnimatedBuilder(
          animation: bridge,
          builder: (context, _) {
            final colors = Theme.of(context).colorScheme;
            final type = Theme.of(context).textTheme;
            final rooms = records(bridge.home['rooms']);
            final currentRoom = rooms
                .where((row) => row['jid'] == jid)
                .firstOrNull;
            final community =
                currentRoom?['communityName'] ??
                bridge.home['communityName'] ??
                'Waddle';
            final peers = records(bridge.home['dmPeers']);
            final unread = [...rooms, ...peers].fold(
              0,
              (n, r) => n + _count(r, 'unread') + _count(r, 'threadUnread'),
            );
            final name =
                '${bridge.session['username'] ?? bridge.session['jid']?.toString().split('@').first ?? 'You'}';
            final roomRows = rooms.where((r) => r['groupDm'] != true);
            final direct = [
              ...rooms.where((r) => r['groupDm'] == true),
              ...peers,
            ];
            Widget row(
              Map<String, dynamic> r,
              bool group, {
              bool avatar = false,
            }) => _SidebarRow(
              title: _entryName(r),
              leading: avatar && r['groupDm'] != true
                  ? _AccountAvatar(
                      jid: r['jid'] as String,
                      name: _entryName(r),
                      size: 28,
                      online: _around(r),
                    )
                  : Icon(_roomIcon(r), size: 19),
              count: _count(r, 'unread'),
              selected: r['jid'] == jid,
              onTap: () => open(r, group),
            );
            return Material(
              color: colors.surfaceContainerLow,
              child: Column(
                children: [
                  Padding(
                    padding: const EdgeInsets.fromLTRB(12, 10, 8, 7),
                    child: Row(
                      children: [
                        Expanded(
                          child: InkWell(
                            onTap: () => select('/'),
                            borderRadius: BorderRadius.circular(8),
                            child: Padding(
                              padding: const EdgeInsets.symmetric(vertical: 5),
                              child: Row(
                                children: [
                                  Container(
                                    width: 36,
                                    height: 36,
                                    alignment: Alignment.center,
                                    decoration: BoxDecoration(
                                      color: const Color(0xfffff4e1),
                                      borderRadius: BorderRadius.circular(8),
                                    ),
                                    child: const WaddleMark(size: 27),
                                  ),
                                  const SizedBox(width: 10),
                                  Flexible(
                                    child: Text(
                                      '$community',
                                      overflow: TextOverflow.ellipsis,
                                      style: type.titleMedium?.copyWith(
                                        fontSize: 17,
                                        fontWeight: FontWeight.w800,
                                      ),
                                    ),
                                  ),
                                ],
                              ),
                            ),
                          ),
                        ),
                        PopupMenuButton<String>(
                          tooltip: 'Community menu',
                          icon: const Icon(Icons.keyboard_arrow_down, size: 18),
                          onSelected: select,
                          itemBuilder: (_) => [
                            for (final dest in _more)
                              PopupMenuItem(
                                value: dest.route,
                                child: Text(dest.label),
                              ),
                          ],
                        ),
                        IconButton(
                          tooltip: 'New message',
                          icon: const Icon(Icons.edit_square, size: 19),
                          onPressed: () => select('people'),
                        ),
                      ],
                    ),
                  ),
                  Padding(
                    padding: const EdgeInsets.fromLTRB(10, 0, 10, 8),
                    child: OutlinedButton(
                      onPressed: jump,
                      style: OutlinedButton.styleFrom(
                        backgroundColor: colors.surfaceContainer,
                        foregroundColor: colors.onSurfaceVariant,
                        shape: RoundedRectangleBorder(
                          borderRadius: BorderRadius.circular(8),
                        ),
                        padding: const EdgeInsets.symmetric(horizontal: 11),
                      ),
                      child: Row(
                        children: [
                          const Icon(Icons.search, size: 19),
                          const SizedBox(width: 8),
                          const Expanded(
                            child: Text(
                              'Search or jump to…',
                              maxLines: 1,
                              overflow: TextOverflow.ellipsis,
                            ),
                          ),
                          if (desktop)
                            Text(
                              Theme.of(context).platform == TargetPlatform.macOS
                                  ? '⌘ K'
                                  : 'Ctrl K',
                              style: type.labelSmall?.copyWith(
                                fontFamily: 'Atkinson Hyperlegible Mono',
                              ),
                            ),
                        ],
                      ),
                    ),
                  ),
                  Expanded(
                    child: ListView(
                      padding: const EdgeInsets.symmetric(horizontal: 8),
                      children: [
                        for (final item in _views)
                          _SidebarRow(
                            title: item.label,
                            leading: Icon(item.icon, size: 20),
                            selected:
                                page == item.route ||
                                item.route == 'inbox' && page == 'threads',
                            onTap: () => select(item.route),
                            count: item.route == 'inbox' ? unread : 0,
                          ),
                        _SideHeading(
                          'Rooms',
                          action: IconButton(
                            tooltip: 'Browse all rooms',
                            icon: const Icon(Icons.explore_outlined, size: 18),
                            onPressed: () => select('rooms'),
                          ),
                        ),
                        if (roomRows.isEmpty)
                          _SideNote(
                            bridge.home['settled'] == true
                                ? 'No rooms yet. Browse to create the first one.'
                                : 'Loading rooms…',
                          ),
                        for (final r in roomRows) row(r, true),
                        const _SideHeading('Direct messages'),
                        if (direct.isEmpty)
                          const _SideNote('No conversations yet.'),
                        for (final r in direct)
                          row(r, r['groupDm'] == true, avatar: true),
                      ],
                    ),
                  ),
                  if (_hasCall(bridge))
                    _SidebarRow(
                      title: bridge.call['state'] == 'incoming'
                          ? 'Incoming call'
                          : 'Return to call',
                      leading: Icon(
                        Icons.phone_in_talk_outlined,
                        color: colors.tertiary,
                      ),
                      onTap: () => select('call'),
                    ),
                  const Divider(height: 1),
                  Padding(
                    padding: const EdgeInsets.fromLTRB(14, 6, 8, 6),
                    child: Row(
                      children: [
                        _AccountAvatar(
                          jid: '${bridge.session['jid']}',
                          name: name,
                          size: 32,
                          online:
                              bridge.topic('connection')['state'] == 'ready',
                        ),
                        const SizedBox(width: 10),
                        Expanded(
                          child: TextButton(
                            onPressed: () => select('profile'),
                            style: TextButton.styleFrom(
                              alignment: Alignment.centerLeft,
                              foregroundColor: colors.onSurface,
                              padding: EdgeInsets.zero,
                            ),
                            child: Text(name, overflow: TextOverflow.ellipsis),
                          ),
                        ),
                        IconButton(
                          tooltip: 'Settings',
                          icon: const Icon(Icons.settings_outlined, size: 19),
                          onPressed: () => select('settings'),
                        ),
                        if (bridge.home['isCommunityOwner'] == true)
                          IconButton(
                            tooltip: 'Community administration',
                            icon: const Icon(
                              Icons.admin_panel_settings_outlined,
                              size: 19,
                            ),
                            onPressed: () => select('admin'),
                          ),
                      ],
                    ),
                  ),
                ],
              ),
            );
          },
        ),
      ),
    );
  }
}

class _SidebarRow extends StatelessWidget {
  const _SidebarRow({
    required this.title,
    required this.leading,
    required this.onTap,
    this.selected = false,
    this.count = 0,
  });
  final String title;
  final Widget leading;
  final bool selected;
  final int count;
  final VoidCallback onTap;
  @override
  Widget build(BuildContext context) {
    final c = Theme.of(context).colorScheme;
    return Semantics(
      selected: selected,
      button: true,
      child: Stack(
        children: [
          Material(
            color: selected
                ? c.onSurface.withValues(alpha: .10)
                : Colors.transparent,
            borderRadius: BorderRadius.circular(8),
            child: InkWell(
              onTap: onTap,
              borderRadius: BorderRadius.circular(8),
              child: ConstrainedBox(
                constraints: BoxConstraints(
                  minHeight:
                      MediaQuery.sizeOf(context).width < _sidebarBreakpoint
                      ? 48
                      : 36,
                ),
                child: Padding(
                  padding: const EdgeInsets.symmetric(
                    horizontal: 10,
                    vertical: 4,
                  ),
                  child: Row(
                    children: [
                      SizedBox(
                        width: 28,
                        child: IconTheme(
                          data: IconThemeData(
                            color: selected ? c.onSurface : c.onSurfaceVariant,
                          ),
                          child: leading,
                        ),
                      ),
                      const SizedBox(width: 8),
                      Expanded(
                        child: Text(
                          title,
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: TextStyle(
                            color: c.onSurface,
                            fontSize: 15,
                            fontWeight: count > 0 || selected
                                ? FontWeight.w700
                                : FontWeight.w500,
                          ),
                        ),
                      ),
                      if (count > 0)
                        Badge(
                          label: Text('$count'),
                          backgroundColor: c.secondaryContainer,
                          textColor: c.onSecondaryContainer,
                        ),
                    ],
                  ),
                ),
              ),
            ),
          ),
          if (selected)
            Positioned(
              left: 0,
              top: 8,
              bottom: 8,
              child: IgnorePointer(
                child: Container(
                  width: 3,
                  decoration: BoxDecoration(
                    color: c.onSurface,
                    borderRadius: const BorderRadius.horizontal(
                      right: Radius.circular(3),
                    ),
                  ),
                ),
              ),
            ),
        ],
      ),
    );
  }
}

class _SideHeading extends StatelessWidget {
  const _SideHeading(this.text, {this.action});
  final String text;
  final Widget? action;
  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.fromLTRB(12, 14, 2, 4),
    child: Row(
      children: [
        Expanded(
          child: Text(
            text,
            style: Theme.of(context).textTheme.labelLarge?.copyWith(
              color: Theme.of(context).colorScheme.onSurfaceVariant,
              fontWeight: FontWeight.bold,
            ),
          ),
        ),
        ?action,
      ],
    ),
  );
}

class _SideNote extends StatelessWidget {
  const _SideNote(this.text);
  final String text;
  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.fromLTRB(12, 4, 12, 8),
    child: Text(
      text,
      style: Theme.of(context).textTheme.bodySmall?.copyWith(
        color: Theme.of(context).colorScheme.onSurfaceVariant,
      ),
    ),
  );
}
