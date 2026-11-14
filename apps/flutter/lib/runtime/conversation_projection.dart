part of 'conversation_runtime.dart';

Map<String, dynamic> _localAttachment(Map<String, dynamic> file) {
  final encrypted = runtimeMap(file['encrypted']);
  return {
    'url': file['url'],
    'name': file['name'],
    'media_type': file['media_type'],
    'size': file['size'],
    'inline': file['disposition'] == 'inline',
    'encryption': encrypted.isEmpty
        ? null
        : {
            'cipher': encrypted['cipher'],
            'key_b64': encrypted['key_b64'],
            'iv_b64': encrypted['iv_b64'],
            'sha256_b64': runtimeRows(encrypted['hashes'])
                .where((row) => row['algo'] == 'sha-256')
                .map((row) => row['value_b64'])
                .toList(),
            'sources': encrypted['sources'] ?? [],
          },
  };
}

Map<String, dynamic> _sharedAttachment(Map<String, dynamic> attachment) {
  final encryption = runtimeMap(attachment['encryption']);
  return {
    'url': attachment['url'],
    'name': attachment['name'],
    'media_type': attachment['media_type'],
    'size': attachment['size'],
    'disposition': attachment['inline'] == true ? 'inline' : 'attachment',
    'hashes': [],
    'encrypted': encryption.isEmpty
        ? null
        : {
            'cipher': encryption['cipher'],
            'key_b64': encryption['key_b64'],
            'iv_b64': encryption['iv_b64'],
            'hashes': [
              for (final hash in (encryption['sha256_b64'] as List? ?? []))
                {'algo': 'sha-256', 'value_b64': hash},
            ],
            'sources': encryption['sources'] ?? [],
          },
  };
}

List<Map<String, dynamic>> _filePresentation(
  List<Map<String, dynamic>> files,
) => [
  for (var index = 0; index < files.length; index++)
    {
      'id': '$index',
      'name': files[index]['name'],
      'mimeType': files[index]['media_type'],
      'size': files[index]['size'],
      'width': files[index]['width'],
      'height': files[index]['height'],
      'encrypted': files[index]['encrypted'] != null,
      if (files[index]['encrypted'] == null) 'url': files[index]['url'],
    },
];

List<Map<String, dynamic>> _displayMarkup(
  Map<String, dynamic> raw,
  int displayLength,
) {
  final start = raw['reply_fallback_start'];
  final end = raw['reply_fallback_end'];
  final removed = start is int && end is int && start >= 0 && end >= start
      ? end - start
      : 0;
  final result = <Map<String, dynamic>>[];
  for (final span in runtimeRows(raw['markup_spans'])) {
    var first = span['start'];
    var last = span['end'];
    if (first is! int || last is! int || first < 0 || last <= first) {
      continue;
    }
    if (removed > 0) {
      if (first >= (end as int)) {
        first -= removed;
        last -= removed;
      } else if (last > (start as int)) {
        continue;
      }
    }
    if (last > displayLength) {
      continue;
    }
    result.add({
      'start': first,
      'end': last,
      'kind': span['span_type'],
      'uri': span['uri'],
    });
  }
  return result;
}

Map<String, dynamic> _scorePresentation(Map<String, dynamic> scores) => {
  'modelVersion': scores['model_version'],
  'scores': [
    for (final score in runtimeRows(scores['scores']))
      {
        'category': score['category'],
        'probability': score['probability'],
        'taxonomyVersion': score['taxonomy_version'],
      },
  ],
};

