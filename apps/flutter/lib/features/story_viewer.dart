import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:video_player/video_player.dart';
import 'package:video_player_media_kit/video_player_media_kit.dart';

import '../bridge.dart';

class StoryViewer extends StatefulWidget {
  const StoryViewer({
    super.key,
    required this.bridge,
    required this.stories,
    required this.initialIndex,
  });
  final WaddleBridge bridge;
  final List<Map<String, dynamic>> stories;
  final int initialIndex;

  @override
  State<StoryViewer> createState() => _StoryViewerState();
}

class _StoryViewerState extends State<StoryViewer> with WidgetsBindingObserver {
  late int _index;
  bool _paused = false, _muted = true, _foreground = true;
  Timer? _advance, _expires;
  VideoPlayerController? _video;
  String? _error;
  Uri? _url;
  int _revision = 0;
  String? _read;

  Map<String, dynamic> get _story => widget.stories[_index];
  bool get _isVideo => '${_story['media_type']}'.startsWith('video/');
  bool get _callActive => !{
    null,
    'idle',
    'ended',
    'failed',
  }.contains(widget.bridge.topic('call')['state']);

  @override
  void initState() {
    super.initState();
    _index = widget.initialIndex;
    widget.bridge.addListener(_volume);
    WidgetsBinding.instance.addObserver(this);
    _open();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    _foreground = state == AppLifecycleState.resumed;
    _playback();
  }

  @override
  void dispose() {
    _revision++;
    WidgetsBinding.instance.removeObserver(this);
    widget.bridge.removeListener(_volume);
    _advance?.cancel();
    _expires?.cancel();
    _disposeVideo();
    super.dispose();
  }

  void _disposeVideo() {
    final video = _video;
    _video = null;
    if (video != null) {
      video.removeListener(_onFrame);
      unawaited(video.dispose());
    }
  }

  void _volume() {
    final controller = _video;
    if (controller != null && controller.value.isInitialized) {
      final volume = _muted || _callActive ? 0.0 : 1.0;
      if (controller.value.volume != volume) {
        unawaited(controller.setVolume(volume));
      }
    }
  }

  void _onFrame() {
    final controller = _video;
    if (controller == null || !mounted) return;
    if (controller.value.hasError && _error == null) {
      setState(() => _error = 'This video could not be played.');
    } else if (controller.value.isCompleted && !_paused && _foreground) {
      _move(1);
    }
  }

  void _markRead() {
    final key = '${_story['author']}/${_story['id']}';
    if (_read == key) return;
    _read = key;
    unawaited(
      widget.bridge
          .invoke('community.readStory', {'id': _story['id']})
          .catchError((Object _) => null),
    );
  }

  Future<void> _open() async {
    final revision = ++_revision;
    _advance?.cancel();
    _expires?.cancel();
    _disposeVideo();
    _paused = false;
    _error = null;
    _url = Uri.tryParse('${_story['media_url']}');
    if (_url?.scheme != 'https' ||
        _url!.host.isEmpty ||
        _url!.userInfo.isNotEmpty) {
      _error = 'This story has no usable media link.';
      return;
    }
    final expiry = DateTime.tryParse('${_story['expires']}');
    if (expiry != null) {
      _expires = Timer(expiry.difference(DateTime.now()), () => _move(1));
    }
    if (!_isVideo) return;
    try {
      VideoPlayerMediaKit.ensureInitialized(linux: true);
      final controller = VideoPlayerController.networkUrl(
        _url!,
        videoPlayerOptions: VideoPlayerOptions(mixWithOthers: true),
      );
      _video = controller;
      await controller.initialize();
      if (!mounted || revision != _revision) return;
      controller.addListener(_onFrame);
      _volume();
      _markRead();
      setState(() {});
      _playback();
    } catch (_) {
      if (mounted && revision == _revision) {
        setState(() => _error = 'This video could not be played.');
      }
    }
  }

