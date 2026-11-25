import 'package:flutter/material.dart';

import '../bridge.dart';
import 'conversation.dart';
import '../runtime/context.dart';

class RemoteCommunitiesScreen extends StatefulWidget {
  const RemoteCommunitiesScreen({super.key, required this.bridge});
  final WaddleBridge bridge;

  @override
  State<RemoteCommunitiesScreen> createState() => _RemoteCommunitiesState();
}

class _RemoteCommunitiesState extends State<RemoteCommunitiesScreen> {
  final _address = TextEditingController();
  Map<String, dynamic>? _result;
  List<Map<String, dynamic>> _saved = [];
  String? _error, _notice;
  bool _busy = false, _bookmarkError = false;

  @override
  void initState() {
    super.initState();
    widget.bridge.addListener(_bridgeChanged);
    _loadSaved();
  }

  void _bridgeChanged() {
    final result = _result;
    if (result?['kind'] != 'room') return;
    final state = widget.bridge.topic('connection')['state'];
    final confirmed = widget.bridge.home['confirmedRooms'];
    final bool? joined = state != null && state != 'ready'
        ? false
        : confirmed is List
        ? confirmed.contains(result!['address'])
        : null;
    if (joined != null && joined != result!['joined']) {
      setState(() => _result = {...result, 'joined': joined});
    }
  }

  Future<void> _loadSaved() async {
    final owner = widget.bridge.generation;
    try {
      final saved = runtimeRows(await widget.bridge.invoke('remote.saved'));
      if (mounted && widget.bridge.generation == owner) {
        setState(() => _saved = saved);
      }
    } catch (error) {
      if (mounted && widget.bridge.generation == owner) {
        setState(
          () => _error = 'Saved Spaces are unavailable. ${error.toString()}',
        );
      }
    }
  }

  Future<void> _run(String method, String address) async {
    if (_busy || address.trim().isEmpty) return;
    final owner = widget.bridge.generation;
    setState(() {
      _busy = true;
      _error = null;
      _notice = null;
      if (method == 'lookup') {
        _result = null;
        _bookmarkError = false;
      }
    });
    try {
      final reply = runtimeMap(
        await widget.bridge.invoke('remote.$method', {'address': address}),
      );
      if (!mounted || widget.bridge.generation != owner) return;
      if (method == 'lookup') {
        setState(() => _result = reply);
      } else {
        setState(() {
          _notice = reply['message'] as String?;
          _bookmarkError = reply['bookmarkError'] == true;
          if (reply.containsKey('joined') && _result != null) {
            _result = {..._result!, 'joined': reply['joined']};
          }
        });
        await _loadSaved();
      }
    } catch (error) {
      if (mounted && widget.bridge.generation == owner) {
        setState(() => _error = error.toString());
      }
    } finally {
      if (mounted && widget.bridge.generation == owner) {
        setState(() => _busy = false);
      }
    }
  }

  Widget _entry(Map<String, dynamic> entry, {bool saved = false}) {
    final address = entry['address'] as String;
    final name = entry['name'] as String?;
    return ListTile(
      title: Text(name ?? address),
      subtitle: entry['status'] is String
          ? Text("$address\n${entry['status']}")
          : name == null
          ? null
          : Text(address),
      trailing: saved
          ? PopupMenuButton<String>(
              enabled: !_busy,
              onSelected: (method) => _run(method, address),
              itemBuilder: (_) => [
                if (entry['unsaved'] == true)
                  const PopupMenuItem(
                    value: 'retrySave',
                    child: Text('Retry saving preference'),
                  ),
                if (entry['follow'] == true)
                  const PopupMenuItem(
                    value: 'unsubscribe',
                    child: Text('Unsubscribe from Space'),
                  ),
                if (entry['follow'] != true)
                  const PopupMenuItem(
                    value: 'forget',
                    child: Text('Forget Space'),
                  ),
              ],
            )
          : const Icon(Icons.chevron_right),
      onTap: _busy
          ? null
          : () {
              _address.text = address;
              _run('lookup', address);
            },
    );
  }

