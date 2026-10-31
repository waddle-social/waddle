import 'dart:async';
import 'dart:convert';
import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../bridge.dart';

/// Presentation only. Native conversation view models retain ownership of
/// delivery, encryption, history cursors, read markers and XMPP action IDs.
class ConversationScreen extends StatefulWidget {
  const ConversationScreen({
    super.key,
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
  State<ConversationScreen> createState() => _ConversationScreenState();
}

class _ConversationScreenState extends State<ConversationScreen>
    with WidgetsBindingObserver, RouteAware, RestorationMixin {
  static int _nextViewId = 0;
  final _restorableDraft = RestorableTextEditingController();
  final _restorableMentions = RestorableString('[]');
  final _savedScrollOffset = RestorableDouble(0);
  TextEditingController? _observedDraft;
  TextEditingController get _draft => _restorableDraft.value;
  final _scroll = ScrollController();
  final _focus = FocusNode();
  final _imageBytes = <String, Future<Uint8List?>>{};
  final _avatarBytes = <String, Future<Uint8List?>>{};
  Map<String, dynamic>? _reply;
  Map<String, dynamic>? _edit;
  String? _error;
  bool _sending = false;
  bool _startingCall = false;
  bool _opening = true;
  bool _openedOnce = false;
  bool _active = true;
  bool _atNewest = true;
  String? _mentionQuery;
  List<Map<String, dynamic>> _mentions = [];
  Timer? _mentionTimer;
  int _mentionGeneration = 0;
  String _previousDraft = '';
  final _mentionRefs = <Map<String, dynamic>>[];
  Map<String, dynamic>? _lastEdit;
  int _restoredEditAttempt = 0;
  Map<String, dynamic> _roomCallState = {};
  String _roomCallVersion = '';
  final _viewId = 'conversation-${++_nextViewId}';
  ModalRoute<void>? _route;

  Map<String, dynamic> get _args => {
    'jid': widget.jid,
    'group': widget.group,
    'viewId': _viewId,
    'atNewestEdge': _atNewest,
    if (widget.threadId != null) 'threadId': widget.threadId,
  };

  Map<String, dynamic> get _data {
    final value = widget.bridge.conversation(widget.jid);
    return value['viewId'] != null && value['viewId'] != _viewId
        ? const {}
        : value;
  }

  bool get _isGroupDm => _maps(
    widget.bridge.home['rooms'],
  ).any((room) => room['jid'] == widget.jid && room['groupDm'] == true);

  @override
  String get restorationId =>
      'conversation:${widget.jid}:${widget.threadId ?? 'main'}';

  @override
  void restoreState(RestorationBucket? oldBucket, bool initialRestore) {
    _observedDraft?.removeListener(_onDraft);
    registerForRestoration(_restorableDraft, 'draft');
    registerForRestoration(_restorableMentions, 'mentions');
    registerForRestoration(_savedScrollOffset, 'scroll-edge');
    _observedDraft = _draft;
    _draft.addListener(_onDraft);
    _previousDraft = _draft.text;
    _atNewest = _savedScrollOffset.value <= 72;
    _mentionRefs.clear();
    try {
      for (final ref in _maps(jsonDecode(_restorableMentions.value))) {
        final start = ref['start'];
        final end = ref['end'];
        if (start is int &&
            end is int &&
            start >= 0 &&
            end >= start &&
            end <= _draft.text.length &&
            _draft.text.substring(start, end) == ref['text'] &&
            ref['uri'] is String) {
          _mentionRefs.add(ref);
        }
      }
    } on FormatException {
      _restorableMentions.value = '[]';
    }
  }

  void _saveMentions() {
    _restorableMentions.value = jsonEncode(_mentionRefs);
  }

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    widget.bridge.addListener(_bridgeChanged);
    _scroll.addListener(_onScroll);
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (mounted && _active) {
        unawaited(_open());
      }
    });
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state == AppLifecycleState.resumed && _active && _openedOnce) {
      unawaited(_command('conversation.visibility', {'visible': true}));
      _readNewest();
    } else if (state == AppLifecycleState.paused && _active && _openedOnce) {
      // System document/image pickers pause this activity. Keep the native
      // model alive so their result remains attached to the same upload.
      unawaited(_command('conversation.visibility', {'visible': false}));
    }
  }

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    final route = ModalRoute.of<void>(context);
    if (route != _route) {
      waddleRoutes.unsubscribe(this);
      _route = route;
      if (route != null) {
        waddleRoutes.subscribe(this, route);
      }
    }
  }

  @override
  void didPushNext() {
    _active = false;
    if (_openedOnce) {
      unawaited(_command('conversation.visibility', {'visible': false}));
    }
  }

  @override
  void didPopNext() {
    _active = true;
    unawaited(_open());
  }

  Future<void> _open() async {
    if (!mounted || !_active) {
      return;
    }
    setState(() => _opening = true);
    await _command('conversation.open');
    _openedOnce = _error == null;
    if (!mounted || !_active) {
      return;
    }
    setState(() => _opening = false);
    await _command('conversation.visibility', {'visible': true});
    _readNewest();
    if (widget.group) {
      unawaited(_refreshRoomCallState());
    }
  }

  Future<void> _refreshRoomCallState() async {
    if (!widget.group || !mounted) {
      return;
    }
    final value = await _command('call.roomState');
    if (mounted && value is Map) {
      setState(() => _roomCallState = Map<String, dynamic>.from(value));
    }
  }

  Future<dynamic> _command(
    String method, [
    Map<String, dynamic> values = const {},
  ]) async {
    try {
      final result = await widget.bridge.invoke(method, {..._args, ...values});
      if (mounted && _error != null) {
        setState(() => _error = null);
      }
      return result;
    } on PlatformException catch (error) {
      if (mounted) {
        setState(() => _error = error.message ?? error.code);
      }
    } catch (error) {
      if (mounted) {
        setState(() => _error = error.toString());
      }
    }
    return null;
  }

  void _readNewest() {
    if (_active) {
      unawaited(_command('conversation.read', {'atNewestEdge': _atNewest}));
    }
  }

  void _onScroll() {
    if (!_scroll.hasClients || !_active) {
      return;
    }
    final position = _scroll.position;
    _savedScrollOffset.value = position.pixels;
    // Both layouts put their live edge at scroll offset zero.
    final atEdge = position.pixels <= 72;
    if (atEdge != _atNewest) {
      setState(() => _atNewest = atEdge);
      _readNewest();
    }
    if (position.extentAfter < 180 &&
        _data['loadingOlder'] != true &&
        _data['reachedStart'] != true) {
      unawaited(_command('conversation.history'));
    }
  }

  void _onDraft() {
    if (!_active) {
      return;
    }
    final textChanged = _draft.text != _previousDraft;
    _reconcileMentions();
    _saveMentions();
    if (textChanged && _draft.text.isNotEmpty) {
      unawaited(_command('conversation.typing'));
    }
    final text = _draft.text;
    final caret = _draft.selection.baseOffset;
    final before = text.substring(0, caret.clamp(0, text.length).toInt());
    final match = RegExp(r'(?:^|\s)@([^\s@]*)$').firstMatch(before);
    final query = widget.group ? match?.group(1) : null;
    if (query == _mentionQuery) {
      return;
    }
    _mentionQuery = query;
    _mentionTimer?.cancel();
    final generation = ++_mentionGeneration;
    if (query == null) {
      setState(() => _mentions = []);
      return;
    }
    _mentionTimer = Timer(const Duration(milliseconds: 250), () async {
      final result = await _command('conversation.mentionSearch', {
        'query': query,
      });
      if (mounted && generation == _mentionGeneration) {
        setState(
          () => _mentions = [
            for (final name in ['everyone', 'here'])
              if (name.startsWith(query.toLowerCase()))
                {'name': name, 'username': name, 'uri': 'xmpp:@$name'},
            ..._maps(result),
          ],
        );
      }
    });
  }

  void _insertMention(Map<String, dynamic> person) {
    final text = _draft.text;
    final caret = _draft.selection.baseOffset.clamp(0, text.length).toInt();
    final before = text.substring(0, caret);
    final match = RegExp(r'@[^\s@]*$').firstMatch(before);
    if (match == null) {
      return;
    }
    final name = _str(
      person['username'],
      _str(person['name'], _str(person['jid']).split('@').first),
    );
    final replacement = '@$name ';
    _draft.value = TextEditingValue(
      text: text.replaceRange(match.start, caret, replacement),
      selection: TextSelection.collapsed(
        offset: match.start + replacement.length,
      ),
    );
    final jid = _str(person['jid']);
    final uri = person['uri'] ?? (jid.isEmpty ? null : 'xmpp:$jid');
    if (uri != null) {
      _mentionRefs.add({
        'start': match.start,
        'end': match.start + replacement.length - 1,
        'text': replacement.trimRight(),
        'uri': uri,
      });
      _saveMentions();
    }
    _focus.requestFocus();
  }

  void _reconcileMentions() {
    final next = _draft.text;
    if (next == _previousDraft) {
      return;
    }
    var prefix = 0;
    while (prefix < next.length &&
        prefix < _previousDraft.length &&
        next.codeUnitAt(prefix) == _previousDraft.codeUnitAt(prefix)) {
      prefix++;
    }
    var suffix = 0;
    while (suffix < next.length - prefix &&
        suffix < _previousDraft.length - prefix &&
        next.codeUnitAt(next.length - 1 - suffix) ==
            _previousDraft.codeUnitAt(_previousDraft.length - 1 - suffix)) {
      suffix++;
    }
    final oldEnd = _previousDraft.length - suffix;
    final delta = next.length - _previousDraft.length;
    _mentionRefs.removeWhere((ref) {
      var start = ref['start'] as int;
      var end = ref['end'] as int;
      if (oldEnd <= start) {
        start += delta;
        end += delta;
      } else if (prefix < end) {
        return true;
      }
      if (start < 0 ||
          end > next.length ||
          next.substring(start, end) != ref['text']) {
        return true;
      }
      ref['start'] = start;
      ref['end'] = end;
      return false;
    });
    _previousDraft = next;
  }

  void _bridgeChanged() {
    if (mounted && _active && widget.group) {
      final rooms = _maps(widget.bridge.home['rooms']);
      final room = rooms.firstWhere(
        (value) => value['jid'] == widget.jid,
        orElse: () => <String, dynamic>{},
      );
      final version =
          '${widget.bridge.call['state']}:${widget.bridge.call['peerJid']}:${room['live']}:${room['video']}:${(room['huddle'] as List?)?.length}';
      if (version != _roomCallVersion) {
        _roomCallVersion = version;
        unawaited(_refreshRoomCallState());
      }
    }
    final composer = widget.bridge.topic('composer');
    final attempt = (composer['attempt'] as num? ?? 0).toInt();
    if (!mounted ||
        composer['jid'] != widget.jid ||
        (composer['viewId'] != null && composer['viewId'] != _viewId) ||
        composer['mode'] != 'editing' ||
        attempt <= _restoredEditAttempt ||
        _lastEdit == null) {
      return;
    }
    _restoredEditAttempt = attempt;
    setState(() {
      _edit = _lastEdit;
      _draft.text = _str(composer['body']);
    });
  }

  Future<void> _send() async {
    final body = _draft.text.trim();
    if (body.isEmpty || _sending) {
      return;
    }
    setState(() => _sending = true);
    final editing = _edit;
    if (editing != null) {
      _lastEdit = editing;
    }
    final trimStart = _draft.text.indexOf(body);
    await _command(
      editing == null ? 'conversation.send' : 'conversation.edit',
      {
        'body': body,
        if (editing != null) 'targetId': editing['id'],
        if (_reply != null) 'replyTo': _reply!['id'],
        'mentions': [
          for (final ref in _mentionRefs)
            if ((ref['start'] as int) >= trimStart &&
                (ref['end'] as int) <= trimStart + body.length)
              {
                'uri': ref['uri'],
                'begin': body
                    .substring(0, (ref['start'] as int) - trimStart)
                    .runes
                    .length,
                'end': body
                    .substring(0, (ref['end'] as int) - trimStart)
                    .runes
                    .length,
              },
        ],
      },
    );
    if (!mounted) {
      return;
    }
    setState(() {
      _sending = false;
      if (_error == null) {
        _draft.clear();
        _reply = null;
        _edit = null;
      }
    });
    if (_error == null) {
      _jumpToNewest();
    }
  }

  void _jumpToNewest() {
    if (_scroll.hasClients) {
      final disableMotion = MediaQuery.disableAnimationsOf(context);
      if (disableMotion) {
        _scroll.jumpTo(0);
      } else {
        unawaited(
          _scroll.animateTo(
            0,
            duration: const Duration(milliseconds: 240),
            curve: Curves.easeOutCubic,
          ),
        );
      }
    }
  }

  @override
  void dispose() {
    _active = false;
    WidgetsBinding.instance.removeObserver(this);
    widget.bridge.removeListener(_bridgeChanged);
    waddleRoutes.unsubscribe(this);
    _mentionTimer?.cancel();
    unawaited(
      widget.bridge
          .invoke('conversation.close', _args)
          .catchError((Object _) => null),
    );
    _observedDraft?.removeListener(_onDraft);
    _restorableDraft.dispose();
    _restorableMentions.dispose();
    _savedScrollOffset.dispose();
    _scroll.dispose();
    _focus.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => AnimatedBuilder(
    animation: widget.bridge,
    builder: (context, _) {
      final data = _data;
      final social =
          data['newestFirst'] == true ||
          widget.bridge.preferences['timeline'] == 'social';
      final messages = _maps(data['messages']);
      final rows = messages.reversed.toList();
      final callState = widget.bridge.call['state'];
      final callBusy =
          callState != null && callState != 'idle' && callState != 'ended';
      final callHere =
          callBusy &&
          (widget.bridge.call['peerJid'] ?? widget.bridge.call['peer']) ==
              widget.jid;
      final joinCall = _roomCallState['controls'] == 'join';
      final typing = (data['typing'] as List? ?? const [])
          .map((value) => value.toString())
          .toList();
      return Scaffold(
        appBar: AppBar(
          title: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(
                widget.threadId == null ? widget.title : 'Thread',
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
              ),
              Text(
                widget.threadId == null
                    ? (widget.group ? 'Community conversation' : widget.jid)
                    : widget.title,
                style: Theme.of(context).textTheme.labelSmall,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
              ),
            ],
          ),
          actions: [
            IconButton(
              tooltip: 'Search messages',
              icon: const Icon(Icons.search),
              onPressed: _search,
            ),
            if (widget.threadId == null)
              IconButton(
                tooltip: callHere
                    ? 'Return to call'
                    : joinCall
                    ? 'Join room call'
                    : 'Start audio call',
                icon: _startingCall
                    ? const SizedBox(
                        width: 20,
                        height: 20,
                        child: CircularProgressIndicator(
                          strokeWidth: 2,
                          semanticsLabel: 'Connecting call',
                        ),
                      )
                    : Icon(
                        joinCall && _roomCallState['video'] == true
                            ? Icons.videocam_outlined
                            : Icons.call_outlined,
                      ),
                onPressed: _startingCall || callBusy && !callHere
                    ? null
                    : callHere
                    ? () => Navigator.of(context).restorablePushNamed('call')
                    : () => _startCall(
                        joinCall && _roomCallState['video'] == true,
                      ),
              ),
            PopupMenuButton<String>(
              tooltip: 'Conversation options',
              onOpened: () {
                if (widget.group) {
                  unawaited(_refreshRoomCallState());
                }
              },
              onSelected: _menu,
              itemBuilder: (context) => [
                const PopupMenuItem(
                  value: 'video',
                  child: Text('Start video call'),
                ),
                if (widget.group)
                  const PopupMenuItem(value: 'members', child: Text('Members')),
                const PopupMenuItem(value: 'threads', child: Text('Threads')),
                const PopupMenuItem(
                  value: 'pins',
                  child: Text('Pinned messages'),
                ),
                const PopupMenuItem(
                  value: 'notifications',
                  child: Text('Notifications'),
                ),
                if (widget.group && !_isGroupDm && data['canConfigure'] == true)
                  const PopupMenuItem(
                    value: 'settings',
                    child: Text('Room settings'),
                  ),
                if (_isGroupDm)
                  const PopupMenuItem(
                    value: 'rename',
                    child: Text('Rename group'),
                  ),
                if (_isGroupDm)
                  const PopupMenuItem(
                    value: 'invite',
                    child: Text('Invite to group'),
                  ),
                if (_isGroupDm)
                  const PopupMenuItem(
                    value: 'leave',
                    child: Text('Leave group'),
                  ),
                if (_roomCallState['canLeaveRetained'] == true)
                  const PopupMenuItem(
                    value: 'leaveRoomCall',
                    child: Text('Leave room call'),
                  ),
              ],
            ),
          ],
        ),
        body: SafeArea(
          top: false,
          child: Column(
            children: [
              if (_error != null)
                MaterialBanner(
                  content: Text(_error!),
                  leading: const Icon(Icons.error_outline),
                  actions: [
                    TextButton(
                      onPressed: () => setState(() => _error = null),
                      child: const Text('Dismiss'),
                    ),
                  ],
                ),
              if (data['uploadState'] == 'uploading')
                const LinearProgressIndicator(
                  semanticsLabel: 'Encrypting and uploading attachment',
                ),
              if (data['uploadState'] == 'failed' ||
                  data['uploadState'] == 'toolarge')
                Padding(
                  padding: const EdgeInsets.all(8),
                  child: Text(
                    data['uploadState'] == 'toolarge'
                        ? 'This file exceeds the server upload limit.'
                        : 'The attachment could not be uploaded. Please try again.',
                  ),
                ),
              if (_roomCallState['controls'] == 'join' &&
                  widget.threadId == null)
                Material(
                  color: Theme.of(context).colorScheme.secondaryContainer,
                  child: ListTile(
                    leading: Icon(
                      _roomCallState['video'] == true
                          ? Icons.videocam_outlined
                          : Icons.call_outlined,
                    ),
                    title: Text(
                      '${_roomCallState['participantCount'] ?? 0} in the room call',
                    ),
                    trailing: TextButton(
                      onPressed: _startingCall
                          ? null
                          : () => _startCall(_roomCallState['video'] == true),
                      child: const Text('Join'),
                    ),
                  ),
                ),
              if (social) _composer(),
              Expanded(
                child: Stack(
                  children: [
                    if ((_opening || data['loading'] == true) && rows.isEmpty)
                      const Center(
                        child: CircularProgressIndicator(
                          semanticsLabel: 'Loading messages',
                        ),
                      )
                    else if (rows.isEmpty)
                      Center(
                        child: Padding(
                          padding: const EdgeInsets.all(32),
                          child: Column(
                            mainAxisSize: MainAxisSize.min,
                            children: [
                              Icon(
                                Icons.forum_outlined,
                                size: 48,
                                color: Theme.of(context).colorScheme.primary,
                              ),
                              const SizedBox(height: 16),
                              Text(
                                'Start the conversation',
                                style: Theme.of(context).textTheme.titleLarge,
                              ),
                              const SizedBox(height: 8),
                              const Text(
                                'Send a message, share a file, or say hello.',
                                textAlign: TextAlign.center,
                              ),
                            ],
                          ),
                        ),
                      )
                    else
                      ListView.builder(
                        restorationId: 'timeline:$social',
                        key: PageStorageKey(
                          '${widget.jid}/${widget.threadId ?? ''}/$social',
                        ),
                        controller: _scroll,
                        reverse: !social,
                        keyboardDismissBehavior:
                            ScrollViewKeyboardDismissBehavior.onDrag,
                        padding: const EdgeInsets.symmetric(
                          horizontal: 12,
                          vertical: 12,
                        ),
                        itemCount: rows.length + 1,
                        itemBuilder: (context, index) {
                          if (index == rows.length) {
                            return Center(
                              child: Padding(
                                padding: const EdgeInsets.all(12),
                                child: data['loadingOlder'] == true
                                    ? const SizedBox(
                                        width: 20,
                                        height: 20,
                                        child: CircularProgressIndicator(
                                          strokeWidth: 2,
                                        ),
                                      )
                                    : data['reachedStart'] == true
                                    ? Text(
                                        'Beginning of conversation',
                                        style: Theme.of(
                                          context,
                                        ).textTheme.labelSmall,
                                      )
                                    : TextButton(
                                        onPressed: () =>
                                            _command('conversation.history'),
                                        child: const Text(
                                          'Load earlier messages',
                                        ),
                                      ),
                              ),
                            );
                          }
                          final message = rows[index];
                          final older = index + 1 < rows.length
                              ? rows[index + 1]
                              : null;
                          final date = _date(message);
                          final olderDate = older == null ? null : _date(older);
                          final sameDay =
                              date != null &&
                              olderDate != null &&
                              date.year == olderDate.year &&
                              date.month == olderDate.month &&
                              date.day == olderDate.day;
                          final grouped =
                              older != null &&
                              older['author'] == message['author'] &&
                              sameDay &&
                              date != null &&
                              olderDate != null &&
                              date.difference(olderDate).inMinutes.abs() < 5;
                          final newerDate = index > 0
                              ? _date(rows[index - 1])
                              : null;
                          final startsSocialDay =
                              date != null &&
                              (newerDate == null ||
                                  date.year != newerDate.year ||
                                  date.month != newerDate.month ||
                                  date.day != newerDate.day);
                          final separator = message.containsKey('daySeparator')
                              ? DateTime.tryParse(_str(message['daySeparator']))
                              : (social ? startsSocialDay : !sameDay)
                              ? date
                              : null;
                          return Column(
                            children: [
                              if (separator != null) _dayLabel(separator),
                              if (message['unreadDivider'] == true)
                                const Padding(
                                  padding: EdgeInsets.symmetric(vertical: 8),
                                  child: Row(
                                    children: [
                                      Expanded(child: Divider()),
                                      Padding(
                                        padding: EdgeInsets.symmetric(
                                          horizontal: 12,
                                        ),
                                        child: Text('New messages'),
                                      ),
                                      Expanded(child: Divider()),
                                    ],
                                  ),
                                ),
                              _message(
                                message,
                                message.containsKey('startsGroup')
                                    ? message['startsGroup'] != true
                                    : grouped,
                              ),
                            ],
                          );
                        },
                      ),
                    if (!_atNewest)
                      Positioned(
                        right: 12,
                        bottom: 12,
                        child: FloatingActionButton.small(
                          heroTag: 'latest-${widget.jid}-${widget.threadId}',
                          tooltip: 'Jump to latest message',
                          onPressed: _jumpToNewest,
                          child: Icon(
                            social ? Icons.arrow_upward : Icons.arrow_downward,
                          ),
                        ),
                      ),
                  ],
                ),
              ),
              if (typing.isNotEmpty)
                Semantics(
                  liveRegion: true,
                  child: Padding(
                    padding: const EdgeInsets.symmetric(
                      horizontal: 20,
                      vertical: 4,
                    ),
                    child: Align(
                      alignment: Alignment.centerLeft,
                      child: Text(
                        '${typing.join(', ')} ${typing.length == 1 ? 'is' : 'are'} typing…',
                        style: Theme.of(context).textTheme.labelSmall,
                      ),
                    ),
                  ),
                ),
              if (!social) _composer(),
            ],
          ),
        ),
      );
    },
  );

  Widget _dayLabel(DateTime date) {
    final now = DateTime.now();
    final today = DateTime(now.year, now.month, now.day);
    final day = DateTime(date.year, date.month, date.day);
    final label = day == today
        ? 'Today'
        : day == today.subtract(const Duration(days: 1))
        ? 'Yesterday'
        : MaterialLocalizations.of(context).formatMediumDate(date);
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 16),
      child: Text(label, style: Theme.of(context).textTheme.labelMedium),
    );
  }

  Widget _message(Map<String, dynamic> message, bool grouped) {
    final colors = Theme.of(context).colorScheme;
    final mine = message['mine'] == true;
    final deleted = message['deleted'] == true;
    final actionable = !deleted && message['pending'] != true;
    final author = _str(message['author'], mine ? 'You' : 'Unknown');
    final date = _date(message);
    final reactions = _maps(message['reactions']);
    final files = _maps(message['files']);
    final thread = _str(
      message['threadTargetId'],
      _str(message['threadId'], _str(message['id'])),
    );
    final replies = _maps(
      _data['threads'],
    ).where((value) => value['threadId'] == thread).toList();
    final replyCount =
        (message['replyCount'] as num?)?.toInt() ??
        (replies.isEmpty
            ? 0
            : (replies.first['replyCount'] as num? ?? 0).toInt());
    final pinned =
        message['pinned'] == true ||
        (_data['pinnedIds'] as List? ?? const []).contains(message['id']);
    return Padding(
      key: ValueKey(message['presentationId'] ?? message['id']),
      padding: EdgeInsets.only(top: grouped ? 2 : 12, bottom: 2),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          SizedBox(
            width: 40,
            child: grouped
                ? null
                : Semantics(
                    label: 'Profile of $author',
                    button: true,
                    child: InkWell(
                      borderRadius: BorderRadius.circular(20),
                      onTap: () => _profile(message),
                      child: _authorAvatar(message, author, mine),
                    ),
                  ),
          ),
          const SizedBox(width: 8),
          Expanded(
            child: GestureDetector(
              behavior: HitTestBehavior.opaque,
              onLongPress: actionable ? () => _actions(message) : null,
              child: Container(
                padding: const EdgeInsets.symmetric(
                  horizontal: 12,
                  vertical: 8,
                ),
                decoration: BoxDecoration(
                  color: mine
                      ? colors.primaryContainer.withValues(alpha: .45)
                      : colors.surfaceContainerLow,
                  borderRadius: BorderRadius.circular(16),
                  border: pinned
                      ? Border.all(color: colors.primary.withValues(alpha: .4))
                      : null,
                ),
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    if (!grouped)
                      Wrap(
                        crossAxisAlignment: WrapCrossAlignment.center,
                        spacing: 8,
                        children: [
                          InkWell(
                            onTap: () => _profile(message),
                            child: Text(
                              author,
                              style: Theme.of(context).textTheme.labelLarge,
                            ),
                          ),
                          if (date != null)
                            Text(
                              MaterialLocalizations.of(
                                context,
                              ).formatTimeOfDay(TimeOfDay.fromDateTime(date)),
                              style: Theme.of(context).textTheme.labelSmall
                                  ?.copyWith(color: colors.onSurfaceVariant),
                            ),
                          if (message['edited'] == true)
                            Text(
                              'edited',
                              style: Theme.of(context).textTheme.labelSmall,
                            ),
                          if (pinned)
                            Icon(
                              Icons.push_pin,
                              size: 14,
                              color: colors.primary,
                            ),
                        ],
                      ),
                    if (message['replyTo'] != null && !deleted)
                      Padding(
                        padding: const EdgeInsets.only(top: 4, bottom: 6),
                        child: Container(
                          padding: const EdgeInsets.all(8),
                          decoration: BoxDecoration(
                            color: colors.surfaceContainerHighest,
                            borderRadius: BorderRadius.circular(8),
                          ),
                          child: Column(
                            crossAxisAlignment: CrossAxisAlignment.start,
                            children: [
                              Text(
                                '↳ ${_str((message['replyPreview'] as Map?)?['author'], 'Reply')}',
                                style: Theme.of(context).textTheme.labelSmall
                                    ?.copyWith(color: colors.primary),
                              ),
                              if (message['replyPreview'] is Map)
                                Text(
                                  _str(
                                    (message['replyPreview'] as Map)['body'],
                                  ),
                                  maxLines: 2,
                                  overflow: TextOverflow.ellipsis,
                                  style: Theme.of(context).textTheme.bodySmall,
                                ),
                            ],
                          ),
                        ),
                      ),
                    if (deleted)
                      Text(
                        'Message removed',
                        style: TextStyle(
                          fontStyle: FontStyle.italic,
                          color: colors.onSurfaceVariant,
                        ),
                      )
                    else ...[
                      if (message['isCallRow'] == true)
                        _callRow(message)
                      else if (_str(message['body']).isNotEmpty)
                        _richBody(message),
                      for (final file in files) _attachment(message, file),
                      for (final preview in _maps(message['linkPreviews']))
                        _linkPreview(preview),
                    ],
                    if (message['pending'] == true ||
                        message['failed'] == true ||
                        message['rejected'] == true ||
                        message['queued'] == true)
                      Padding(
                        padding: const EdgeInsets.only(top: 6),
                        child: Row(
                          mainAxisSize: MainAxisSize.min,
                          children: [
                            Icon(
                              message['failed'] == true ||
                                      message['rejected'] == true
                                  ? Icons.error_outline
                                  : Icons.schedule,
                              size: 14,
                              color: colors.onSurfaceVariant,
                            ),
                            const SizedBox(width: 4),
                            Text(
                              message['queued'] == true
                                  ? 'Queued · waiting for connection'
                                  : message['failed'] == true ||
                                        message['rejected'] == true
                                  ? 'Not sent'
                                  : 'Sending…',
                              style: Theme.of(context).textTheme.labelSmall,
                            ),
                            if (message['failed'] == true)
                              TextButton(
                                onPressed: () => _command(
                                  'conversation.retry',
                                  {'localId': message['localId']},
                                ),
                                child: const Text('Retry'),
                              ),
                          ],
                        ),
                      ),
                    if (!deleted && reactions.isNotEmpty)
                      Padding(
                        padding: const EdgeInsets.only(top: 6),
                        child: Wrap(
                          spacing: 6,
                          runSpacing: 4,
                          children: [
                            for (final reaction in reactions)
                              FilterChip(
                                visualDensity: VisualDensity.compact,
                                selected: reaction['mine'] == true,
                                showCheckmark: false,
                                label: Text(
                                  '${_str(reaction['emoji'])} ${reaction['count'] ?? 0}',
                                ),
                                onSelected: (_) =>
                                    _command('conversation.reaction', {
                                      'targetId': message['id'],
                                      'emoji': reaction['emoji'],
                                    }),
                              ),
                          ],
                        ),
                      ),
                    if (!deleted && widget.threadId == null && replyCount > 0)
                      TextButton.icon(
                        onPressed: () => _thread(thread),
                        icon: const Icon(Icons.forum_outlined, size: 16),
                        label: Text(
                          '$replyCount ${replyCount == 1 ? 'reply' : 'replies'}',
                        ),
                      ),
                    if (message['safetyScores'] != null)
                      TextButton.icon(
                        onPressed: () => _safetyScores(message['safetyScores']),
                        icon: const Icon(Icons.shield_outlined, size: 16),
                        label: const Text('Safety scores'),
                      ),
                  ],
                ),
              ),
            ),
          ),
          SizedBox(
            width: 32,
            child: !actionable
                ? null
                : IconButton(
                    tooltip: 'Message actions',
                    visualDensity: VisualDensity.compact,
                    icon: const Icon(Icons.more_horiz, size: 18),
                    onPressed: () => _actions(message),
                  ),
          ),
        ],
      ),
    );
  }

  Widget _authorAvatar(Map<String, dynamic> message, String author, bool mine) {
    final colors = Theme.of(context).colorScheme;
    Widget avatar(Uint8List? bytes) => CircleAvatar(
      radius: 18,
      backgroundColor: mine
          ? colors.primaryContainer
          : colors.secondaryContainer,
      backgroundImage: bytes == null ? null : MemoryImage(bytes),
      child: bytes != null
          ? null
          : Text(
              author.isEmpty
                  ? '?'
                  : String.fromCharCode(author.runes.first).toUpperCase(),
              style: TextStyle(
                color: mine
                    ? colors.onPrimaryContainer
                    : colors.onSecondaryContainer,
              ),
            ),
    );
    final jid = _str(message['authorJid']);
    if (jid.isEmpty) {
      return avatar(null);
    }
    final ownAvatar = widget.bridge.topic('profile')['avatar'];
    if (jid == widget.bridge.session['jid'] && ownAvatar is Uint8List) {
      return avatar(ownAvatar);
    }
    return FutureBuilder<Uint8List?>(
      future: _avatarBytes.putIfAbsent(jid, () async {
        try {
          final value = await widget.bridge.invoke('profile.avatarBytes', {
            'jid': jid,
          });
          if (value is Uint8List) {
            return value;
          }
          if (value is List) {
            return Uint8List.fromList(value.cast<int>());
          }
        } catch (_) {
          // An unavailable avatar uses initials and does not prevent reading.
        }
        return null;
      }),
      builder: (context, snapshot) => avatar(snapshot.data),
    );
  }

  Widget _attachment(Map<String, dynamic> message, Map<String, dynamic> file) {
    final name = _str(file['name'], 'Attachment');
    final mime = _str(file['mimeType']);
    final id = '${message['id']}/${file['id'] ?? name}';
    final values = {
      'targetId': message['id'],
      'fileId': file['id'],
      'index': _maps(message['files']).indexOf(file),
    };
    final isImage = mime.startsWith('image/');
    return Padding(
      padding: const EdgeInsets.only(top: 8),
      child: isImage
          ? InkWell(
              onTap: () => _command('conversation.fileOpen', values),
              child: Semantics(
                label: name,
                image: true,
                child: ClipRRect(
                  borderRadius: BorderRadius.circular(12),
                  child: file['encrypted'] != true && file['url'] != null
                      ? Image.network(
                          _str(file['url']),
                          fit: BoxFit.contain,
                          height: 220,
                          semanticLabel: name,
                          errorBuilder: (_, error, stack) => _fileTile(
                            name,
                            Icons.broken_image_outlined,
                            () => _command('conversation.fileOpen', values),
                          ),
                        )
                      : FutureBuilder<Uint8List?>(
                          // Native code decrypts XEP-0448 files into memory. Never hand an
                          // encrypted source URL directly to Image.network.
                          future: _imageBytes.putIfAbsent(id, () async {
                            final value = await _command(
                              'conversation.fileBytes',
                              values,
                            );
                            if (value is Uint8List) {
                              return value;
                            }
                            if (value is List) {
                              return Uint8List.fromList(value.cast<int>());
                            }
                            return null;
                          }),
                          builder: (context, snapshot) {
                            if (snapshot.hasData) {
                              return Image.memory(
                                snapshot.data!,
                                fit: BoxFit.contain,
                                height: 220,
                                semanticLabel: name,
                              );
                            }
                            if (snapshot.connectionState !=
                                ConnectionState.done) {
                              return const SizedBox(
                                height: 100,
                                child: Center(
                                  child: CircularProgressIndicator(),
                                ),
                              );
                            }
                            return _fileTile(
                              name,
                              Icons.image_outlined,
                              () => _command('conversation.fileOpen', values),
                            );
                          },
                        ),
                ),
              ),
            )
          : _fileTile(
              name,
              file['encrypted'] == true
                  ? Icons.lock_outline
                  : Icons.insert_drive_file_outlined,
              () => _command('conversation.fileOpen', values),
            ),
    );
  }

  Widget _fileTile(String name, IconData icon, VoidCallback onTap) => ListTile(
    contentPadding: const EdgeInsets.symmetric(horizontal: 8),
    shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(12)),
    tileColor: Theme.of(context).colorScheme.surfaceContainerHighest,
    leading: Icon(icon),
    title: Text(name, maxLines: 2, overflow: TextOverflow.ellipsis),
    trailing: const Icon(Icons.open_in_new, size: 18),
    onTap: onTap,
  );

  Widget _callRow(Map<String, dynamic> message) {
    final call = message['call'] is Map ? message['call'] as Map : const {};
    final ended = message['callEnded'] is Map
        ? message['callEnded'] as Map
        : null;
    final video = (call['media'] as List? ?? []).contains('video');
    final duration = _str(
      ended?['duration'],
    ).replaceFirst('PT', '').toLowerCase();
    return ListTile(
      contentPadding: EdgeInsets.zero,
      leading: Icon(
        ended == null
            ? (video ? Icons.videocam_outlined : Icons.call_outlined)
            : Icons.call_end_outlined,
      ),
      title: Text(
        ended != null
            ? 'Call ended'
            : video
            ? 'Video call'
            : 'Audio call',
      ),
      subtitle: ended == null ? null : Text(duration),
      onTap: widget.threadId == null
          ? () => _thread(
              _str(
                message['threadTargetId'],
                _str(message['threadId'], _str(message['id'])),
              ),
            )
          : null,
      trailing: widget.threadId == null
          ? const Icon(Icons.forum_outlined)
          : null,
    );
  }

  Widget _linkPreview(Map<String, dynamic> preview) => Padding(
    padding: const EdgeInsets.only(top: 8),
    child: Card.filled(
      margin: EdgeInsets.zero,
      child: ListTile(
        title: Text(
          _str(preview['title'], _str(preview['url'])),
          maxLines: 2,
          overflow: TextOverflow.ellipsis,
        ),
        subtitle: preview['description'] == null
            ? null
            : Text(
                _str(preview['description']),
                maxLines: 3,
                overflow: TextOverflow.ellipsis,
              ),
        trailing: const Icon(Icons.open_in_new),
        onTap: () => _command('link.open', {'url': preview['url']}),
      ),
    ),
  );

  Widget _richBody(Map<String, dynamic> message) {
    final body = _str(message['body']);
    final spans = _maps(message['markup']);
    if (spans.isEmpty) {
      final loneImage = Uri.tryParse(body.trim());
      if (loneImage != null &&
          loneImage.scheme == 'https' &&
          !body.trim().contains(RegExp(r'\s')) &&
          RegExp(
            r'\.(gif|png|jpe?g|webp)$',
            caseSensitive: false,
          ).hasMatch(loneImage.path)) {
        return InkWell(
          onTap: () => _command('link.open', {'url': body.trim()}),
          child: ClipRRect(
            borderRadius: BorderRadius.circular(12),
            child: Image.network(
              body.trim(),
              height: 220,
              fit: BoxFit.contain,
              semanticLabel: 'Shared image',
              errorBuilder: (_, error, stack) => SelectableText(
                body,
                style: Theme.of(context).textTheme.bodyLarge,
              ),
            ),
          ),
        );
      }
      return Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          SelectableText(body, style: Theme.of(context).textTheme.bodyLarge),
          _bodyLinks(body, spans),
        ],
      );
    }
    final runes = body.runes.toList();
    final boundaries = <int>{0, runes.length};
    for (final span in spans) {
      boundaries.add(
        ((span['start'] ?? span['begin'] ?? 0) as num)
            .toInt()
            .clamp(0, runes.length)
            .toInt(),
      );
      boundaries.add(
        ((span['end'] ?? 0) as num).toInt().clamp(0, runes.length).toInt(),
      );
    }
    final ordered = boundaries.toList()..sort();
    final children = <TextSpan>[];
    for (var index = 0; index + 1 < ordered.length; index++) {
      final start = ordered[index];
      final end = ordered[index + 1];
      var style = const TextStyle();
      for (final span in spans) {
        final begin = ((span['start'] ?? span['begin'] ?? 0) as num).toInt();
        final finish = ((span['end'] ?? 0) as num).toInt();
        if (begin > start || finish < end) {
          continue;
        }
        switch (_str(span['kind'], _str(span['type'])).toLowerCase()) {
          case 'strong':
          case 'bold':
            style = style.merge(const TextStyle(fontWeight: FontWeight.bold));
          case 'emphasis':
          case 'italic':
            style = style.merge(const TextStyle(fontStyle: FontStyle.italic));
          case 'code':
          case 'code_block':
            style = style.merge(
              TextStyle(
                fontFamily: 'monospace',
                backgroundColor: Theme.of(
                  context,
                ).colorScheme.surfaceContainerHighest,
              ),
            );
          case 'strikethrough':
          case 'strike':
            style = style.merge(
              const TextStyle(decoration: TextDecoration.lineThrough),
            );
          case 'link':
            style = style.merge(
              TextStyle(
                color: Theme.of(context).colorScheme.primary,
                decoration: TextDecoration.underline,
              ),
            );
          case 'blockquote':
            style = style.merge(
              TextStyle(
                fontStyle: FontStyle.italic,
                color: Theme.of(context).colorScheme.onSurfaceVariant,
              ),
            );
        }
      }
      children.add(
        TextSpan(
          text: String.fromCharCodes(runes.sublist(start, end)),
          style: style,
        ),
      );
    }
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        SelectableText.rich(
          TextSpan(children: children),
          style: Theme.of(context).textTheme.bodyLarge,
        ),
        _bodyLinks(body, spans),
      ],
    );
  }

  Widget _bodyLinks(String body, List<Map<String, dynamic>> spans) {
    final urls =
        <String>{
          for (final match in RegExp(r'https?://[^\s<>]+').allMatches(body))
            match.group(0)!.replaceFirst(RegExp(r'[.,;!?)]+$'), ''),
          for (final span in spans)
            if (span['uri'] != null) _str(span['uri']),
        }.where((url) {
          final uri = Uri.tryParse(url);
          return uri != null &&
              (uri.scheme == 'https' || uri.scheme == 'http') &&
              uri.host.isNotEmpty;
        }).toList();
    if (urls.isEmpty) {
      return const SizedBox.shrink();
    }
    return Padding(
      padding: const EdgeInsets.only(top: 6),
      child: Wrap(
        spacing: 6,
        runSpacing: 4,
        children: [
          for (final url in urls)
            ActionChip(
              avatar: const Icon(Icons.open_in_new, size: 16),
              label: Text(Uri.parse(url).host),
              tooltip: url,
              onPressed: () => _command('link.open', {'url': url}),
            ),
        ],
      ),
    );
  }

  Future<void> _safetyScores(dynamic value) async {
    final details = value is Map ? value : <String, dynamic>{};
    await showDialog<void>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('Safety scores'),
        content: SingleChildScrollView(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              const Text('Automated scores attached by the room.'),
              if (details['modelVersion'] != null)
                Padding(
                  padding: const EdgeInsets.only(top: 8),
                  child: Text(
                    'Model: ${details['modelVersion']}',
                    style: Theme.of(context).textTheme.labelSmall,
                  ),
                ),
              for (final score in _maps(details['scores']))
                Padding(
                  padding: const EdgeInsets.only(top: 16),
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Row(
                        children: [
                          Expanded(child: Text(_str(score['category']))),
                          Text(
                            '${(((score['probability'] as num?)?.toDouble() ?? 0) * 100).toStringAsFixed(1)}%',
                          ),
                        ],
                      ),
                      const SizedBox(height: 6),
                      LinearProgressIndicator(
                        value: ((score['probability'] as num?)?.toDouble() ?? 0)
                            .clamp(0.0, 1.0)
                            .toDouble(),
                      ),
                      if (score['taxonomyVersion'] != null)
                        Padding(
                          padding: const EdgeInsets.only(top: 4),
                          child: Text(
                            'Taxonomy: ${score['taxonomyVersion']}',
                            style: Theme.of(context).textTheme.labelSmall,
                          ),
                        ),
                    ],
                  ),
                ),
            ],
          ),
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context),
            child: const Text('Close'),
          ),
        ],
      ),
    );
  }

  Widget _composer() {
    final colors = Theme.of(context).colorScheme;
    return Material(
      color: colors.surfaceContainerLow,
      child: Padding(
        padding: const EdgeInsets.fromLTRB(8, 8, 8, 6),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            if (_reply != null || _edit != null)
              Row(
                children: [
                  Padding(
                    padding: const EdgeInsets.symmetric(horizontal: 12),
                    child: Icon(
                      _edit == null ? Icons.reply : Icons.edit_outlined,
                      size: 18,
                    ),
                  ),
                  Expanded(
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Text(
                          _edit == null
                              ? 'Replying to ${_str(_reply?['author'])}'
                              : 'Editing message',
                          style: Theme.of(context).textTheme.labelLarge,
                        ),
                        Text(
                          _str((_edit ?? _reply)?['body']),
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: Theme.of(context).textTheme.bodySmall,
                        ),
                      ],
                    ),
                  ),
                  IconButton(
                    tooltip: 'Cancel ${_edit == null ? 'reply' : 'edit'}',
                    onPressed: () => setState(() {
                      _reply = null;
                      _edit = null;
                    }),
                    icon: const Icon(Icons.close),
                  ),
                ],
              ),
            if (_mentions.isNotEmpty)
              ConstrainedBox(
                constraints: const BoxConstraints(maxHeight: 180),
                child: ListView(
                  shrinkWrap: true,
                  children: [
                    for (final person in _mentions)
                      ListTile(
                        dense: true,
                        leading: const Icon(Icons.alternate_email),
                        title: Text(_str(person['name'], _str(person['jid']))),
                        subtitle: Text(_str(person['jid'])),
                        onTap: () => _insertMention(person),
                      ),
                  ],
                ),
              ),
            Row(
              crossAxisAlignment: CrossAxisAlignment.end,
              children: [
                IconButton(
                  tooltip: 'Attach encrypted file',
                  icon: const Icon(Icons.add_circle_outline),
                  onPressed: () => _command('conversation.attach'),
                ),
                Expanded(
                  child: TextField(
                    controller: _draft,
                    focusNode: _focus,
                    minLines: 1,
                    maxLines: 6,
                    textCapitalization: TextCapitalization.sentences,
                    keyboardType: TextInputType.multiline,
                    contentInsertionConfiguration:
                        ContentInsertionConfiguration(
                          allowedMimeTypes: const ['image/*'],
                          onContentInserted: (content) =>
                              _command('conversation.insertContent', {
                                'uri': content.uri,
                                'mimeType': content.mimeType,
                                if (content.data != null) 'data': content.data,
                              }),
                        ),
                    decoration: InputDecoration(
                      hintText: widget.threadId == null
                          ? 'Message ${widget.title}'
                          : 'Reply in thread',
                      filled: true,
                      fillColor: colors.surfaceContainerHighest,
                      border: OutlineInputBorder(
                        borderRadius: BorderRadius.circular(24),
                        borderSide: BorderSide.none,
                      ),
                      contentPadding: const EdgeInsets.symmetric(
                        horizontal: 16,
                        vertical: 12,
                      ),
                    ),
                  ),
                ),
                const SizedBox(width: 4),
                IconButton.filled(
                  tooltip: _edit == null ? 'Send message' : 'Save edit',
                  onPressed: _sending ? null : _send,
                  icon: _sending
                      ? SizedBox(
                          width: 20,
                          height: 20,
                          child: CircularProgressIndicator(
                            strokeWidth: 2,
                            color: colors.onPrimary,
                          ),
                        )
                      : Icon(_edit == null ? Icons.send : Icons.check),
                ),
              ],
            ),
            Wrap(
              children: [
                IconButton(
                  tooltip: 'Bold',
                  icon: const Icon(Icons.format_bold, size: 20),
                  onPressed: () => _format('**'),
                ),
                IconButton(
                  tooltip: 'Italic',
                  icon: const Icon(Icons.format_italic, size: 20),
                  onPressed: () => _format('*'),
                ),
                IconButton(
                  tooltip: 'Code',
                  icon: const Icon(Icons.code, size: 20),
                  onPressed: () => _format('`'),
                ),
                IconButton(
                  tooltip: 'Strikethrough',
                  icon: const Icon(Icons.strikethrough_s, size: 20),
                  onPressed: () => _format('~~'),
                ),
                IconButton(
                  tooltip: 'Paste image',
                  icon: const Icon(Icons.content_paste),
                  onPressed: () async {
                    final pasted = await _command('conversation.pasteImage');
                    if (mounted && pasted == false) {
                      ScaffoldMessenger.of(context).showSnackBar(
                        const SnackBar(
                          content: Text('Clipboard has no image.'),
                        ),
                      );
                    }
                  },
                ),
                IconButton(
                  tooltip: 'GIFs',
                  icon: const Icon(Icons.gif_box_outlined),
                  onPressed: _gifPicker,
                ),
                IconButton(
                  tooltip: 'Stickers',
                  icon: const Icon(Icons.emoji_emotions_outlined),
                  onPressed: _stickerPicker,
                ),
                IconButton(
                  tooltip: 'Slash commands',
                  icon: const Icon(Icons.terminal),
                  onPressed: _slashCommands,
                ),
              ],
            ),
          ],
        ),
      ),
    );
  }

  void _format(String marker) {
    final selection = _draft.selection;
    final text = _draft.text;
    final start = selection.start.clamp(0, text.length).toInt();
    final end = selection.end.clamp(start, text.length).toInt();
    final selected = text.substring(start, end);
    final preservedMentions = <Map<String, dynamic>>[];
    for (final ref in _mentionRefs) {
      final refStart = ref['start'] as int;
      final refEnd = ref['end'] as int;
      if (refEnd <= start) {
        preservedMentions.add(Map<String, dynamic>.from(ref));
      } else if (end <= refStart) {
        preservedMentions.add({
          ...ref,
          'start': refStart + marker.length * 2,
          'end': refEnd + marker.length * 2,
        });
      } else if (start <= refStart && refEnd <= end) {
        preservedMentions.add({
          ...ref,
          'start': refStart + marker.length,
          'end': refEnd + marker.length,
        });
      }
    }
    _draft.value = TextEditingValue(
      text: text.replaceRange(start, end, '$marker$selected$marker'),
      selection: TextSelection(
        baseOffset: start + marker.length,
        extentOffset: end + marker.length,
      ),
    );
    _mentionRefs
      ..clear()
      ..addAll(preservedMentions);
    _saveMentions();
    _focus.requestFocus();
  }

  Future<void> _actions(Map<String, dynamic> message) async {
    final choice = await showModalBottomSheet<String>(
      context: context,
      showDragHandle: true,
      builder: (context) => SafeArea(
        child: SingleChildScrollView(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              Padding(
                padding: const EdgeInsets.symmetric(horizontal: 16),
                child: Wrap(
                  spacing: 8,
                  children: [
                    for (final emoji in ['👍', '❤️', '😂', '🎉', '👀', '😢'])
                      IconButton(
                        tooltip: 'React $emoji',
                        onPressed: () => Navigator.pop(context, 'react:$emoji'),
                        icon: Text(emoji, style: const TextStyle(fontSize: 24)),
                      ),
                  ],
                ),
              ),
              _actionTile(context, 'Reply', Icons.reply, 'reply'),
              _actionTile(
                context,
                'Reply in thread',
                Icons.forum_outlined,
                'thread',
              ),
              _actionTile(context, 'Copy text', Icons.copy_outlined, 'copy'),
              if (message['mine'] == true)
                _actionTile(context, 'Edit', Icons.edit_outlined, 'edit'),
              if (widget.group && _data['canPin'] == true)
                _actionTile(
                  context,
                  (_data['pinnedIds'] as List? ?? []).contains(message['id'])
                      ? 'Unpin'
                      : 'Pin',
                  Icons.push_pin_outlined,
                  'pin',
                ),
              if (message['mine'] == true)
                _actionTile(
                  context,
                  'Remove message',
                  Icons.delete_outline,
                  'retract',
                ),
              if (message['mine'] != true && _data['canModerate'] == true)
                _actionTile(
                  context,
                  'Moderate message',
                  Icons.shield_outlined,
                  'moderate',
                ),
              const SizedBox(height: 12),
            ],
          ),
        ),
      ),
    );
    if (!mounted || choice == null) {
      return;
    }
    if (choice.startsWith('react:')) {
      await _command('conversation.reaction', {
        'targetId': message['id'],
        'emoji': choice.substring(6),
      });
      return;
    }
    switch (choice) {
      case 'reply':
        setState(() {
          _reply = message;
          _edit = null;
        });
        _focus.requestFocus();
      case 'thread':
        await _thread(
          _str(
            message['threadTargetId'],
            _str(message['threadId'], _str(message['id'])),
          ),
        );
      case 'copy':
        await Clipboard.setData(ClipboardData(text: _str(message['body'])));
      case 'edit':
        _restoredEditAttempt = 0;
        setState(() {
          _edit = message;
          _reply = null;
          _draft.text = _str(message['body']);
        });
        _draft.selection = TextSelection.collapsed(offset: _draft.text.length);
        _focus.requestFocus();
      case 'pin':
        await _command('conversation.pin', {
          'targetId': message['id'],
          'pinned': !(_data['pinnedIds'] as List? ?? []).contains(
            message['id'],
          ),
        });
      case 'retract':
      case 'moderate':
        if (await _confirm(
          'Remove this message?',
          'The message will be removed for everyone.',
        )) {
          await _command('conversation.$choice', {'targetId': message['id']});
        }
    }
  }

  Widget _actionTile(
    BuildContext context,
    String label,
    IconData icon,
    String value,
  ) => ListTile(
    leading: Icon(icon),
    title: Text(label),
    onTap: () => Navigator.pop(context, value),
  );

  Future<bool> _confirm(
    String title,
    String body, {
    String action = 'Remove',
  }) async =>
      await showDialog<bool>(
        context: context,
        builder: (context) => AlertDialog(
          title: Text(title),
          content: Text(body),
          actions: [
            TextButton(
              onPressed: () => Navigator.pop(context, false),
              child: const Text('Cancel'),
            ),
            FilledButton(
              onPressed: () => Navigator.pop(context, true),
              child: Text(action),
            ),
          ],
        ),
      ) ??
      false;

  Future<void> _thread(String threadId) async {
    Navigator.of(context).restorablePushNamed(
      'conversation',
      arguments: {
        'jid': widget.jid,
        'title': widget.title,
        'group': widget.group,
        'threadId': threadId,
      },
    );
  }

  Future<void> _startCall(bool video) async {
    if (_startingCall) {
      return;
    }
    setState(() => _startingCall = true);
    await _command('call.start', {'video': video});
    if (!mounted) {
      return;
    }
    setState(() => _startingCall = false);
    if (_error == null && _active) {
      Navigator.of(context).restorablePushNamed('call');
    }
  }

  Future<void> _menu(String value) async {
    switch (value) {
      case 'video':
        await _startCall(true);
      case 'members':
        await _members();
      case 'threads':
        await _threads();
      case 'pins':
        await _pins();
      case 'notifications':
        await _notifications();
      case 'settings':
        await _roomSettings();
      case 'rename':
        await _renameGroup();
      case 'invite':
        await _inviteGroup();
      case 'leave':
        if (await _confirm(
          'Leave this group?',
          'You will need an invitation to join again.',
          action: 'Leave',
        )) {
          await _command('group.leave');
          if (mounted && _error == null) {
            Navigator.of(context).pop();
          }
        }
      case 'leaveRoomCall':
        await _command('call.leaveRetained');
        await _refreshRoomCallState();
    }
  }

  Future<void> _notifications() async {
    final current = _str(_data['notifyMode'], 'always').toLowerCase();
    final value = await showModalBottomSheet<String>(
      context: context,
      showDragHandle: true,
      builder: (context) => SafeArea(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Padding(
              padding: const EdgeInsets.all(16),
              child: Text(
                'Notifications',
                style: Theme.of(context).textTheme.titleLarge,
              ),
            ),
            for (final entry in {
              'always': 'All messages',
              'on_mention': 'Mentions only',
              'never': 'Muted',
            }.entries)
              ListTile(
                leading: Icon(
                  current == entry.key
                      ? Icons.radio_button_checked
                      : Icons.radio_button_unchecked,
                ),
                title: Text(entry.value),
                onTap: () => Navigator.pop(context, entry.key),
              ),
            const SizedBox(height: 12),
          ],
        ),
      ),
    );
    if (value != null) {
      await _command('conversation.notify', {'mode': value});
    }
  }

  Future<void> _threads() async {
    final threads = _maps(_data['threads']);
    final id = await showModalBottomSheet<String>(
      context: context,
      showDragHandle: true,
      builder: (context) => SafeArea(
        child: SizedBox(
          height: MediaQuery.sizeOf(context).height * .65,
          child: Column(
            children: [
              Padding(
                padding: const EdgeInsets.all(16),
                child: Text(
                  'Threads',
                  style: Theme.of(context).textTheme.titleLarge,
                ),
              ),
              Expanded(
                child: threads.isEmpty
                    ? const Center(child: Text('No threads in loaded history'))
                    : ListView(
                        children: [
                          for (final thread in threads)
                            ListTile(
                              leading: const Icon(Icons.forum_outlined),
                              title: Text(
                                _str(thread['body']),
                                maxLines: 2,
                                overflow: TextOverflow.ellipsis,
                              ),
                              subtitle: Text(
                                '${thread['replyCount'] ?? 0} replies · ${_str(thread['author'])}',
                              ),
                              onTap: () => Navigator.pop(
                                context,
                                _str(thread['threadId']),
                              ),
                            ),
                        ],
                      ),
              ),
            ],
          ),
        ),
      ),
    );
    if (id != null && mounted) {
      await _thread(id);
    }
  }

  Future<void> _pins() async {
    final ids = _data['pinnedIds'] as List? ?? [];
    final messages = _maps(
      _data['messages'],
    ).where((message) => ids.contains(message['id'])).toList();
    if (!mounted) {
      return;
    }
    await showModalBottomSheet<void>(
      context: context,
      showDragHandle: true,
      builder: (context) => SafeArea(
        child: SizedBox(
          height: MediaQuery.sizeOf(context).height * .65,
          child: Column(
            children: [
              Padding(
                padding: const EdgeInsets.all(16),
                child: Text(
                  'Pinned messages',
                  style: Theme.of(context).textTheme.titleLarge,
                ),
              ),
              Expanded(
                child: messages.isEmpty
                    ? const Center(
                        child: Text('No pinned messages in loaded history'),
                      )
                    : ListView(
                        children: [
                          for (final message in messages)
                            ListTile(
                              leading: const Icon(Icons.push_pin_outlined),
                              title: Text(_str(message['body'])),
                              subtitle: Text(_str(message['author'])),
                            ),
                        ],
                      ),
              ),
            ],
          ),
        ),
      ),
    );
  }

  Future<void> _search() async {
    final query = TextEditingController();
    await showModalBottomSheet<void>(
      context: context,
      isScrollControlled: true,
      showDragHandle: true,
      builder: (context) => _SearchSheet(
        title: 'Search messages',
        controller: query,
        search: (value) async =>
            _maps(await _command('conversation.search', {'query': value})),
        tile: (context, result) => ListTile(
          title: Text(_str(result['body'])),
          subtitle: Text(_str(result['author'], _str(result['from']))),
        ),
      ),
    );
    query.dispose();
  }

  Future<void> _members() async {
    final result = await _command('conversation.members');
    if (!mounted) {
      return;
    }
    final members = result is Map ? _maps(result['members']) : _maps(result);
    await showModalBottomSheet<void>(
      context: context,
      showDragHandle: true,
      builder: (context) => SafeArea(
        child: SizedBox(
          height: MediaQuery.sizeOf(context).height * .7,
          child: Column(
            children: [
              Padding(
                padding: const EdgeInsets.all(16),
                child: Text(
                  'Members',
                  style: Theme.of(context).textTheme.titleLarge,
                ),
              ),
              Expanded(
                child: members.isEmpty
                    ? const Center(child: Text('No members available'))
                    : ListView(
                        children: [
                          for (final member in members)
                            ListTile(
                              leading: const CircleAvatar(
                                child: Icon(Icons.person_outline),
                              ),
                              title: Text(
                                _str(member['name'], _str(member['jid'])),
                              ),
                              subtitle: Text(
                                _str(
                                  member['affiliation'],
                                  _str(member['role']),
                                ),
                              ),
                              onTap: () => _profile({
                                'authorJid': member['jid'],
                                'author': member['name'],
                              }),
                              trailing: _data['canManageMembers'] == true
                                  ? PopupMenuButton<String>(
                                      onSelected: (action) async {
                                        if (action == 'kick') {
                                          await _command('room.kick', {
                                            'nick':
                                                member['nick'] ??
                                                member['name'],
                                          });
                                        } else {
                                          await _command('room.affiliation', {
                                            'targetJid': member['jid'],
                                            'affiliation': action,
                                          });
                                        }
                                      },
                                      itemBuilder: (context) => const [
                                        PopupMenuItem(
                                          value: 'member',
                                          child: Text('Make member'),
                                        ),
                                        PopupMenuItem(
                                          value: 'admin',
                                          child: Text('Make admin'),
                                        ),
                                        PopupMenuItem(
                                          value: 'kick',
                                          child: Text('Remove from room'),
                                        ),
                                        PopupMenuItem(
                                          value: 'outcast',
                                          child: Text('Ban'),
                                        ),
                                      ],
                                    )
                                  : null,
                            ),
                        ],
                      ),
              ),
            ],
          ),
        ),
      ),
    );
  }

  Future<void> _roomSettings() async {
    final result = await _command('room.config');
    if (!mounted || result is! Map) {
      return;
    }
    final name = TextEditingController(
      text: _str(result['name'], widget.title),
    );
    final description = TextEditingController(
      text: _str(result['description']),
    );
    var forum = result['forum'] == true;
    var pinPermission = _str(result['pinPermission'], 'admins_only');
    if (!['admins_only', 'anyone'].contains(pinPermission)) {
      pinPermission = 'admins_only';
    }
    final save = await showDialog<String>(
      context: context,
      builder: (context) => StatefulBuilder(
        builder: (context, update) => AlertDialog(
          title: const Text('Room settings'),
          content: SingleChildScrollView(
            child: Column(
              mainAxisSize: MainAxisSize.min,
              children: [
                TextField(
                  controller: name,
                  decoration: const InputDecoration(labelText: 'Name'),
                ),
                const SizedBox(height: 16),
                TextField(
                  controller: description,
                  maxLines: 3,
                  decoration: const InputDecoration(labelText: 'Description'),
                ),
                const SizedBox(height: 16),
                SwitchListTile(
                  contentPadding: EdgeInsets.zero,
                  title: const Text('Forum room'),
                  subtitle: const Text(
                    'Organize the conversation around threads',
                  ),
                  value: forum,
                  onChanged: (value) => update(() => forum = value),
                ),
                DropdownButtonFormField<String>(
                  initialValue: pinPermission,
                  decoration: const InputDecoration(
                    labelText: 'Who can pin messages',
                  ),
                  items: [
                    for (final permission in ['admins_only', 'anyone'])
                      DropdownMenuItem(
                        value: permission,
                        child: Text(
                          permission == 'admins_only'
                              ? 'Owners and admins'
                              : 'Everyone',
                        ),
                      ),
                  ],
                  onChanged: (value) =>
                      update(() => pinPermission = value ?? pinPermission),
                ),
                const SizedBox(height: 16),
                TextButton.icon(
                  onPressed: () => Navigator.pop(context, 'destroy'),
                  icon: const Icon(Icons.delete_forever_outlined),
                  label: const Text('Delete room'),
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
              onPressed: () => Navigator.pop(context, 'save'),
              child: const Text('Save'),
            ),
          ],
        ),
      ),
    );
    if (save == 'save') {
      await _command('room.save', {
        'name': name.text.trim(),
        'description': description.text.trim(),
        'forum': forum,
        'pinPermission': pinPermission,
      });
    }
    if (save == 'destroy' &&
        mounted &&
        await _confirm(
          'Delete this room?',
          'Everyone will lose access to the room.',
          action: 'Delete',
        )) {
      await _command('room.destroy');
      if (mounted && _error == null) {
        Navigator.of(context).pop();
      }
    }
    name.dispose();
    description.dispose();
  }

  Future<void> _renameGroup() async {
    final name = TextEditingController(text: widget.title);
    final rename = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('Rename group'),
        content: TextField(
          controller: name,
          maxLength: 100,
          decoration: const InputDecoration(labelText: 'Group name'),
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: const Text('Cancel'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(context, true),
            child: const Text('Save'),
          ),
        ],
      ),
    );
    if (rename == true) {
      await _command('group.rename', {'name': name.text.trim()});
    }
    name.dispose();
  }

  Future<void> _inviteGroup() async {
    final query = TextEditingController();
    await showModalBottomSheet<void>(
      context: context,
      isScrollControlled: true,
      showDragHandle: true,
      builder: (sheetContext) => _SearchSheet(
        title: 'Find someone to invite',
        controller: query,
        search: (value) async =>
            _maps(await _command('directory.search', {'query': value})),
        tile: (context, person) => ListTile(
          leading: const Icon(Icons.person_add_outlined),
          title: Text(_str(person['name'], _str(person['jid']))),
          subtitle: Text(_str(person['jid'])),
          onTap: () async {
            var fullHistory = false;
            final invite = await showDialog<bool>(
              context: context,
              builder: (context) => StatefulBuilder(
                builder: (context, update) => AlertDialog(
                  title: Text(
                    'Invite ${_str(person['name'], _str(person['jid']))}?',
                  ),
                  content: SwitchListTile(
                    contentPadding: EdgeInsets.zero,
                    title: const Text('Share full history'),
                    subtitle: const Text(
                      'Otherwise they will see messages from the moment they join.',
                    ),
                    value: fullHistory,
                    onChanged: (value) => update(() => fullHistory = value),
                  ),
                  actions: [
                    TextButton(
                      onPressed: () => Navigator.pop(context, false),
                      child: const Text('Cancel'),
                    ),
                    FilledButton(
                      onPressed: () => Navigator.pop(context, true),
                      child: const Text('Invite'),
                    ),
                  ],
                ),
              ),
            );
            if (invite == true) {
              if (sheetContext.mounted) {
                Navigator.pop(sheetContext);
              }
              await _command('group.invite', {
                'inviteeJid': person['jid'],
                'fullHistory': fullHistory,
              });
            }
          },
        ),
      ),
    );
    query.dispose();
  }

  Future<void> _profile(Map<String, dynamic> message) async {
    final jid = _str(message['authorJid']);
    if (jid.isEmpty) {
      return;
    }
    final result = await _command('profile.load', {'jid': jid});
    if (!mounted) {
      return;
    }
    final profile = result is Map ? result : <String, dynamic>{};
    final messagePerson =
        await showModalBottomSheet<bool>(
          context: context,
          showDragHandle: true,
          builder: (context) => SafeArea(
            child: SingleChildScrollView(
              child: Padding(
                padding: const EdgeInsets.all(24),
                child: Column(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    CircleAvatar(
                      radius: 36,
                      backgroundImage: profile['avatar'] is Uint8List
                          ? MemoryImage(profile['avatar'] as Uint8List)
                          : null,
                      child: profile['avatar'] is Uint8List
                          ? null
                          : const Icon(Icons.person_outline, size: 36),
                    ),
                    const SizedBox(height: 16),
                    Text(
                      _str(
                        profile['fullName'],
                        _str(profile['nickname'], _str(message['author'], jid)),
                      ),
                      style: Theme.of(context).textTheme.headlineSmall,
                    ),
                    Text(jid),
                    if (profile['note'] != null)
                      Padding(
                        padding: const EdgeInsets.only(top: 12),
                        child: Text(_str(profile['note'])),
                      ),
                    if (profile['mood'] != null)
                      Padding(
                        padding: const EdgeInsets.only(top: 8),
                        child: Text(_str(profile['mood'])),
                      ),
                    const SizedBox(height: 24),
                    FilledButton.icon(
                      onPressed: () => Navigator.pop(context, true),
                      icon: const Icon(Icons.chat_bubble_outline),
                      label: const Text('Message'),
                    ),
                  ],
                ),
              ),
            ),
          ),
        ) ??
        false;
    if (!messagePerson || !mounted || jid == widget.jid) {
      return;
    }
    Navigator.of(context).restorablePushNamed(
      'conversation',
      arguments: {
        'jid': jid,
        'title': _str(message['author'], jid),
        'group': false,
      },
    );
  }

  Future<void> _gifPicker() async {
    final query = TextEditingController();
    await showModalBottomSheet<void>(
      context: context,
      isScrollControlled: true,
      showDragHandle: true,
      builder: (context) => _SearchSheet(
        title: 'Find a GIF',
        controller: query,
        search: (value) async =>
            _maps(await _command('gif.search', {'query': value})),
        tile: (context, gif) => ListTile(
          leading: gif['previewUrl'] != null
              ? Image.network(
                  _str(gif['previewUrl']),
                  width: 64,
                  fit: BoxFit.cover,
                  errorBuilder: (_, error, stack) =>
                      const Icon(Icons.gif_box_outlined),
                )
              : const Icon(Icons.gif_box_outlined),
          title: Text(_str(gif['title'], 'GIF')),
          onTap: () async {
            Navigator.pop(context);
            await _command('conversation.gif', {'url': gif['originalUrl']});
          },
        ),
      ),
    );
    query.dispose();
  }

  Future<void> _stickerPicker() async {
    final result = await _command('stickers.load');
    if (!mounted) {
      return;
    }
    final packs = _maps(result);
    await showModalBottomSheet<void>(
      context: context,
      showDragHandle: true,
      builder: (context) => SafeArea(
        child: SizedBox(
          height: MediaQuery.sizeOf(context).height * .65,
          child: ListView(
            children: [
              Padding(
                padding: const EdgeInsets.all(16),
                child: Row(
                  children: [
                    Expanded(
                      child: Text(
                        'Stickers',
                        style: Theme.of(context).textTheme.titleLarge,
                      ),
                    ),
                    FilledButton.tonalIcon(
                      onPressed: () {
                        Navigator.pop(context);
                        unawaited(_createStickerPack());
                      },
                      icon: const Icon(Icons.add),
                      label: const Text('Create'),
                    ),
                  ],
                ),
              ),
              if (packs.isEmpty)
                const Padding(
                  padding: EdgeInsets.all(32),
                  child: Text('No sticker packs yet.'),
                ),
              for (final pack in packs) ...[
                ListTile(
                  title: Text(_str(pack['name'])),
                  trailing: IconButton(
                    tooltip: 'Remove sticker pack',
                    icon: const Icon(Icons.delete_outline),
                    onPressed: () async {
                      Navigator.pop(context);
                      if (await _confirm(
                        'Remove this sticker pack?',
                        'The pack will be removed from your collection.',
                      )) {
                        await _command('stickers.remove', {
                          'packId': pack['id'],
                        });
                      }
                    },
                  ),
                ),
                Padding(
                  padding: const EdgeInsets.symmetric(horizontal: 16),
                  child: Wrap(
                    spacing: 8,
                    runSpacing: 8,
                    children: [
                      for (final item in _maps(pack['stickers']))
                        Tooltip(
                          message: _str(item['desc']),
                          child: InkWell(
                            borderRadius: BorderRadius.circular(12),
                            onTap: () async {
                              Navigator.pop(context);
                              await _command('conversation.sticker', {
                                'packId': pack['id'],
                                'index': item['index'],
                              });
                            },
                            child: SizedBox(
                              width: 80,
                              height: 80,
                              child: item['url'] == null
                                  ? const Icon(Icons.emoji_emotions_outlined)
                                  : Image.network(
                                      _str(item['url']),
                                      semanticLabel: _str(item['desc']),
                                      errorBuilder: (_, error, stack) =>
                                          const Icon(
                                            Icons.broken_image_outlined,
                                          ),
                                    ),
                            ),
                          ),
                        ),
                    ],
                  ),
                ),
              ],
              const SizedBox(height: 24),
            ],
          ),
        ),
      ),
    );
  }

  Future<void> _createStickerPack() async {
    final name = TextEditingController();
    final summary = TextEditingController();
    final formKey = GlobalKey<FormState>();
    final create = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        scrollable: true,
        title: const Text('Create sticker pack'),
        content: Form(
          key: formKey,
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              TextFormField(
                controller: name,
                decoration: const InputDecoration(labelText: 'Pack name'),
                validator: (value) => value == null || value.trim().isEmpty
                    ? 'Enter a name'
                    : null,
              ),
              const SizedBox(height: 16),
              TextField(
                controller: summary,
                decoration: const InputDecoration(labelText: 'Description'),
              ),
              const SizedBox(height: 16),
              const Text('Choose the images for your pack on the next screen.'),
            ],
          ),
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: const Text('Cancel'),
          ),
          FilledButton(
            onPressed: () {
              if (formKey.currentState?.validate() == true) {
                Navigator.pop(context, true);
              }
            },
            child: const Text('Choose images'),
          ),
        ],
      ),
    );
    if (create == true) {
      await _command('stickers.create', {
        'name': name.text.trim(),
        'summary': summary.text.trim(),
      });
    }
    name.dispose();
    summary.dispose();
  }

  Future<void> _slashCommands() async {
    final result = await _command('slash.list');
    if (!mounted) {
      return;
    }
    final commands = _maps(result);
    final command = await showModalBottomSheet<Map<String, dynamic>>(
      context: context,
      showDragHandle: true,
      builder: (context) => SafeArea(
        child: SizedBox(
          height: MediaQuery.sizeOf(context).height * .6,
          child: Column(
            children: [
              Padding(
                padding: const EdgeInsets.all(16),
                child: Text(
                  'Commands',
                  style: Theme.of(context).textTheme.titleLarge,
                ),
              ),
              Expanded(
                child: commands.isEmpty
                    ? const Center(child: Text('No commands available'))
                    : ListView(
                        children: [
                          for (final command in commands)
                            ListTile(
                              leading: const Icon(Icons.terminal),
                              title: Text(
                                _str(command['name'], _str(command['node'])),
                              ),
                              subtitle: command['description'] == null
                                  ? null
                                  : Text(_str(command['description'])),
                              onTap: () => Navigator.pop(context, command),
                            ),
                        ],
                      ),
              ),
            ],
          ),
        ),
      ),
    );
    if (command == null) {
      return;
    }
    final response = await _command('slash.invoke', {
      'serviceJid': command['serviceJid'],
      'node': command['node'],
    });
    if (!mounted || response == null) {
      return;
    }
    await _commandForm(response, command);
  }

  Future<void> _commandForm(
    dynamic response,
    Map<String, dynamic> command,
  ) async {
    if (response is! Map) {
      return;
    }
    final form = response['form'] is Map
        ? Map<String, dynamic>.from(response['form'] as Map)
        : <String, dynamic>{};
    final fields = _maps(form['fields']);
    final notes = _maps(
      response['notes'],
    ).map((note) => _str(note['value'])).join('\n');
    if (fields.isEmpty && response['status'] != 'executing') {
      await _showDetails(
        _str(form['title'], 'Command result'),
        notes.isEmpty ? _str(response['status'], 'Completed') : notes,
      );
      return;
    }
    final controllers = <String, TextEditingController>{
      for (final field in fields)
        _str(field['name']): TextEditingController(
          text: (field['values'] as List? ?? []).join('\n'),
        ),
    };
    final formKey = GlobalKey<FormState>();
    final actions = (response['actions'] as List? ?? const ['complete'])
        .map((action) => action.toString())
        .toList();
    final action = await showDialog<String>(
      context: context,
      builder: (context) => StatefulBuilder(
        builder: (context, update) => AlertDialog(
          title: Text(_str(form['title'], 'Command')),
          content: Form(
            key: formKey,
            child: SingleChildScrollView(
              child: Column(
                mainAxisSize: MainAxisSize.min,
                children: [
                  if (_str(form['instructions']).isNotEmpty)
                    Padding(
                      padding: const EdgeInsets.only(bottom: 16),
                      child: Text(_str(form['instructions'])),
                    ),
                  if (notes.isNotEmpty)
                    Padding(
                      padding: const EdgeInsets.only(bottom: 16),
                      child: Text(notes),
                    ),
                  for (final field in fields)
                    if (field['type'] != 'hidden')
                      Padding(
                        padding: const EdgeInsets.only(bottom: 16),
                        child: Builder(
                          builder: (context) {
                            final type = _str(field['type']);
                            final name = _str(field['name']);
                            final controller = controllers[name]!;
                            final label = _str(field['label'], name);
                            final options = _maps(field['options']);
                            if (type == 'fixed') {
                              return Text(controller.text);
                            }
                            if (type == 'boolean') {
                              return SwitchListTile(
                                contentPadding: EdgeInsets.zero,
                                title: Text(label),
                                value:
                                    controller.text == '1' ||
                                    controller.text == 'true',
                                onChanged: (value) => update(
                                  () => controller.text = value ? '1' : '0',
                                ),
                              );
                            }
                            if (type == 'list_single' && options.isNotEmpty) {
                              return DropdownButtonFormField<String>(
                                initialValue:
                                    options.any(
                                      (option) =>
                                          option['value'] == controller.text,
                                    )
                                    ? controller.text
                                    : null,
                                decoration: InputDecoration(labelText: label),
                                isExpanded: true,
                                items: [
                                  for (final option in options)
                                    DropdownMenuItem(
                                      value: _str(option['value']),
                                      child: Text(
                                        _str(
                                          option['label'],
                                          _str(option['value']),
                                        ),
                                      ),
                                    ),
                                ],
                                onChanged: (value) =>
                                    controller.text = value ?? '',
                                validator: (value) =>
                                    field['required'] == true &&
                                        (value == null || value.isEmpty)
                                    ? 'Choose a value'
                                    : null,
                              );
                            }
                            if (type == 'list_multi' && options.isNotEmpty) {
                              return Column(
                                crossAxisAlignment: CrossAxisAlignment.start,
                                children: [
                                  Text(
                                    label,
                                    style: Theme.of(
                                      context,
                                    ).textTheme.labelLarge,
                                  ),
                                  for (final option in options)
                                    CheckboxListTile(
                                      contentPadding: EdgeInsets.zero,
                                      title: Text(
                                        _str(
                                          option['label'],
                                          _str(option['value']),
                                        ),
                                      ),
                                      value: controller.text
                                          .split('\n')
                                          .contains(option['value']),
                                      onChanged: (checked) => update(() {
                                        final values = controller.text
                                            .split('\n')
                                            .where((value) => value.isNotEmpty)
                                            .toSet();
                                        if (checked == true) {
                                          values.add(_str(option['value']));
                                        } else {
                                          values.remove(option['value']);
                                        }
                                        controller.text = values.join('\n');
                                      }),
                                    ),
                                ],
                              );
                            }
                            return TextFormField(
                              controller: controller,
                              maxLines:
                                  type == 'text_multi' || type == 'jid_multi'
                                  ? 4
                                  : 1,
                              obscureText: type == 'text_private',
                              decoration: InputDecoration(
                                labelText: label,
                                helperText: field['required'] == true
                                    ? 'Required'
                                    : null,
                              ),
                              validator: (value) =>
                                  field['required'] == true &&
                                      (value == null || value.trim().isEmpty)
                                  ? 'Enter a value'
                                  : null,
                            );
                          },
                        ),
                      ),
                ],
              ),
            ),
          ),
          actions: [
            TextButton(
              onPressed: () => Navigator.pop(context, 'cancel'),
              child: const Text('Cancel'),
            ),
            if (actions.contains('prev'))
              TextButton(
                onPressed: () => Navigator.pop(context, 'prev'),
                child: const Text('Previous'),
              ),
            for (final action in actions.where(
              (action) => action == 'next' || action == 'complete',
            ))
              FilledButton(
                onPressed: () {
                  if (formKey.currentState?.validate() == true) {
                    Navigator.pop(context, action);
                  }
                },
                child: Text(action == 'next' ? 'Next' : 'Submit'),
              ),
          ],
        ),
      ),
    );
    final next = await _command('slash.submit', {
      'serviceJid': command['serviceJid'],
      'node': command['node'],
      'sessionId': response['sessionId'],
      'action': action ?? 'cancel',
      'fields': [
        for (final field in fields)
          {
            'name': field['name'],
            'values': controllers[_str(field['name'])]!.text.split('\n'),
          },
      ],
    });
    for (final controller in controllers.values) {
      controller.dispose();
    }
    if (mounted && action != null && action != 'cancel' && next != null) {
      await _commandForm(next, command);
    }
  }

  Future<void> _showDetails(String title, dynamic details) => showDialog<void>(
    context: context,
    builder: (context) => AlertDialog(
      title: Text(title),
      content: SingleChildScrollView(
        child: SelectableText(
          details is Map
              ? details.entries
                    .map((entry) => '${entry.key}: ${entry.value}')
                    .join('\n')
              : details.toString(),
        ),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.pop(context),
          child: const Text('Close'),
        ),
      ],
    ),
  );
}