Map<String, dynamic> _projectRow(
  Map<String, dynamic> row,
  Map<String, dynamic> raw,
  List<Map<String, dynamic>> files,
  bool group,
  String ownJid,
  Set<String> pins,
) {
  final deleted = row['retracted'] == true;
  final body = deleted ? 'Message removed' : _string(row['body']);
  final reply = runtimeMap(row['reply']);
  final scores = runtimeMap(row['scores']);
  final call = runtimeMap(raw['call_thread']);
  final ended = runtimeMap(raw['call_thread_ended']);
  return {
    'id': row['id'], 'presentationId': row['id'], 'body': body,
    'author': row['own'] == true ? 'You' : row['sender'],
    'authorJid':
        row['author_id'] ??
        (row['own'] == true ? ownJid : raw['_author_stamp']),
    // Full occupant identity remains a fallback identity, never a claimed real JID.
    'senderId': group ? raw['from'] : row['author_id'],
    'mine': row['own'] == true,
    'timestamp': DateTime.fromMillisecondsSinceEpoch(
      (row['timestamp_ms'] as num? ?? 0).toInt(),
      isUtc: true,
    ).toIso8601String(),
    'threadId': row['thread'],
    'threadTargetId': row['thread'] ?? row['reference_id'] ?? row['id'],
    'replyTo': reply['target_id'],
    if (reply.isNotEmpty)
      'replyPreview': {
        'author': reply['sender'],
        'body': reply['target_retracted'] == true
            ? 'Message removed'
            : reply['snippet'],
        'deleted': reply['target_retracted'] == true,
      },
    'edited': row['edited'] == true,
    'deleted': deleted,
    'rejected': row['delivery'] == 'failed',
    'pending': false, 'pinned': pins.contains(row['reference_id']),
    'reactions': [
      for (final reaction in runtimeRows(row['reactions']))
        {
          'emoji': reaction['emoji'],
          'count': reaction['count'],
          'mine': reaction['own'] == true,
        },
    ],
    'files': deleted ? <Map<String, dynamic>>[] : _filePresentation(files),
    'markup': deleted || row['edited'] == true
        ? <Map<String, dynamic>>[]
        : _displayMarkup(raw, body.runes.length),
    'sticker': raw['is_sticker'] == true,
    'linkPreviews': deleted
        ? <Map<String, dynamic>>[]
        : [
            for (final preview in runtimeRows(raw['link_previews']))
              {
                'url': preview['original_url'],
                'title': preview['title'],
                'description': preview['description'],
              },
          ],
    'isCallRow': call.isNotEmpty || ended.isNotEmpty,
    if (call.isNotEmpty) 'call': call,
    if (ended.isNotEmpty) 'callEnded': ended,
    if (scores.isNotEmpty) 'safetyScores': _scorePresentation(scores),
  };
}

Map<String, dynamic> _projectPending(_Outbound row) {
  final options = row.options;
  final fallback = runtimeMap(options['fallback']);
  return {
    'id': 'pending:${row.id}',
    'presentationId': 'pending:${row.id}',
    'localId': row.id,
    'body': row.body,
    'mine': true,
    'author': 'You',
    'authorJid': row.owner,
    'timestamp': DateTime.fromMillisecondsSinceEpoch(
      row.timestamp,
      isUtc: true,
    ).toIso8601String(),
    'pending': true,
    'failed': row.failed,
    'queued': row.data['status'] == 'queued',
    'acked': row.acked,
    'rejected': row.data['rejected'] == true,
    'threadId': row.thread,
    'replyTo': runtimeMap(options['reply'])['message_id'],
    'files': _filePresentation(runtimeRows(options['shared_files'])),
    'reactions': <Map<String, dynamic>>[],
    'markup': _displayMarkup({
      'markup_spans': options['markup_spans'],
      'reply_fallback_start': fallback['start'],
      'reply_fallback_end': fallback['end'],
    }, row.body.runes.length),
  };
}

List<Map<String, dynamic>> _threadSummaries(List<Map<String, dynamic>> items) {
  final threads = <String, List<Map<String, dynamic>>>{};
  for (final item in items) {
    final thread = _optional(item['thread']);
    if (thread != null && !_identities(item).contains(thread)) {
      threads.putIfAbsent(thread, () => []).add(item);
    }
  }
  final result = <Map<String, dynamic>>[];
  for (final entry in threads.entries) {
    final root = items
        .where((row) => _identities(row).contains(entry.key))
        .firstOrNull;
    final latest = entry.value.last;
    result.add({
      'threadId': entry.key,
      'author': root?['sender'] ?? latest['sender'],
      'body': root?['retracted'] == true
          ? 'Message removed'
          : root?['body'] ?? latest['body'],
      'replyCount': entry.value.length,
      'timestamp': DateTime.fromMillisecondsSinceEpoch(
        (latest['timestamp_ms'] as num).toInt(),
        isUtc: true,
      ).toIso8601String(),
    });
  }
  result.sort(
    (one, two) =>
        _string(two['timestamp']).compareTo(_string(one['timestamp'])),
  );
  return result;
}

String? _previewUrl(String body) {
  final match = RegExp(r'https://[^\s<>]+').firstMatch(body);
  final uri = match == null ? null : Uri.tryParse(match.group(0)!);
  return uri != null && uri.host.contains('.') && uri.userInfo.isEmpty
      ? uri.toString()
      : null;
}
