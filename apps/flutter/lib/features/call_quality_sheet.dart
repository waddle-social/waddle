import 'dart:async';

import 'package:flutter/material.dart';

import '../bridge.dart';
import '../runtime/call_quality.dart' show voiceProcessingLabels;

Future<void> showCallQuality(
  BuildContext context,
  WaddleBridge bridge,
  String sid,
) => showModalBottomSheet<void>(
  context: context,
  useRootNavigator: false,
  showDragHandle: true,
  isScrollControlled: true,
  builder: (_) => _CallQualitySheet(bridge: bridge, sid: sid),
);

class _CallQualitySheet extends StatefulWidget {
  const _CallQualitySheet({required this.bridge, required this.sid});
  final WaddleBridge bridge;
  final String sid;
  @override
  State<_CallQualitySheet> createState() => _CallQualitySheetState();
}

class _CallQualitySheetState extends State<_CallQualitySheet> {
  Timer? _timer;
  var _fetching = false, _changing = false;
  late final int _owner;
  Map<String, dynamic> _data = {};
  String? _error;
  @override
  void initState() {
    super.initState();
    _owner = widget.bridge.generation;
    widget.bridge.addListener(_callChanged);
    unawaited(_refresh());
    _timer = Timer.periodic(
      const Duration(seconds: 1),
      (_) => unawaited(_refresh()),
    );
  }

  bool get _current =>
      mounted &&
      widget.bridge.generation == _owner &&
      widget.bridge.call['sid'] == widget.sid &&
      widget.bridge.call['state'] == 'active';
  void _callChanged() {
    if (_current) return;
    final route = ModalRoute.of(context);
    if (route != null && route.isActive) {
      Navigator.of(context).removeRoute(route);
    }
  }

  @override
  void dispose() {
    _timer?.cancel();
    widget.bridge.removeListener(_callChanged);
    super.dispose();
  }

  Future<void> _refresh() async {
    if (!_current || _fetching || _changing) return;
    _fetching = true;
    try {
      final data = await widget.bridge.invokeQuiet('call.quality', {
        'expectedSid': widget.sid,
      });
      if (_current && !_changing && data is Map) {
        setState(() => _data = Map<String, dynamic>.from(data));
      }
    } catch (error) {
      if (_current) setState(() => _error = WaddleBridge.describeError(error));
    } finally {
      _fetching = false;
    }
  }

  Future<void> _set(String key, bool enabled) async {
    if (!_current || _changing) return;
    setState(() {
      _changing = true;
      _error = null;
    });
    try {
      await widget.bridge.invokeQuiet('call.voiceProcessing', {
        'expectedSid': widget.sid,
        'key': key,
        'enabled': enabled,
      });
    } catch (error) {
      if (_current) setState(() => _error = WaddleBridge.describeError(error));
    } finally {
      if (mounted) setState(() => _changing = false);
      await _refresh();
    }
  }

  @override
  Widget build(BuildContext context) {
    final voice = _data['voice'] as Map? ?? const {};
    final streams = (_data['streams'] as List? ?? const []).whereType<Map>();
    return SafeArea(
      child: ConstrainedBox(
        constraints: BoxConstraints(
          maxHeight: MediaQuery.sizeOf(context).height * .8,
        ),
        child: ListView(
          shrinkWrap: true,
          children: [
            const ListTile(title: Text('Call settings')),
            for (final entry in voiceProcessingLabels.entries)
              if (voice[entry.key] is bool)
                SwitchListTile(
                  title: Text(entry.value),
                  value: voice[entry.key] as bool,
                  onChanged: _data['canChangeVoice'] == true && !_changing
                      ? (value) => _set(entry.key, value)
                      : null,
                ),
            if (_error != null)
              Padding(
                padding: const EdgeInsets.symmetric(horizontal: 16),
                child: Text(_error!),
              ),
            const ListTile(title: Text('Live media statistics')),
            if (streams.isEmpty)
              const ListTile(title: Text('Waiting for an active media track')),
            for (final stream in streams)
              ListTile(
                title: Text("${stream['participant']} · ${stream['source']}"),
                subtitle: Text(
                  [
                    if (stream['outgoing'] == true) 'Sending' else 'Receiving',
                    if (stream['layer'] case final String layer
                        when layer.isNotEmpty)
                      layer,
                    if (stream['codec'] != null) stream['codec'],
                    if (stream['resolution'] != null) stream['resolution'],
                    if (stream['fps'] != null) "${stream['fps']} fps",
                    if (stream['kbps'] != null) "${stream['kbps']} kbit/s",
                    if (stream['packetsLost'] != null)
                      "${stream['packetsLost']} packets lost",
                  ].join(' · '),
                ),
              ),
          ],
        ),
      ),
    );
  }
}