/// Shared search presentation stays on the existing XMPP history/directory
/// paths; it deliberately does not create an HTTP search client.
class _SearchSheet extends StatefulWidget {
  const _SearchSheet({
    required this.title,
    required this.controller,
    required this.search,
    required this.tile,
  });
  final String title;
  final TextEditingController controller;
  final Future<List<Map<String, dynamic>>> Function(String query) search;
  final Widget Function(BuildContext context, Map<String, dynamic> result) tile;
  @override
  State<_SearchSheet> createState() => _SearchSheetState();
}

class _SearchSheetState extends State<_SearchSheet> {
  List<Map<String, dynamic>> _results = [];
  bool _loading = false;
  bool _searched = false;
  int _generation = 0;
  String? _failure;

  Future<void> _search(String value) async {
    if (value.trim().isEmpty) {
      return;
    }
    final generation = ++_generation;
    setState(() {
      _loading = true;
      _failure = null;
    });
    try {
      final results = await widget.search(value.trim());
      if (mounted && generation == _generation) {
        setState(() {
          _results = results;
          _searched = true;
        });
      }
    } catch (error) {
      if (mounted && generation == _generation) {
        setState(() => _failure = error.toString());
      }
    } finally {
      if (mounted && generation == _generation) {
        setState(() => _loading = false);
      }
    }
  }

