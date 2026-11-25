import 'package:flutter/material.dart';

class CallTile {
  const CallTile({
    required this.id,
    required this.name,
    required this.self,
    required this.screen,
    required this.speaking,
    this.video,
  });

  final String id;
  final String name;
  final bool self;
  final bool screen;
  final bool speaking;
  final Widget? video;

  String get label => screen ? '$name · Screen' : name;
}

class CallStage extends StatefulWidget {
  const CallStage({super.key, required this.tiles, this.activeSpeaker});

  final List<CallTile> tiles;
  final String? activeSpeaker;

  @override
  State<CallStage> createState() => _CallStageState();
}

class _CallStageState extends State<CallStage> {
  bool _gallery = false;
  bool _showSelf = true;
  String? _pin;
  String? _lastSpeaker;

  @override
  void didUpdateWidget(CallStage oldWidget) {
    super.didUpdateWidget(oldWidget);
    _lastSpeaker =
        widget.activeSpeaker ?? oldWidget.activeSpeaker ?? _lastSpeaker;
    if (!widget.tiles.any((tile) => tile.id == _pin)) _pin = null;
  }

  Widget _tile(CallTile tile) {
    final colors = Theme.of(context).colorScheme;
    final pinned = _pin == tile.id;
    final action = '${pinned ? 'Unpin' : 'Pin'} ${tile.label}';
    return Semantics(
      label: tile.speaking ? '${tile.label}, speaking' : tile.label,
      child: Container(
        key: ValueKey(tile.id),
        clipBehavior: Clip.antiAlias,
        decoration: BoxDecoration(
          color: colors.surfaceContainerHighest,
          borderRadius: BorderRadius.circular(12),
        ),
        foregroundDecoration: BoxDecoration(
          borderRadius: BorderRadius.circular(12),
          border: tile.speaking
              ? Border.all(color: colors.primary, width: 3)
              : null,
        ),
        child: Stack(
          fit: StackFit.expand,
          children: [
            tile.video ??
                const Center(child: Icon(Icons.person_outline, size: 36)),
            Align(
              alignment: Alignment.bottomLeft,
              child: Container(
                color: Colors.black87,
                padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
                child: Text(
                  tile.label,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: const TextStyle(color: Colors.white),
                ),
              ),
            ),
            Align(
              alignment: Alignment.topRight,
              child: IconButton(
                tooltip: action,
                style: IconButton.styleFrom(
                  backgroundColor: Colors.black54,
                  foregroundColor: Colors.white,
                ),
                onPressed: () => setState(() {
                  _pin = pinned ? null : tile.id;
                  if (!pinned) _gallery = false;
                }),
                icon: Icon(
                  pinned ? Icons.push_pin : Icons.push_pin_outlined,
                  size: 18,
                ),
              ),
            ),
          ],
        ),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final tiles = widget.tiles
        .where((tile) => _showSelf || !tile.self || tile.screen)
        .toList();
    final focus =
        tiles.where((tile) => tile.id == _pin).firstOrNull ??
        tiles.where((tile) => tile.screen).firstOrNull ??
        tiles
            .where((tile) => tile.id == (widget.activeSpeaker ?? _lastSpeaker))
            .firstOrNull ??
        tiles.where((tile) => !tile.self).firstOrNull ??
        tiles.firstOrNull;
    final others = tiles.where((tile) => tile != focus).toList();
    return Column(
      children: [
        Row(
          mainAxisAlignment: MainAxisAlignment.end,
          children: [
            IconButton(
              tooltip: _gallery ? 'Speaker view' : 'Gallery view',
              onPressed: () => setState(() => _gallery = !_gallery),
              icon: Icon(
                _gallery ? Icons.spatial_audio_off_outlined : Icons.grid_view,
              ),
            ),
            if (widget.tiles.any((tile) => tile.self && !tile.screen))
              IconButton(
                tooltip: _showSelf ? 'Hide self-view' : 'Show self-view',
                onPressed: () => setState(() {
                  _showSelf = !_showSelf;
                  if (!_showSelf &&
                      widget.tiles.any(
                        (tile) => tile.id == _pin && tile.self && !tile.screen,
                      )) {
                    _pin = null;
                  }
                }),
                icon: Icon(
                  _showSelf
                      ? Icons.visibility_off_outlined
                      : Icons.visibility_outlined,
                ),
              ),
          ],
        ),
        Expanded(
          child: _gallery
              ? LayoutBuilder(
                  builder: (context, size) => GridView.builder(
                    gridDelegate: SliverGridDelegateWithFixedCrossAxisCount(
                      crossAxisCount: size.maxWidth < 420 ? 2 : 3,
                      crossAxisSpacing: 6,
                      mainAxisSpacing: 6,
                      childAspectRatio: 1.25,
                    ),
                    itemCount: tiles.length,
                    itemBuilder: (context, index) => _tile(tiles[index]),
                  ),
                )
              : focus == null
              ? const Center(child: Icon(Icons.videocam_off_outlined))
              : Column(
                  children: [
                    Expanded(child: _tile(focus)),
                    if (others.isNotEmpty)
                      SizedBox(
                        height: 90,
                        child: ListView.separated(
                          padding: const EdgeInsets.only(top: 6),
                          scrollDirection: Axis.horizontal,
                          itemCount: others.length,
                          separatorBuilder: (_, _) => const SizedBox(width: 6),
                          itemBuilder: (_, index) =>
                              SizedBox(width: 128, child: _tile(others[index])),
                        ),
                      ),
                  ],
                ),
        ),
      ],
    );
  }
}
