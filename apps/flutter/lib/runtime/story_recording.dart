import 'dart:typed_data';

class StoryMedia {
  const StoryMedia(this.bytes, this.name, this.type);
  final Uint8List bytes;
  final String name, type;
  Map<String, dynamic> toMap() => {
    'bytes': bytes,
    'name': name,
    'media_type': type,
  };
}

abstract interface class StoryRecording {
  Future<StoryMedia> finish();
  Future<void> cancel();
}
