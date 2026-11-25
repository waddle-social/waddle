part of '../app.dart';

/// The sidebar and dashboard often display the same peer simultaneously.
/// Share an XMPP avatar request across those rows and limit roster fetches.
class _AccountPhotos {
  _AccountPhotos(this.bridge)
    : generation = bridge.generation,
      owner = _accountScope(bridge);
  final WaddleBridge bridge;
  final int generation;
  final String owner;
  final _pending = <String, Future<Uint8List?>>{};
  final _recent = <String, Uint8List?>{};
  final _lanes = List.generate(4, (_) => Future<void>.value());
  int _lane = 0, _bytes = 0;
  bool active = true;
  bool get _current =>
      active &&
      bridge.generation == generation &&
      _accountScope(bridge) == owner;

  Future<Uint8List?> load(String jid) {
    if (_recent.containsKey(jid)) {
      final bytes = _recent.remove(jid);
      _recent[jid] = bytes;
      return Future.value(bytes);
    }
    return _pending.putIfAbsent(jid, () {
      final lane = _lane;
      _lane = (_lane + 1) % _lanes.length;
      final read = _lanes[lane]
          .then((_) async {
            if (!_current) return null;
            Uint8List? bytes;
            try {
              final data = await bridge.invokeQuiet('profile.avatarBytes', {
                'jid': jid,
              });
              if (data is Uint8List && data.isNotEmpty) bytes = data;
            } catch (_) {
              // Initials stay visible when the PEP node cannot be read.
            }
            if (!_current) return null;
            _recent[jid] = bytes;
            _bytes += bytes?.length ?? 0;
            while (_recent.length > 96 || _bytes > 8 * 1024 * 1024) {
              _bytes -= _recent.remove(_recent.keys.first)?.length ?? 0;
            }
            return bytes;
          })
          .whenComplete(() {
            _pending.remove(jid);
          });
      _lanes[lane] = read.then((_) {});
      return read;
    });
  }
}

class _PhotoScope extends InheritedWidget {
  const _PhotoScope({required this.photos, required super.child});
  final _AccountPhotos photos;
  @override
  bool updateShouldNotify(_PhotoScope oldWidget) => oldWidget.photos != photos;
}

class _AccountAvatar extends StatefulWidget {
  const _AccountAvatar({
    required this.jid,
    required this.name,
    required this.size,
    this.online = false,
  });
  final String jid, name;
  final double size;
  final bool online;
  @override
  State<_AccountAvatar> createState() => _AccountAvatarState();
}

class _AccountAvatarState extends State<_AccountAvatar> {
  _AccountPhotos? _source;
  String? _jid;
  Future<Uint8List?>? _request;
  @override
  Widget build(BuildContext context) {
    final source = context
        .dependOnInheritedWidgetOfExactType<_PhotoScope>()!
        .photos;
    if (_source != source || _jid != widget.jid) {
      _source = source;
      _jid = widget.jid;
      _request = widget.jid.isEmpty ? null : source.load(widget.jid);
    }
    final own = widget.jid == source.bridge.session['jid']
        ? source.bridge.topic('profile')['avatar']
        : null;
    return FutureBuilder<Uint8List?>(
      key: ValueKey((source, _jid)),
      future: _request,
      builder: (context, snapshot) => WaddleAvatar(
        name: widget.name,
        size: widget.size,
        online: widget.online,
        bytes: own is Uint8List ? own : snapshot.data,
      ),
    );
  }
}