  @override
  Widget build(BuildContext context) {
    final result = _result;
    final address = result?['address'] as String? ?? '';
    final space = result?['kind'] == 'space';
    final room = result?['kind'] == 'room';
    final saved = _saved
        .where((entry) => entry['address'] == address)
        .firstOrNull;
    final joined = result?['joined'] == true;
    return Scaffold(
      appBar: AppBar(title: const Text('Remote communities')),
      body: ListView(
        padding: const EdgeInsets.all(16),
        children: [
          TextField(
            controller: _address,
            enabled: !_busy,
            decoration: const InputDecoration(
              labelText: 'Room, Space or server address',
            ),
            onSubmitted: (text) => _run('lookup', text),
          ),
          FilledButton(
            onPressed: _busy ? null : () => _run('lookup', _address.text),
            child: const Text('Look up address'),
          ),
          if (_busy) const LinearProgressIndicator(),
          if (_error != null)
            Text(
              _error!,
              style: TextStyle(color: Theme.of(context).colorScheme.error),
            ),
          if (_notice != null) Text(_notice!),
          if (result != null) ...[
            ListTile(
              title: Text(result['name'] as String? ?? address),
              subtitle: Text(address),
            ),
            if (result['description'] is String)
              Text(result['description'] as String),
            if (result['kind'] == 'other')
              const Text(
                'The host did not identify this address as a room or Space.',
              ),
            for (final flag in result['flags'] as List? ?? [])
              Chip(label: Text('$flag')),
            if (space) ...[
              const Text(
                'Following this Space subscribes to updates. Each room decides who can join.',
              ),
              FilledButton(
                onPressed: _busy
                    ? null
                    : () => _run(
                        saved?['follow'] == true ? 'unsubscribe' : 'follow',
                        address,
                      ),
                child: Text(
                  saved?['follow'] == true
                      ? 'Unsubscribe from Space'
                      : 'Follow Space',
                ),
              ),
              if (saved?['unsaved'] == true)
                TextButton(
                  onPressed: _busy ? null : () => _run('retrySave', address),
                  child: const Text('Retry saving preference'),
                ),
              if (saved != null && saved['follow'] != true)
                TextButton(
                  onPressed: _busy ? null : () => _run('forget', address),
                  child: const Text('Forget Space'),
                ),
            ],
            if (room)
              FilledButton(
                onPressed: _busy
                    ? null
                    : () => _run(joined ? 'leave' : 'join', address),
                child: Text(joined ? 'Leave room' : 'Join room'),
              ),
            if (room && joined)
              TextButton(
                onPressed: () => Navigator.of(context).push(
                  MaterialPageRoute<void>(
                    builder: (_) => Scaffold(
                      appBar: AppBar(
                        title: Text(result['name'] as String? ?? address),
                      ),
                      body: ConversationScreen(
                        bridge: widget.bridge,
                        jid: address,
                        title: result['name'] as String? ?? address,
                        group: true,
                      ),
                    ),
                  ),
                ),
                child: const Text('Open conversation'),
              ),
            if (room && _bookmarkError)
              TextButton(
                onPressed: _busy ? null : () => _run('saveRoom', address),
                child: const Text('Retry saving bookmark'),
              ),
            if (room &&
                !joined &&
                runtimeRows(
                  widget.bridge.home['rooms'],
                ).any((r) => r['jid'] == address))
              TextButton(
                onPressed: _busy ? null : () => _run('forgetRoom', address),
                child: const Text('Forget room bookmark'),
              ),
            for (final entry in runtimeRows(result['entries'])) _entry(entry),
          ],
          if (_saved.isNotEmpty) ...[
            const Divider(),
            const ListTile(title: Text('Saved Spaces')),
            for (final entry in _saved) _entry(entry, saved: true),
          ],
        ],
      ),
    );
  }

  @override
  void dispose() {
    widget.bridge.removeListener(_bridgeChanged);
    _address.dispose();
    super.dispose();
  }
}
