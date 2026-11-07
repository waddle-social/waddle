import 'package:flutter_test/flutter_test.dart';
import 'package:waddle_flutter/features/message_media.dart';

void main() {
  test('recognizes lone image and GIPHY media URLs', () {
    expect(isInlineImageUrl(' https://media3.giphy.com/media/abc '), isTrue);
    expect(isInlineImageUrl('https://i.giphy.com/abc'), isTrue);
    expect(isInlineImageUrl('https://example.org/picture.AVIF?v=1'), isTrue);
    expect(isInlineImageUrl('https://giphy.com/gifs/abc'), isFalse);
    expect(isInlineImageUrl('https://media.giphy.com.evil.org/abc'), isFalse);
    expect(isInlineImageUrl('https://user@media.giphy.com/abc'), isFalse);
    expect(isInlineImageUrl('http://i.giphy.com/abc'), isFalse);
    expect(isInlineImageUrl('Look at https://i.giphy.com/abc'), isFalse);
  });
}
