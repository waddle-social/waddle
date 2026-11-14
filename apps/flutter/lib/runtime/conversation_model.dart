part of 'conversation_runtime.dart';

class _View {
  _View(
    this.id,
    this.jid,
    this.group,
    this.thread,
    this.generation,
    this.atNewest,
  );
  final String id;
  final String jid;
  final bool group;
  final String? thread;
  final int generation;
  bool visible = true;
  bool atNewest;
  bool loading = true;
  bool paging = false;
  Object? historyRequest;
  Object? initialiseRequest;
  bool reachedStart = false;
  int pages = 0;
  String? cursor;
  String upload = 'idle';
  String? unreadAnchor;
  Map<String, dynamic>? reply;
  Map<String, dynamic>? edit;
  int editAttempt = 0;
  Timer? typingTimer;
  bool composing = false;
}

class _Outbound {
  _Outbound(this.data);
  final Map<String, dynamic> data;
  String get id => data['origin_id'] as String;
  String get jid => data['jid'] as String;
  String get owner => data['owner'] as String;
  bool get group => data['group'] == true;
  bool get acked => data['acked'] == true;
  bool get failed => data['failed'] == true;
  String get body => data['body'] as String;
  Map<String, dynamic> get options => runtimeMap(data['options']);
  int get timestamp => (data['timestamp_ms'] as num).toInt();
  String? get thread => runtimeMap(options['thread'])['id'] as String?;
}

class _RuntimeLease {
  _RuntimeLease(this.context)
    : generation = context.generation,
      owner = context.jid,
      transport = context.transport;
  final RuntimeContext context;
  final int generation;
  final String? owner;
  final Object? transport;
  bool get current => context.generation == generation && context.jid == owner;
  bool get currentConnection =>
      current && identical(transport, context.transport);
  void check({bool connection = false}) {
    if (!current) {
      throw const AccountChanged();
    }
    if (connection && !identical(transport, context.transport)) {
      throw StateError('The connection changed. Please try again.');
    }
  }
}

Set<String> _identities(Map<String, dynamic> row) => {
  for (final field in [
    'id',
    'origin_id',
    'message_id',
    'reference_id',
    'archive_id',
  ])
    if (row[field] is String && (row[field] as String).isNotEmpty)
      row[field] as String,
  for (final value in (row['identity_ids'] as List? ?? const []))
    if (value is String) value,
  for (final value in runtimeRows(row['stanza_ids']))
    if (value['id'] is String) value['id'] as String,
  if (row['stanza_id'] is String) row['stanza_id'] as String,
};

String _string(dynamic value, [String fallback = '']) =>
    value is String ? value : fallback;
String? _optional(dynamic value) =>
    value is String && value.isNotEmpty ? value : null;
String _resource(String jid) =>
    jid.contains('/') ? jid.substring(jid.indexOf('/') + 1) : '';
String _localName(String jid) => bareJid(jid).split('@').first;

String _newOriginId() {
  final bytes = List<int>.generate(16, (_) => Random.secure().nextInt(256));
  bytes[6] = (bytes[6] & 15) | 64;
  bytes[8] = (bytes[8] & 63) | 128;
  final hex = bytes
      .map((byte) => byte.toRadixString(16).padLeft(2, '0'))
      .join();
  return '${hex.substring(0, 8)}-${hex.substring(8, 12)}-${hex.substring(12, 16)}-${hex.substring(16, 20)}-${hex.substring(20)}';
}

Map<String, dynamic> _sendOptions(String id) => {
  'stanza_id': id,
  'subject': null,
  'reply': null,
  'fallback': null,
  'thread': null,
  'markup_spans': <Map<String, dynamic>>[],
  'references': <Map<String, dynamic>>[],
  'shared_files': <Map<String, dynamic>>[],
  'link_preview_token': null,
  'request_displayed_marker': true,
  'muc_pm': false,
  'sticker': null,
  'mention_directory': null,
};

bool _rejectionOwns(_Outbound message, Map<String, dynamic> rejection) {
  if (_string(rejection['stanza_id']) != message.id) {
    return false;
  }
  final to = _optional(rejection['to']);
  if (to != null && bareJid(to) != bareJid(message.owner)) {
    return false;
  }
  final from = _string(rejection['from']);
  if (from.isEmpty) {
    return false;
  }
  final sender = bareJid(from);
  final recipient = bareJid(message.jid);
  final resource = _resource(from);
  if (!sender.contains('@') &&
      resource.isEmpty &&
      (sender == bareJid(message.owner).split('@').last ||
          sender == recipient.split('@').last)) {
    return true;
  }
  if (sender != recipient) {
    return false;
  }
  if (message.group) {
    return resource.isEmpty;
  }
  final targetResource = _resource(message.jid);
  return targetResource.isEmpty ||
      resource.isEmpty ||
      resource == targetResource;
}

class _Composed {
  _Composed(this.body, this.markup, this.references);
  final String body;
  final List<Map<String, dynamic>> markup;
  final List<Map<String, dynamic>> references;
}

/// Parse outgoing formatting only. Incoming bodies are never Markdown parsed.
_Composed _compose(String input, List<Map<String, dynamic>> mentions) {
  final source = input.runes.toList();
  final output = <int>[];
  final offsets = List<int>.filled(source.length + 1, 0);
  final spans = <Map<String, dynamic>>[];
  const marks = {
    '**': 'bold',
    '~~': 'strikethrough',
    '`': 'code',
    '*': 'italic',
  };
  var index = 0;
  while (index < source.length) {
    offsets[index] = output.length;
    String? matched;
    int? ending;
    for (final mark in marks.keys) {
      final token = mark.runes.toList();
      if (index + token.length > source.length ||
          String.fromCharCodes(source.sublist(index, index + token.length)) !=
              mark) {
        continue;
      }
      for (
        var end = index + token.length + 1;
        end + token.length <= source.length;
        end++
      ) {
        if (String.fromCharCodes(source.sublist(end, end + token.length)) ==
            mark) {
          matched = mark;
          ending = end;
          break;
        }
      }
      if (matched != null) {
        break;
      }
    }
    if (matched == null || ending == null) {
      output.add(source[index]);
      index++;
      continue;
    }
    final width = matched.runes.length;
    final start = output.length;
    for (var cursor = index; cursor < index + width; cursor++) {
      offsets[cursor] = output.length;
    }
    for (var cursor = index + width; cursor < ending; cursor++) {
      offsets[cursor] = output.length;
      output.add(source[cursor]);
    }
    spans.add({
      'span_type': marks[matched],
      'start': start,
      'end': output.length,
      'uri': null,
    });
    for (var cursor = ending; cursor < ending + width; cursor++) {
      offsets[cursor] = output.length;
    }
    index = ending + width;
  }
  offsets[source.length] = output.length;
  final references = <Map<String, dynamic>>[];
  for (final mention in mentions) {
    final start = mention['begin'];
    final end = mention['end'];
    final uri = _optional(mention['uri']);
    if (start is! int ||
        end is! int ||
        start < 0 ||
        end <= start ||
        end > source.length ||
        uri == null ||
        !uri.startsWith('xmpp:')) {
      throw ArgumentError('Invalid mention range or identity');
    }
    references.add({
      'ref_type': {'type': 'mention'},
      'uri': uri,
      'begin': offsets[start],
      'end': offsets[end],
      'anchor': null,
    });
  }
  return _Composed(String.fromCharCodes(output), spans, references);
}
