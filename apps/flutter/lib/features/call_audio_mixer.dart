part of 'surfaces.dart';

Future<void> _showCallMixer(
  BuildContext context,
  WaddleBridge bridge,
  String sid,
) async {
  await showModalBottomSheet<void>(
    context: context,
    showDragHandle: true,
    builder: (sheet) => ListenableBuilder(
      listenable: bridge,
      builder: (sheet, _) {
        final tracks = bridge.call['sid'] == sid
            ? _rows(bridge.call['audioTracks'])
            : const <Map<String, dynamic>>[];
        return SafeArea(
          child: ListView(
            shrinkWrap: true,
            children: [
              if (tracks.isEmpty)
                const ListTile(title: Text('No remote audio')),
              for (final track in tracks)
                _PlaybackSlider(
                  key: ValueKey(track['sid']),
                  track: track,
                  apply: (gain) => _command(sheet, bridge, 'call.volume', {
                    'expectedSid': sid,
                    'track': track['sid'],
                    'gain': gain,
                  }),
                ),
            ],
          ),
        );
      },
    ),
  );
}

class _PlaybackSlider extends StatefulWidget {
  const _PlaybackSlider({super.key, required this.track, required this.apply});
  final Map<String, dynamic> track;
  final Future<void> Function(double) apply;
  @override
  State<_PlaybackSlider> createState() => _PlaybackSliderState();
}

class _PlaybackSliderState extends State<_PlaybackSlider> {
  double? _drag;
  @override
  Widget build(BuildContext context) {
    final level = _drag ?? (widget.track['gain'] as num).toDouble();
    final name = widget.track['name'] as String;
    final label = widget.track['screen'] == true
        ? '$name · Screen audio'
        : name;
    final percent = '${(level * 100).round()}%';
    return ListTile(
      title: Text(label),
      trailing: Text(percent),
      subtitle: Slider(
        value: level,
        max: 2,
        divisions: 40,
        semanticFormatterCallback: (value) => '${(value * 100).round()}%',
        onChanged: (value) => setState(() => _drag = value),
        onChangeEnd: (value) async {
          await widget.apply(value);
          if (mounted && _drag == value) setState(() => _drag = null);
        },
      ),
    );
  }
}
