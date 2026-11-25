part of '../app.dart';

class _HomeDashboard extends StatefulWidget {
  const _HomeDashboard({
    required this.bridge,
    required this.select,
    required this.open,
  });
  final WaddleBridge bridge;
  final ValueChanged<String> select;
  final _OpenEntry open;
  @override
  State<_HomeDashboard> createState() => _HomeDashboardState();
}

class _HomeDashboardState extends State<_HomeDashboard> {
  late final Timer _clock;
  @override
  void initState() {
    super.initState();
    _clock = Timer.periodic(const Duration(minutes: 1), (_) => setState(() {}));
  }

  @override
  void dispose() {
    _clock.cancel();
    super.dispose();
  }

  void _enter(Map<String, dynamic> row) {
    if (row['live'] == true && !_hasCall(widget.bridge)) {
      perform(
        context,
        () => widget.bridge.invoke('call.start', {
          'jid': row['jid'],
          'group': true,
          'video': row['video'] == true,
        }),
      );
    } else {
      widget.open(row, true);
    }
  }

  @override
  Widget build(BuildContext context) => AnimatedBuilder(
    animation: widget.bridge,
    builder: (context, _) {
      final home = widget.bridge.home;
      final rooms = records(home['rooms']);
      final peers = records(home['dmPeers']);
      final peopleByJid = {
        for (final row in [...peers, ...records(home['people'])])
          row['jid']: row,
      };
      final people = peopleByJid.values.toList();
      final around = people.where(_around).toList();
      final away = people.where((p) => !_around(p)).toList();
      final live = rooms.where((r) => r['live'] == true).toList();
      final sorted =
          rooms
              .where(
                (r) =>
                    r['groupDm'] != true &&
                    (_count(r, 'unread') + _count(r, 'threadUnread') > 0 ||
                        r['live'] == true ||
                        r['preview'] != null),
              )
              .toList()
            ..sort((a, b) {
              for (final key in ['threadUnread', 'unread', 'lastUpdated']) {
                final cmp = _count(b, key).compareTo(_count(a, key));
                if (cmp != 0) return cmp;
              }
              return _entryName(a).compareTo(_entryName(b));
            });
      final needs = sorted.where((r) => _count(r, 'threadUnread') > 0).toList();
      final activeRows = sorted.where(
        (r) => r['live'] != true && _count(r, 'threadUnread') == 0,
      );
      final happening = [
        ...live,
        ...activeRows.take((3 - live.length).clamp(0, 3)),
      ];
      final direct = [...rooms.where((r) => r['groupDm'] == true), ...peers];
      final current = widget.bridge.call;
      final hasCall = _hasCall(widget.bridge);
      final otherLive = live.where((r) => r['jid'] != current['peerJid']);
      final primary =
          otherLive.firstOrNull ??
          sorted.where((r) => r['jid'] != current['peerJid']).firstOrNull;
      final now = DateTime.now();
      final (greeting, quiet) = switch (now.hour) {
        >= 5 && < 11 => (
          "Good morning",
          "Everything's quiet. A good moment to start something.",
        ),
        >= 11 && < 17 => (
          'Good afternoon',
          'All caught up. The room is yours.',
        ),
        >= 17 && < 22 => (
          'Good evening',
          'All caught up. Maybe say hi to someone.',
        ),
        _ => (
          'Late one tonight',
          'Quiet night. Sleep well — or send a long-form note.',
        ),
      };
      final user =
          widget.bridge.session['jid']?.toString().split('@').first ?? '';
      final unread = [
        ...rooms,
        ...peers,
      ].fold(0, (n, r) => n + _count(r, 'unread'));
      final replies = rooms.fold(0, (n, r) => n + _count(r, 'threadUnread'));
      final calls =
          live.length +
          (hasCall && !live.any((r) => r['jid'] == current['peerJid']) ? 1 : 0);
      final huddled = people.where((p) => p['inHuddle'] == true).length;
      final summary = [
        if (unread > 0)
          '$unread unread ${unread == 1 ? 'message' : 'messages'}',
        if (replies > 0)
          '$replies thread ${replies == 1 ? 'reply' : 'replies'}',
        if (calls > 0) '$calls active ${calls == 1 ? 'call' : 'calls'}',
        if (around.isNotEmpty)
          '${around.length} ${around.length == 1 ? 'friend' : 'friends'} online',
      ];
      final c = Theme.of(context).colorScheme;
      final t = Theme.of(context).textTheme;
      final heading = Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          _Kicker(
            [
              MaterialLocalizations.of(context).formatMediumDate(now),
              if (around.isNotEmpty) '${around.length} around',
              if (huddled > 0) '$huddled in a huddle',
            ].join(' · '),
          ),
          const SizedBox(height: 6),
          Text(
            '$greeting${user.isEmpty ? '' : ', $user'}.',
            style: t.headlineMedium?.copyWith(
              fontFamily: 'Bricolage Grotesque',
              fontSize: 30,
              height: 1.18,
              fontWeight: FontWeight.w700,
            ),
          ),
          const SizedBox(height: 6),
          Text(
            summary.isEmpty ? quiet : summary.join(' · '),
            style: t.bodyMedium?.copyWith(
              fontSize: 15,
              color: c.onSurfaceVariant,
            ),
          ),
        ],
      );
      final cta = FilledButton.icon(
        onPressed: () =>
            primary == null ? widget.select('rooms') : _enter(primary),
        label: Text(
          primary == null
              ? 'Browse rooms'
              : primary['live'] == true && !hasCall
              ? 'Join ${_entryName(primary)} call'
              : 'Jump into ${_entryName(primary)}',
        ),
        icon: const Icon(Icons.arrow_forward, size: 16),
        iconAlignment: IconAlignment.end,
      );
      return Row(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Expanded(
            child: SingleChildScrollView(
              child: Align(
                alignment: Alignment.topLeft,
                child: ConstrainedBox(
                  constraints: const BoxConstraints(maxWidth: 1000),
                  child: Padding(
                    padding: const EdgeInsets.symmetric(
                      horizontal: 34,
                      vertical: 32,
                    ),
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.stretch,
                      children: [
                        LayoutBuilder(
                          builder: (context, bounds) =>
                              bounds.maxWidth < 580 ||
                                  MediaQuery.textScalerOf(context).scale(1) >
                                      1.2
                              ? Column(
                                  crossAxisAlignment: CrossAxisAlignment.start,
                                  children: [
                                    heading,
                                    const SizedBox(height: 14),
                                    cta,
                                  ],
                                )
                              : Row(
                                  crossAxisAlignment: CrossAxisAlignment.end,
                                  children: [
                                    Expanded(child: heading),
                                    const SizedBox(width: 12),
                                    Flexible(
                                      flex: 0,
                                      child: ConstrainedBox(
                                        constraints: BoxConstraints(
                                          maxWidth: bounds.maxWidth * .42,
                                        ),
                                        child: cta,
                                      ),
                                    ),
                                  ],
                                ),
                        ),
                        if (home['settled'] != true &&
                            rooms.isEmpty &&
                            peers.isEmpty)
                          const Padding(
                            padding: EdgeInsets.all(32),
                            child: LinearProgressIndicator(),
                          )
                        else if (summary.isEmpty && rooms.isEmpty) ...[
                          const SizedBox(height: 42),
                          const Center(child: WaddleMark(size: 88)),
                          const SizedBox(height: 12),
                          Text(
                            'Nothing here yet',
                            textAlign: TextAlign.center,
                            style: t.titleLarge,
                          ),
                          const Text(
                            'Join a room and your mentions, events and people show up here.',
                            textAlign: TextAlign.center,
                          ),
                        ],
                        if (happening.isNotEmpty || hasCall)
                          _HomeSection(
                            title: 'Happening now',
                            live: true,
                            children: [
                              if (hasCall &&
                                  !live.any(
                                    (r) => r['jid'] == current['peerJid'],
                                  ))
                                _DashboardCard(
                                  child: ListTile(
                                    leading: Icon(
                                      Icons.phone_in_talk_outlined,
                                      color: c.tertiary,
                                    ),
                                    title: Text(
                                      '${current['peerName'] ?? current['peerJid'] ?? 'Current call'}',
                                    ),
                                    subtitle: Text(
                                      current['state'] == 'incoming'
                                          ? 'Incoming call'
                                          : 'Return to call',
                                    ),
                                    onTap: () => widget.select('call'),
                                  ),
                                ),
                              for (final row in happening)
                                _RoomActivity(
                                  row: row,
                                  onTap: () =>
                                      row['live'] == true &&
                                          hasCall &&
                                          row['jid'] == current['peerJid']
                                      ? widget.select('call')
                                      : _enter(row),
                                ),
                            ],
                          ),
                        if (needs.isNotEmpty)
                          _HomeSection(
                            title: 'Needs someone like you',
                            children: [
                              for (final row in needs)
                                _HomePerson(
                                  row: row,
                                  caption:
                                      '${_count(row, 'threadUnread')} unread thread replies',
                                  onTap: () => widget.open(row, true),
                                ),
                            ],
                          ),
                        _HomeSection(
                          title: 'Direct messages',
                          empty: 'No direct messages yet.',
                          children: [
                            for (final row in direct)
                              _HomePerson(
                                row: row,
                                caption: '${row['preview'] ?? _presence(row)}',
                                onTap: () =>
                                    widget.open(row, row['groupDm'] == true),
                              ),
                          ],
                        ),
                        _HomeSection(
                          title:
                              'Around${around.isEmpty ? '' : ' · ${around.length}'}',
                          empty: away.isEmpty
                              ? 'No contacts yet.'
                              : 'Nobody is around right now.',
                          children: [
                            for (final p in around)
                              _HomePerson(
                                row: p,
                                caption: _presence(p),
                                onTap: () => widget.open(p, false),
                              ),
                          ],
                        ),
                        if (away.isNotEmpty)
                          _HomeSection(
                            title: 'Away and offline · ${away.length}',
                            children: [
                              for (final p in away)
                                _HomePerson(
                                  row: p,
                                  caption: _presence(p),
                                  onTap: () => widget.open(p, false),
                                ),
                            ],
                          ),
                      ],
                    ),
                  ),
                ),
              ),
            ),
          ),
          if (MediaQuery.sizeOf(context).width >= 1280) ...[
            const VerticalDivider(width: 1, thickness: 1),
            SizedBox(
              width: 360,
              child: _HomeContext(
                bridge: widget.bridge,
                people: people,
                open: widget.open,
                calendar: () => widget.select('events'),
              ),
            ),
          ],
        ],
      );
    },
  );
}

