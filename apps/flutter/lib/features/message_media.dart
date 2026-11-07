/// Identifies a message consisting of a single HTTPS image or GIPHY media URL.
bool isInlineImageUrl(String body) {
  final value = body.trim();
  if (value.contains(RegExp(r'\s'))) return false;
  final uri = Uri.tryParse(value);
  if (uri == null ||
      uri.scheme != 'https' ||
      uri.host.isEmpty ||
      uri.userInfo.isNotEmpty) {
    return false;
  }
  final giphy =
      uri.host == 'i.giphy.com' ||
      RegExp(r'^media\d*\.giphy\.com$').hasMatch(uri.host);
  return giphy ||
      RegExp(
        r'\.(gif|png|jpe?g|webp|avif|bmp)$',
        caseSensitive: false,
      ).hasMatch(uri.path);
}
