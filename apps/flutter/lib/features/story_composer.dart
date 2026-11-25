import 'package:flutter/material.dart';

import '../runtime/story_recording.dart';
import '../bridge.dart';
import 'story_camera.dart';

class StoryComposer extends StatefulWidget {
  const StoryComposer({super.key, this.bridge, this.open = openStoryCamera});
  final WaddleBridge? bridge;
  final Future<StoryCamera> Function(bool) open;
  @override
  State<StoryComposer> createState() => _StoryComposerState();
}

class _StoryComposerState extends State<StoryComposer> {
  String _caption = '';
  StoryMedia? _media;

  Future<void> _capture(bool video) async {
    final result = await showDialog<dynamic>(
      context: context,
      useRootNavigator: false,
      builder: (_) => StoryCameraDialog(
        video: video,
        open: widget.open,
        bridge: widget.bridge,
      ),
    );
    if (!mounted) return;
    if (result == 'pick') {
      Navigator.pop(context, {'body': _caption});
    }
    if (result is StoryMedia) {
      setState(() => _media = result);
    }
  }

  @override
  Widget build(BuildContext context) => AlertDialog(
    title: const Text('New story'),
    content: SingleChildScrollView(
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          TextField(
            decoration: const InputDecoration(labelText: 'Caption (optional)'),
            maxLines: 2,
            onChanged: (text) => _caption = text.trim(),
          ),
          if (_media case final media?)
            if (media.type.startsWith('image/'))
              Image.memory(media.bytes, height: 120)
            else
              const ListTile(
                leading: Icon(Icons.videocam),
                title: Text('Video recorded'),
              ),
          Wrap(
            children: [
              TextButton.icon(
                icon: const Icon(Icons.photo_camera_outlined),
                label: const Text('Take photo'),
                onPressed: () => _capture(false),
              ),
              TextButton.icon(
                icon: const Icon(Icons.videocam_outlined),
                label: const Text('Record video'),
                onPressed: () => _capture(true),
              ),
            ],
          ),
        ],
      ),
    ),
    actions: [
      TextButton(
        onPressed: () => Navigator.pop(context),
        child: const Text('Cancel'),
      ),
      TextButton(
        onPressed: () => Navigator.pop(context, {'body': _caption}),
        child: const Text('Choose existing media'),
      ),
      if (_media != null)
        FilledButton(
          onPressed: () => Navigator.pop(context, {
            'body': _caption,
            'capture': _media!.toMap(),
          }),
          child: const Text('Publish story'),
        ),
    ],
  );
}