class _Kicker extends StatelessWidget {
  const _Kicker(this.text, {this.live = false});
  final String text;
  final bool live;
  @override
  Widget build(BuildContext context) => Text(
    text,
    style: TextStyle(
      fontSize: 13,
      height: 1.35,
      fontWeight: FontWeight.w700,
      color: live
          ? Theme.of(context).colorScheme.tertiary
          : Theme.of(context).colorScheme.onSurfaceVariant,
    ),
  );
}

class _HomeSection extends StatelessWidget {
  const _HomeSection({
    required this.title,
    required this.children,
    this.live = false,
    this.empty,
  });
  final String title;
  final String? empty;
  final bool live;
  final List<Widget> children;
  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.only(top: 30),
    child: Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        _Kicker(title, live: live),
        const SizedBox(height: 12),
        if (children.isEmpty && empty != null)
          CustomPaint(
            painter: _EmptyBorder(Theme.of(context).colorScheme.outlineVariant),
            child: Padding(
              padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 24),
              child: Text(empty!, style: Theme.of(context).textTheme.bodySmall),
            ),
          )
        else
          LayoutBuilder(
            builder: (context, bounds) {
              final maxCols = live ? children.length.clamp(1, 3) : 2;
              final cols =
                  ((bounds.maxWidth + 12) /
                          (220 + 12) /
                          MediaQuery.textScalerOf(context).scale(1))
                      .floor()
                      .clamp(1, maxCols);
              final width = (bounds.maxWidth - (cols - 1) * 12) / cols;
              return Column(
                children: [
                  for (var i = 0; i < children.length; i += cols)
                    Padding(
                      padding: EdgeInsets.only(top: i == 0 ? 0 : 10),
                      child: IntrinsicHeight(
                        child: Row(
                          crossAxisAlignment: CrossAxisAlignment.stretch,
                          children: [
                            for (
                              var col = 0;
                              col < cols && i + col < children.length;
                              col++
                            ) ...[
                              if (col > 0) const SizedBox(width: 12),
                              SizedBox(width: width, child: children[i + col]),
                            ],
                          ],
                        ),
                      ),
                    ),
                ],
              );
            },
          ),
      ],
    ),
  );
}

