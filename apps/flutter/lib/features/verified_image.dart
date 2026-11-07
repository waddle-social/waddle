import 'dart:typed_data';

import 'package:flutter/material.dart';

/// Retains only a bounded number of verified image bytes across visible rows.
class VerifiedImageCache {
  VerifiedImageCache({this.maxBytes = 16 * 1024 * 1024});

  final int maxBytes;
  final _images = <String, Uint8List>{};
  final _pending = <String, Future<Uint8List>>{};
  int _size = 0;
  int _generation = 0;

  Future<Uint8List> load(String key, Future<Uint8List> Function() fetch) {
    final cached = _images.remove(key);
    if (cached != null) {
      _images[key] = cached;
      return Future.value(cached);
    }
    final pending = _pending[key];
    if (pending != null) return pending;
    final generation = _generation;
    late final Future<Uint8List> result;
    result = Future.sync(fetch)
        .then((bytes) {
          if (generation == _generation && bytes.length <= maxBytes) {
            while (_size + bytes.length > maxBytes && _images.isNotEmpty) {
              _size -= _images.remove(_images.keys.first)!.length;
            }
            _images[key] = bytes;
            _size += bytes.length;
          }
          return bytes;
        })
        .whenComplete(() {
          if (identical(_pending[key], result)) _pending.remove(key);
        });
    _pending[key] = result;
    return result;
  }

  void clear() {
    _generation++;
    _images.clear();
    _pending.clear();
    _size = 0;
  }

  void evict(String key) {
    final removed = _images.remove(key);
    if (removed != null) _size -= removed.length;
  }
}

class VerifiedImage extends StatefulWidget {
  const VerifiedImage({
    super.key,
    required this.name,
    required this.cache,
    required this.cacheKey,
    required this.fetch,
    required this.connected,
    required this.onOpen,
  });

  final String name;
  final VerifiedImageCache cache;
  final String cacheKey;
  final Future<Uint8List> Function() fetch;
  final bool connected;
  final VoidCallback onOpen;

  @override
  State<VerifiedImage> createState() => _VerifiedImageState();
}

class _VerifiedImageState extends State<VerifiedImage> {
  late Future<Uint8List> _image;
  bool _failed = false;
  int _attempt = 0;

  @override
  void initState() {
    super.initState();
    _load();
  }

  @override
  void didUpdateWidget(VerifiedImage oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.cacheKey != widget.cacheKey ||
        (_failed && !oldWidget.connected && widget.connected)) {
      _load();
    }
  }

  void _load() {
    _failed = false;
    _attempt++;
    _image = widget.cache.load(widget.cacheKey, widget.fetch);
  }

  @override
  Widget build(BuildContext context) => FutureBuilder<Uint8List>(
    key: ValueKey(_attempt),
    future: _image,
    builder: (context, snapshot) {
      if (snapshot.hasData) {
        return Image.memory(
          snapshot.data!,
          fit: BoxFit.contain,
          height: 220,
          semanticLabel: widget.name,
          errorBuilder: (_, _, _) {
            _failed = true;
            widget.cache.evict(widget.cacheKey);
            return _failure();
          },
        );
      }
      if (snapshot.connectionState != ConnectionState.done) {
        return const SizedBox(
          height: 100,
          child: Center(child: CircularProgressIndicator()),
        );
      }
      _failed = true;
      return _failure();
    },
  );

  Widget _failure() => ListTile(
    title: Text(widget.name, maxLines: 2, overflow: TextOverflow.ellipsis),
    leading: const Icon(Icons.broken_image_outlined),
    onTap: widget.onOpen,
    trailing: IconButton(
      tooltip: 'Retry image',
      icon: const Icon(Icons.refresh),
      onPressed: () => setState(_load),
    ),
  );
}
