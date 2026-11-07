part of 'conversation_runtime.dart';

typedef _RawKey = (bool, String, String);
typedef _TrustedRawKey = (bool, String, String, String);

class _IndexedRaw {
  _IndexedRaw(this.record, this.keys, this.trusted, this.sequence, this.group);
  final Map<String, dynamic> record;
  final Set<_RawKey> keys;
  final _TrustedRawKey? trusted;
  final int sequence;
  final bool group;
}

/// Index only original messages, scoped by conversation, actor and identity.
class _RawConversation {
  static const _limit = 1600;
  final _records = Queue<_IndexedRaw>();
  final _index = <_RawKey, Queue<_IndexedRaw>>{};
  final _trusted = <_TrustedRawKey, Queue<_IndexedRaw>>{};
  int _sequence = 0;

  Iterable<Map<String, dynamic>> get records =>
      _records.map((row) => row.record);
  bool? get group => _records.isEmpty ? null : _records.last.group;

  void add(
    Map<String, dynamic> record, {
    required String jid,
    required bool group,
    required String account,
    required String? ownNick,
  }) {
    final actor = _actor(record, jid, group, bareJid(account), ownNick);
    final original =
        actor != null &&
        record['replaces_id'] == null &&
        record['reaction_target_id'] == null &&
        record['retracts_id'] == null &&
        record['moderation_target_id'] == null &&
        record['is_retracted'] != true;
    final identities = original
        ? _rawIdentities(record, jid, group, bareJid(account))
        : (aliases: <String>{}, trusted: null);
    final keys = <_RawKey>{
      if (actor != null)
        for (final id in identities.aliases) (group, actor, id),
    };
    final trusted = identities.trusted;
    final trustedKey = actor == null || trusted == null
        ? null
        : (group, actor, trusted.$2, trusted.$1);
    final entry = _IndexedRaw(record, keys, trustedKey, ++_sequence, group);
    _records.addLast(entry);
    for (final key in keys) {
      _index.putIfAbsent(key, Queue.new).addLast(entry);
    }
    if (trustedKey != null) {
      _trusted.putIfAbsent(trustedKey, Queue.new).addLast(entry);
    }
    if (_records.length > _limit) {
      final expired = _records.removeFirst();
      for (final key in expired.keys) {
        final candidates = _index[key]!;
        assert(identical(candidates.first, expired));
        candidates.removeFirst();
        if (candidates.isEmpty) {
          _index.remove(key);
        }
      }
      final trustedKey = expired.trusted;
      if (trustedKey != null) {
        final candidates = _trusted[trustedKey]!;
        assert(identical(candidates.first, expired));
        candidates.removeFirst();
        if (candidates.isEmpty) {
          _trusted.remove(trustedKey);
        }
      }
    }
  }

  ({Set<String> aliases, (String, String)? trusted}) _rawIdentities(
    Map<String, dynamic> raw,
    String jid,
    bool group,
    String account,
  ) {
    final ids = <String>{?_optional(raw['id']), ?_optional(raw['origin_id'])};
    final mamId = raw['_archived'] == true ? _optional(raw['mam_id']) : null;
    if (mamId != null) {
      ids.add(mamId);
      return (aliases: ids, trusted: (mamId, group ? jid : account));
    }
    bool trusted(dynamic value) {
      if (value is! String || value.isEmpty || _resource(value).isNotEmpty) {
        return false;
      }
      final by = bareJid(value);
      return group
          ? by == jid
          : by == account || !by.contains('@') && by == account.split('@').last;
    }

    for (final assigned in runtimeRows(raw['stanza_ids'])) {
      final id = _optional(assigned['id']);
      if (id != null && trusted(assigned['by'])) {
        ids.add(id);
        return (aliases: ids, trusted: (id, bareJid(assigned['by'] as String)));
      }
    }
    final id = _optional(raw['stanza_id']);
    if (id != null && trusted(raw['stanza_id_by'])) {
      ids.add(id);
      return (
        aliases: ids,
        trusted: (id, bareJid(raw['stanza_id_by'] as String)),
      );
    }
    return (aliases: ids, trusted: null);
  }

  Map<String, dynamic>? lookup(
    Map<String, dynamic> row, {
    required bool group,
    required String jid,
    required String account,
    bool preferArchived = false,
  }) {
    final author = _optional(row['author_id']);
    final actor = group
        ? author == null
              ? 'occupant:${_string(row['sender'])}'
              : 'author:${bareJid(author)}'
        : 'author:${bareJid(author ?? (row['own'] == true ? account : jid))}';
    final archiveId =
        _optional(row['archive_id']) ??
        (group ? _optional(row['reference_id']) : null);
    final archiveBy = _optional(row['archive_by']) ?? (group ? jid : null);
    if (archiveId != null && archiveBy != null) {
      final candidates =
          _trusted[(group, actor, bareJid(archiveBy), archiveId)];
      if (preferArchived && candidates != null) {
        for (final candidate in candidates.toList().reversed) {
          if (candidate.record['_archived'] == true) return candidate.record;
        }
      }
      return candidates?.last.record;
    }
    _IndexedRaw? newest;
    for (final id in _identities(row)) {
      final candidates = _index[(group, actor, id)];
      if (candidates == null) {
        continue;
      }
      for (final candidate in candidates.toList().reversed) {
        if (candidate.trusted == null) {
          if (newest == null || candidate.sequence > newest.sequence) {
            newest = candidate;
          }
          break;
        }
      }
    }
    return newest?.record;
  }

  String? _actor(
    Map<String, dynamic> raw,
    String jid,
    bool group,
    String account,
    String? ownNick,
  ) {
    final from = _string(raw['from']);
    if (group) {
      final nick = _resource(from);
      if (bareJid(from) != jid ||
          nick.isEmpty ||
          raw['message_type'] != 'groupchat') {
        return null;
      }
      final archived = raw['_archived'] == true;
      final attributed = _optional(
        archived ? raw['author_real_jid'] : raw['_author_stamp'],
      );
      final author = attributed == null && !archived && nick == ownNick
          ? account
          : attributed == null
          ? null
          : bareJid(attributed);
      return author == null ? 'occupant:$nick' : 'author:$author';
    }
    if (raw['message_type'] != 'chat' ||
        raw['muc_user'] == true ||
        raw['muc_pm'] == true ||
        raw['is_muc'] == true) {
      return null;
    }
    final sender = bareJid(from);
    final carbon = raw['carbon'];
    final sent =
        carbon == 'sent' ||
        carbon == 'Sent' ||
        carbon is Map && carbon['sent'] == true && carbon['received'] == false;
    final own = sender == account || sent;
    final peer = own ? bareJid(_string(raw['to'])) : sender;
    if (peer != jid || peer == account) {
      return null;
    }
    return 'author:${own ? account : peer}';
  }
}