class _EmptyBorder extends CustomPainter {
  const _EmptyBorder(this.color);
  final Color color;
  @override
  void paint(Canvas canvas, Size size) {
    final rect = (Offset.zero & size).deflate(.5);
    final path = Path()
      ..addRRect(RRect.fromRectAndRadius(rect, const Radius.circular(12)));
    final paint = Paint()
      ..color = color
      ..style = PaintingStyle.stroke
      ..strokeWidth = 1;
    final edge = path.computeMetrics().first;
    for (double offset = 0; offset < edge.length; offset += 5) {
      canvas.drawPath(edge.extractPath(offset, offset + 3), paint);
    }
  }

  @override
  bool shouldRepaint(_EmptyBorder oldDelegate) => color != oldDelegate.color;
}

class _DashboardCard extends StatelessWidget {
  const _DashboardCard({required this.child});
  final Widget child;
  @override
  Widget build(BuildContext context) {
    final c = Theme.of(context).colorScheme;
    return Material(
      color: c.surfaceContainer,
      shape: RoundedRectangleBorder(
        borderRadius: BorderRadius.circular(12),
        side: BorderSide(color: c.outlineVariant),
      ),
      clipBehavior: Clip.antiAlias,
      child: child,
    );
  }
}

class _RoomActivity extends StatelessWidget {
  const _RoomActivity({required this.row, required this.onTap});
  final Map<String, dynamic> row;
  final VoidCallback onTap;
  @override
  Widget build(BuildContext context) {
    final live = row['live'] == true;
    final unread = _count(row, 'unread');
    final detail = row['preview'];
    return _DashboardCard(
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            _Kicker(
              live
                  ? 'Live call'
                  : [
                      row['forum'] == true ? 'Forum' : 'Room',
                      if (unread > 0) '$unread unread',
                    ].join(' · '),
              live: live,
            ),
            const SizedBox(height: 8),
            Row(
              children: [
                Icon(
                  live ? Icons.phone_in_talk_outlined : _roomIcon(row),
                  size: 16,
                ),
                const SizedBox(width: 6),
                Expanded(
                  child: Text(
                    _entryName(row),
                    maxLines: 2,
                    overflow: TextOverflow.ellipsis,
                    style: Theme.of(context).textTheme.titleMedium?.copyWith(
                      fontWeight: FontWeight.bold,
                    ),
                  ),
                ),
              ],
            ),
            if (detail != null)
              Padding(
                padding: const EdgeInsets.only(top: 6),
                child: Text(
                  '$detail',
                  maxLines: 2,
                  overflow: TextOverflow.ellipsis,
                  style: Theme.of(context).textTheme.bodySmall,
                ),
              ),
            const SizedBox(height: 14),
            OutlinedButton.icon(
              style: OutlinedButton.styleFrom(
                foregroundColor: Theme.of(context).colorScheme.onSurface,
                side: BorderSide(
                  color: Theme.of(context).colorScheme.outlineVariant,
                ),
                minimumSize: const Size(44, 36),
                visualDensity: VisualDensity.compact,
              ),
              onPressed: onTap,
              iconAlignment: IconAlignment.end,
              icon: const Icon(Icons.arrow_forward, size: 14),
              label: Text(live ? 'Open call' : 'Enter'),
            ),
          ],
        ),
      ),
    );
  }
}

class _HomePerson extends StatelessWidget {
  const _HomePerson({
    required this.row,
    required this.onTap,
    required this.caption,
  });
  final Map<String, dynamic> row;
  final String caption;
  final VoidCallback onTap;
  @override
  Widget build(BuildContext context) => _DashboardCard(
    child: InkWell(
      onTap: onTap,
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 11),
        child: Row(
          children: [
            _AccountAvatar(
              jid: row['groupDm'] == true ? '' : row['jid'] as String,
              name: _entryName(row),
              size: 34,
              online: _around(row),
            ),
            const SizedBox(width: 10),
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(
                    _entryName(row),
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: Theme.of(context).textTheme.titleSmall,
                  ),
                  Text(
                    caption,
                    style: Theme.of(context).textTheme.bodySmall?.copyWith(
                      color: Theme.of(context).colorScheme.onSurfaceVariant,
                    ),
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                  ),
                ],
              ),
            ),
            if (_count(row, 'unread') > 0)
              Badge(
                label: Text('${row['unread']}'),
                backgroundColor: Theme.of(context).colorScheme.primary,
                textColor: Theme.of(context).colorScheme.onPrimary,
              ),
          ],
        ),
      ),
    ),
  );
}