  void _playback() {
    _advance?.cancel();
    if (_error != null) return;
    final playing = !_paused && _foreground;
    final controller = _video;
    if (controller != null && controller.value.isInitialized) {
      unawaited(playing ? controller.play() : controller.pause());
    } else if (!_isVideo && playing && _read != null) {
      _advance = Timer(const Duration(seconds: 6), () => _move(1));
    }
  }

  void _move(int direction) {
    if (!mounted || ModalRoute.of(context)?.isCurrent == false) return;
    var next = _index + direction;
    while (next >= 0 && next < widget.stories.length) {
      final expiry = DateTime.tryParse('${widget.stories[next]['expires']}');
      if (expiry == null || expiry.isAfter(DateTime.now())) break;
      next += direction;
    }
    if (next < 0) return;
    if (next >= widget.stories.length) {
      Navigator.of(context).pop();
      return;
    }
    setState(() {
      _index = next;
      _read = null;
    });
    unawaited(_open());
  }

  void _togglePause() {
    setState(() => _paused = !_paused);
    _playback();
  }

  void _toggleMute() {
    setState(() => _muted = !_muted);
    _volume();
  }

  Widget _media() {
    if (_error != null) return Text(_error!);
    if (_isVideo) {
      final controller = _video;
      if (controller == null || !controller.value.isInitialized) {
        return const CircularProgressIndicator();
      }
      return AspectRatio(
        aspectRatio: controller.value.aspectRatio,
        child: VideoPlayer(controller),
      );
    }
    final revision = _revision;
    return Image.network(
      _url!.toString(),
      fit: BoxFit.contain,
      frameBuilder: (context, child, frame, synchronous) {
        if ((synchronous || frame != null) && _read == null) {
          WidgetsBinding.instance.addPostFrameCallback((_) {
            if (mounted && revision == _revision && _read == null) {
              _markRead();
              _playback();
            }
          });
        }
        return child;
      },
      errorBuilder: (_, _, _) => const Text('This image could not be opened.'),
    );
  }

  @override
  Widget build(BuildContext context) => CallbackShortcuts(
    bindings: {
      const SingleActivator(LogicalKeyboardKey.arrowRight): () => _move(1),
      const SingleActivator(LogicalKeyboardKey.arrowLeft): () => _move(-1),
      const SingleActivator(LogicalKeyboardKey.space): _togglePause,
      const SingleActivator(LogicalKeyboardKey.keyM): _toggleMute,
    },
    child: Focus(
      autofocus: true,
      child: Scaffold(
        appBar: AppBar(
          title: Text('${_story['author'] ?? 'Story'}'),
          actions: [
            if (_isVideo)
              IconButton(
                onPressed: _toggleMute,
                tooltip: _muted ? 'Unmute' : 'Mute',
                icon: Icon(_muted ? Icons.volume_off : Icons.volume_up),
              ),
          ],
        ),
        body: SafeArea(
          child: Column(
            children: [
              LinearProgressIndicator(
                value: (_index + 1) / widget.stories.length,
              ),
              Expanded(
                child: GestureDetector(
                  onTap: _togglePause,
                  onHorizontalDragEnd: (details) {
                    if ((details.primaryVelocity ?? 0).abs() > 100) {
                      _move(details.primaryVelocity! < 0 ? 1 : -1);
                    }
                  },
                  child: Center(child: _media()),
                ),
              ),
              if (_story['body'] case final String caption)
                Padding(
                  padding: const EdgeInsets.all(12),
                  child: Text(caption),
                ),
              Row(
                mainAxisAlignment: MainAxisAlignment.spaceEvenly,
                children: [
                  IconButton(
                    tooltip: 'Previous story',
                    onPressed: _index > 0 ? () => _move(-1) : null,
                    icon: const Icon(Icons.chevron_left),
                  ),
                  IconButton(
                    tooltip: _paused ? 'Play' : 'Pause',
                    onPressed: _togglePause,
                    icon: Icon(_paused ? Icons.play_arrow : Icons.pause),
                  ),
                  IconButton(
                    tooltip: 'Next story',
                    onPressed: () => _move(1),
                    icon: const Icon(Icons.chevron_right),
                  ),
                ],
              ),
            ],
          ),
        ),
      ),
    ),
  );
}
