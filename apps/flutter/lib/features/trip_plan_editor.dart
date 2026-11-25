part of 'trip_plan.dart';

class _PlanEditor extends StatefulWidget {
  const _PlanEditor({required this.kind, this.item});
  final String kind;
  final _Item? item;

  @override
  State<_PlanEditor> createState() => _PlanEditorState();
}

class _PlanEditorState extends State<_PlanEditor> {
  late final _Item _draft;
  final _form = GlobalKey<FormState>();
  static const labels = {
    'title': 'Title',
    'description': 'Notes',
    'location': 'Location',
    'category': 'Category',
    'timezone': 'Time zone',
    'starts_at': 'Starts',
    'ends_at': 'Ends',
    'url': 'Link address',
    'assignee': 'Assigned to',
    'participants': 'Participants',
    'amount_minor': 'Amount in minor units',
    'currency': 'Currency',
    'choices': 'Poll choices',
  };

  @override
  void initState() {
    super.initState();
    _draft = Map.of(
      widget.item ??
          {
            'kind': widget.kind,
            'id': widget.kind == 'event' ? 'event' : _newId(),
          },
    );
  }

  String? _validate(String key, String? text) {
    final value = text?.trim() ?? '';
    if (value.isEmpty &&
        {'title', 'url', 'choices', 'currency', 'amount_minor'}.contains(key)) {
      return 'Enter ${labels[key]!.toLowerCase()}.';
    }
    if (key == 'amount_minor' && (int.tryParse(value) ?? -1) < 0) {
      return 'Enter a whole amount of zero or more.';
    }
    if (key == 'currency' && !RegExp(r'^[A-Za-z]{3}$').hasMatch(value)) {
      return 'Use a three-letter currency code.';
    }
    if ((key == 'starts_at' || key == 'ends_at') &&
        value.isNotEmpty &&
        DateTime.tryParse(value) == null) {
      return 'Enter a date and time.';
    }
    if (key == 'choices' &&
        value
                .split('\n')
                .map((line) => line.trim())
                .where((line) => line.isNotEmpty)
                .toSet()
                .length <
            2) {
      return 'Enter at least two choices on separate lines.';
    }
    if (key == 'url') {
      final url = Uri.tryParse(value);
      if (url == null ||
          !{'http', 'https'}.contains(url.scheme) ||
          url.host.isEmpty ||
          url.userInfo.isNotEmpty) {
        return 'Enter a web address.';
      }
    }
    return null;
  }

  Widget _field(String key) {
    final value = _draft[key];
    final options = (_draft['options'] as List? ?? const []).whereType<Map>();
    final initial = key == 'choices'
        ? options.map((opt) => opt['label']).join('\n')
        : value is List
        ? value.join(', ')
        : value?.toString();
    return TextFormField(
      initialValue: initial,
      maxLines: key == 'choices' || key == 'description' ? 3 : 1,
      decoration: InputDecoration(
        labelText: labels[key],
        hintText: switch (key) {
          'starts_at' || 'ends_at' => '2027-06-01 10:00',
          'timezone' => 'Europe/London',
          'participants' => 'JIDs separated by commas',
          'choices' => 'One choice per line',
          _ => null,
        },
      ),
      validator: (text) => _validate(key, text),
      onSaved: (raw) {
        final text = raw?.trim() ?? '';
        if (key == 'choices') {
          final oldIds = {
            for (final option in options) option['label']: option['id'],
          };
          _draft['options'] = [
            for (final label
                in text
                    .split('\n')
                    .map((line) => line.trim())
                    .where((line) => line.isNotEmpty)
                    .toSet())
              {'label': label, 'id': oldIds[label] ?? _newId()},
          ];
        } else {
          _draft[key] = switch (key) {
            'participants' =>
              text
                  .split(',')
                  .map((jid) => jid.trim())
                  .where((jid) => jid.isNotEmpty)
                  .toList(),
            'amount_minor' => int.tryParse(text),
            'starts_at' || 'ends_at' =>
              text.isEmpty
                  ? null
                  : DateTime.parse(text).toUtc().toIso8601String(),
            'currency' => text.toUpperCase(),
            _ => text.isEmpty ? null : text,
          };
        }
      },
    );
  }

  @override
  Widget build(BuildContext context) => AlertDialog(
    title: Text(_kinds[widget.kind]!),
    content: SingleChildScrollView(
      child: Form(
        key: _form,
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            for (final key in [
              'title',
              'description',
              'category',
              if (widget.kind == 'link') 'url',
              if (widget.kind == 'poll') 'choices',
              if (widget.kind == 'task') 'assignee',
              if (widget.kind == 'expense') ...[
                'amount_minor',
                'currency',
                'participants',
              ],
              if ({'event', 'travel', 'itinerary'}.contains(widget.kind)) ...[
                'location',
                'starts_at',
                'ends_at',
                'timezone',
              ],
            ])
              _field(key),
            if ({
              'event',
              'decision',
              'itinerary',
              'travel',
            }.contains(widget.kind))
              DropdownButtonFormField<String>(
                initialValue: _draft['status'] as String?,
                decoration: const InputDecoration(labelText: 'Status'),
                items: [
                  for (final status in ['proposed', 'confirmed', 'cancelled'])
                    DropdownMenuItem(value: status, child: Text(status)),
                ],
                onChanged: (value) => _draft['status'] = value,
              ),
          ],
        ),
      ),
    ),
    actions: [
      TextButton(
        onPressed: () => Navigator.pop(context),
        child: const Text('Cancel'),
      ),
      FilledButton(
        onPressed: () {
          if (_form.currentState!.validate()) {
            _form.currentState!.save();
            Navigator.pop(context, _draft);
          }
        },
        child: const Text('Save'),
      ),
    ],
  );
}
