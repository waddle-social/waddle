import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:waddle_flutter/features/gif_picker.dart';

void main() {
  testWidgets('loads trending and debounces searches without stale results', (
    tester,
  ) async {
    final requests = <String>[];
    final trending = Completer<List<Map<String, dynamic>>>();
    final search = Completer<List<Map<String, dynamic>>>();
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: GifPickerSheet(
            search: (query) {
              requests.add(query);
              return query.isEmpty ? trending.future : search.future;
            },
            onSelect: (_) {},
          ),
        ),
      ),
    );
    expect(requests, ['']);
    expect(find.text('Powered by GIPHY'), findsOneWidget);
    await tester.enterText(find.byType(EditableText), 'cats');
    await tester.pump(const Duration(milliseconds: 299));
    expect(requests, ['']);
    trending.completeError(StateError('Old request failed'));
    await tester.pump();
    expect(find.text('Old request failed'), findsNothing);
    await tester.pump(const Duration(milliseconds: 1));
    expect(requests, ['', 'cats']);
    search.complete([]);
    await tester.pump();
    expect(find.text('No GIFs found'), findsOneWidget);
  });

  testWidgets('shows service errors instead of an empty result', (
    tester,
  ) async {
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: GifPickerSheet(
            search: (_) async => throw StateError('GIF search is busy.'),
            onSelect: (_) {},
          ),
        ),
      ),
    );
    await tester.pump();
    expect(find.text('GIF search is busy.'), findsOneWidget);
    expect(find.text('No GIFs found'), findsNothing);
  });

  testWidgets('selects the original HTTPS GIF URL', (tester) async {
    String? selected;
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: GifPickerSheet(
            search: (_) async => [
              {'title': 'A GIF', 'originalUrl': 'https://example.org/a.gif'},
            ],
            onSelect: (url) => selected = url,
          ),
        ),
      ),
    );
    await tester.pump();
    await tester.tap(find.byIcon(Icons.gif_box_outlined));
    expect(selected, 'https://example.org/a.gif');
  });
}
