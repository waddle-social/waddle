import 'dart:async';

import 'package:flutter/material.dart';

class GifPickerSheet extends StatefulWidget {
  const GifPickerSheet({
    super.key,
    required this.search,
    required this.onSelect,
  });

  final Future<List<Map<String, dynamic>>> Function(String query) search;
  final void Function(String url) onSelect;

  @override
  State<GifPickerSheet> createState() => _GifPickerSheetState();
}

class _GifPickerSheetState extends State<GifPickerSheet> {
  final _query = TextEditingController();
  Timer? _debounce;
  List<Map<String, dynamic>> _results = [];
  String? _failure;
  bool _loading = true;
  int _generation = 0;

  @override
  void initState() {
    super.initState();
    unawaited(_search('', ++_generation));
  }

  void _changed(String value) {
    _debounce?.cancel();
    final generation = ++_generation;
    setState(() {
      _loading = true;
      _failure = null;
      _results = [];
    });
    if (value.trim().isEmpty) {
      unawaited(_search('', generation));
    } else {
      _debounce = Timer(const Duration(milliseconds: 300), () {
        unawaited(_search(value.trim(), generation));
      });
    }
  }

  Future<void> _search(String value, int generation) async {
    try {
      final results = await widget.search(value);
      if (!mounted || generation != _generation) return;
      setState(() {
        _results = results;
        _loading = false;
      });
    } catch (error) {
      if (!mounted || generation != _generation) return;
      setState(() {
        _failure = error is StateError
            ? error.message.toString()
            : 'GIF search is unavailable.';
        _loading = false;
      });
    }
  }

  @override
  void dispose() {
    _generation++;
    _debounce?.cancel();
    _query.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => Padding(
    padding: EdgeInsets.only(bottom: MediaQuery.viewInsetsOf(context).bottom),
    child: SafeArea(
      child: SizedBox(
        height:
            (MediaQuery.sizeOf(context).height -
                    MediaQuery.viewInsetsOf(context).bottom)
                .clamp(0.0, double.infinity) *
            .7,
        child: Column(
          children: [
            Padding(
              padding: const EdgeInsets.fromLTRB(20, 8, 20, 16),
              child: SearchBar(
                controller: _query,
                hintText: 'Search GIFs',
                leading: const Icon(Icons.search),
                onChanged: _changed,
              ),
            ),
            Expanded(child: _content()),
            const Padding(
              padding: EdgeInsets.all(12),
              child: Text('Powered by GIPHY'),
            ),
          ],
        ),
      ),
    ),
  );

  Widget _content() {
    if (_loading) return const Center(child: CircularProgressIndicator());
    if (_failure != null) return Center(child: Text(_failure!));
    if (_results.isEmpty) return const Center(child: Text('No GIFs found'));
    return GridView.builder(
      padding: const EdgeInsets.symmetric(horizontal: 12),
      gridDelegate: const SliverGridDelegateWithFixedCrossAxisCount(
        crossAxisCount: 3,
        crossAxisSpacing: 4,
        mainAxisSpacing: 4,
      ),
      itemCount: _results.length,
      itemBuilder: (context, index) {
        final gif = _results[index];
        final preview = Uri.tryParse('${gif['previewUrl'] ?? ''}');
        final original = Uri.tryParse('${gif['originalUrl'] ?? ''}');
        final usable =
            original != null &&
            original.scheme == 'https' &&
            original.host.isNotEmpty;
        return InkWell(
          onTap: usable ? () => widget.onSelect(original.toString()) : null,
          child: Semantics(
            label: '${gif['title'] ?? 'GIF'}',
            button: true,
            child:
                preview != null &&
                    preview.scheme == 'https' &&
                    preview.host.isNotEmpty
                ? Image.network(
                    preview.toString(),
                    fit: BoxFit.cover,
                    errorBuilder: (_, _, _) =>
                        const Icon(Icons.broken_image_outlined),
                  )
                : const Icon(Icons.gif_box_outlined),
          ),
        );
      },
    );
  }
}
