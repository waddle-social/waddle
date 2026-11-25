import 'dart:convert';

import 'package:flutter/material.dart';

class ComposerEmojiPicker extends StatefulWidget {
  const ComposerEmojiPicker({super.key});

  // This is the same English keyword catalog pinned in the web workspace.
  static Future<Map<String, List<String>>> loadCatalog(AssetBundle bundle) =>
      bundle.loadStructuredData(
        'assets/emoji/emojilib-4.0.3.json',
        (source) async => (jsonDecode(source) as Map<String, dynamic>).map(
          (emoji, values) => MapEntry(emoji, (values as List).cast<String>()),
        ),
      );

  @override
  State<ComposerEmojiPicker> createState() => _ComposerEmojiPickerState();
}

class _ComposerEmojiPickerState extends State<ComposerEmojiPicker> {
  static const _frequent = ['👍', '❤️', '😂', '🎉', '👀', '🔥', '🙌', '💯'];

  late Future<Map<String, List<String>>> _catalog;
  String _query = '';

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    _catalog = ComposerEmojiPicker.loadCatalog(DefaultAssetBundle.of(context));
  }

  Iterable<String> _matches(Map<String, List<String>> catalog) sync* {
    if (_query.isEmpty) {
      yield* _frequent.where(catalog.containsKey);
      yield* catalog.keys.where((emoji) => !_frequent.contains(emoji));
      return;
    }
    final normalized = _query.toLowerCase().replaceAll(RegExp(r'\s+'), '_');
    final exact = <String>[];
    final partial = <String>[];
    for (final entry in catalog.entries) {
      if (entry.key == _query || entry.value.first == normalized) {
        exact.add(entry.key);
      } else if (entry.value.any(
        (word) => word.toLowerCase().contains(normalized),
      )) {
        partial.add(entry.key);
      }
    }
    yield* exact;
    yield* partial;
  }

  @override
  Widget build(BuildContext context) {
    return Dialog(
      insetPadding: const EdgeInsets.symmetric(horizontal: 16, vertical: 12),
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 392, maxHeight: 376),
        child: Padding(
          padding: const EdgeInsets.all(12),
          child: FutureBuilder<Map<String, List<String>>>(
            future: _catalog,
            builder: (context, snapshot) {
              final catalog = snapshot.data ?? const <String, List<String>>{};
              final matches = _matches(catalog).toList();
              return Column(
                children: [
                  Row(
                    children: [
                      Expanded(
                        child: TextField(
                          autofocus: true,
                          decoration: const InputDecoration(
                            hintText: 'Search emoji',
                            prefixIcon: Icon(Icons.search_rounded),
                            isDense: true,
                          ),
                          textInputAction: TextInputAction.search,
                          onChanged: (value) =>
                              setState(() => _query = value.trim()),
                          onSubmitted: (_) {
                            if (matches.isNotEmpty) {
                              Navigator.pop(context, matches.first);
                            }
                          },
                        ),
                      ),
                      IconButton(
                        tooltip: 'Close emoji picker',
                        icon: const Icon(Icons.close_rounded),
                        onPressed: () => Navigator.pop(context),
                      ),
                    ],
                  ),
                  const SizedBox(height: 8),
                  Expanded(
                    child: snapshot.hasError
                        ? const Center(
                            child: Text('Emoji could not be loaded.'),
                          )
                        : !snapshot.hasData
                        ? const Center(
                            child: CircularProgressIndicator(
                              semanticsLabel: 'Loading emoji',
                            ),
                          )
                        : matches.isEmpty
                        ? const Center(child: Text('No matching emoji'))
                        : LayoutBuilder(
                            builder: (context, size) => GridView.builder(
                              key: ValueKey(_query),
                              gridDelegate:
                                  SliverGridDelegateWithFixedCrossAxisCount(
                                    crossAxisCount: (size.maxWidth / 48)
                                        .floor()
                                        .clamp(1, 8),
                                  ),
                              itemCount: matches.length,
                              itemBuilder: (context, index) {
                                final emoji = matches[index];
                                final name = catalog[emoji]!.first.replaceAll(
                                  '_',
                                  ' ',
                                );
                                return IconButton(
                                  tooltip: 'Insert $name',
                                  onPressed: () =>
                                      Navigator.pop(context, emoji),
                                  icon: FittedBox(
                                    child: Text(
                                      emoji,
                                      style: const TextStyle(fontSize: 24),
                                    ),
                                  ),
                                );
                              },
                            ),
                          ),
                  ),
                ],
              );
            },
          ),
        ),
      ),
    );
  }
}