  @override
  Widget build(BuildContext context) => Padding(
    padding: EdgeInsets.only(bottom: MediaQuery.viewInsetsOf(context).bottom),
    child: SafeArea(
      child: SizedBox(
        height:
            (MediaQuery.sizeOf(context).height -
                    MediaQuery.viewInsetsOf(context).bottom)
                .clamp(0.0, double.infinity)
                .toDouble() *
            .7,
        child: Column(
          children: [
            Padding(
              padding: const EdgeInsets.fromLTRB(20, 8, 20, 16),
              child: SearchBar(
                controller: widget.controller,
                hintText: widget.title,
                autoFocus: true,
                leading: const Icon(Icons.search),
                trailing: [
                  IconButton(
                    tooltip: 'Search',
                    onPressed: () => _search(widget.controller.text),
                    icon: const Icon(Icons.arrow_forward),
                  ),
                ],
                onSubmitted: _search,
              ),
            ),
            if (_loading) const LinearProgressIndicator(),
            if (_failure != null)
              Padding(
                padding: const EdgeInsets.all(16),
                child: Text(_failure!),
              ),
            Expanded(
              child: _results.isEmpty
                  ? Center(
                      child: Text(
                        _loading
                            ? 'Searching…'
                            : _searched
                            ? 'No results found'
                            : 'Enter a search term',
                      ),
                    )
                  : ListView.builder(
                      itemCount: _results.length,
                      itemBuilder: (context, index) =>
                          widget.tile(context, _results[index]),
                    ),
            ),
          ],
        ),
      ),
    ),
  );
}

String _str(dynamic value, [String fallback = '']) =>
    value == null ? fallback : value.toString();

List<Map<String, dynamic>> _maps(dynamic value) => value is List
    ? value
          .whereType<Map>()
          .map((item) => Map<String, dynamic>.from(item))
          .toList()
    : const [];

DateTime? _date(Map<String, dynamic> message) {
  final timestamp = message['timestamp'];
  if (timestamp is num) {
    return DateTime.fromMillisecondsSinceEpoch(timestamp.toInt());
  }
  return DateTime.tryParse(_str(timestamp))?.toLocal();
}
