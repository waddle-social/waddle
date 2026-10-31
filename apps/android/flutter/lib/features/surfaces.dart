import 'dart:async';
import 'dart:typed_data';

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../bridge.dart';
import 'surfaces_vocabulary.dart';

Map<String, dynamic> _map(dynamic value) => value is Map
    ? value.map((key, value) => MapEntry(key.toString(), value))
    : <String, dynamic>{};
List<Map<String, dynamic>> _rows(dynamic value) => value is List
    ? value.whereType<Map>().map(_map).toList()
    : <Map<String, dynamic>>[];
String _text(dynamic value, [String fallback = '']) =>
    value?.toString() ?? fallback;

Future<dynamic> _command(
  BuildContext context,
  WaddleBridge bridge,
  String method, [
  Map<String, dynamic> args = const {},
]) async {
  try {
    return await bridge.invoke(method, args);
  } catch (error) {
    if (context.mounted) {
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(
          content: Text(
            error is PlatformException
                ? error.message ?? 'The action could not be completed.'
                : error.toString(),
          ),
        ),
      );
    }
    return null;
  }
}

class _Section extends StatelessWidget {
  const _Section(this.title, this.children);
  final String title;
  final List<Widget> children;
  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.only(top: 24),
    child: Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Padding(
          padding: const EdgeInsets.fromLTRB(20, 0, 20, 12),
          child: Semantics(
            header: true,
            child: Text(
              title,
              style: Theme.of(context).textTheme.titleSmall?.copyWith(
                color: Theme.of(context).colorScheme.primary,
              ),
            ),
          ),
        ),
        Card.filled(
          margin: const EdgeInsets.symmetric(horizontal: 12),
          child: Column(children: children),
        ),
      ],
    ),
  );
}

class _Empty extends StatelessWidget {
  const _Empty(this.icon, this.title, this.detail);
  final IconData icon;
  final String title;
  final String detail;
  @override
  Widget build(BuildContext context) => Center(
    child: Padding(
      padding: const EdgeInsets.all(32),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(icon, size: 56, color: Theme.of(context).colorScheme.primary),
          const SizedBox(height: 20),
          Text(
            title,
            style: Theme.of(context).textTheme.titleLarge,
            textAlign: TextAlign.center,
          ),
          const SizedBox(height: 8),
          Text(
            detail,
            textAlign: TextAlign.center,
            style: Theme.of(context).textTheme.bodyMedium?.copyWith(
              color: Theme.of(context).colorScheme.onSurfaceVariant,
            ),
          ),
        ],
      ),
    ),
  );
}

class PersonAvatar extends StatelessWidget {
  const PersonAvatar({
    super.key,
    required this.name,
    this.radius = 22,
    this.bytes,
  });
  final String name;
  final double radius;
  final Uint8List? bytes;
  @override
  Widget build(BuildContext context) => CircleAvatar(
    radius: radius,
    backgroundColor: Theme.of(context).colorScheme.secondaryContainer,
    foregroundColor: Theme.of(context).colorScheme.onSecondaryContainer,
    foregroundImage: bytes == null ? null : MemoryImage(bytes!),
    onForegroundImageError: bytes == null ? null : (error, stackTrace) {},
    child: Text(
      name.trim().isEmpty ? '?' : name.trim().characters.first.toUpperCase(),
    ),
  );
}

class SettingsScreen extends StatelessWidget {
  const SettingsScreen({super.key, required this.bridge});
  final WaddleBridge bridge;
  @override
  Widget build(BuildContext context) => AnimatedBuilder(
    animation: bridge,
    builder: (context, _) {
      final prefs = bridge.preferences;
      final delivery = {...prefs, ..._map(prefs['delivery'])};
      void set(String key, dynamic value) {
        _command(context, bridge, 'preferences.set', {
          'key': key,
          'value': value,
        });
      }

      Widget toggle(
        String title,
        String detail,
        String key, {
        bool fallback = true,
      }) => SwitchListTile(
        title: Text(title),
        subtitle: Text(detail),
        value: prefs[key] as bool? ?? fallback,
        onChanged: (value) => set(key, value),
      );
      return Scaffold(
        appBar: AppBar(title: const Text('Settings')),
        body: ListView(
          padding: const EdgeInsets.only(bottom: 32),
          children: [
            _Section('Appearance', [
              ListTile(
                leading: const Icon(Icons.palette_outlined),
                title: const Text('Theme'),
                trailing: DropdownButtonHideUnderline(
                  child: DropdownButton<String>(
                    value: _text(prefs['theme'], 'system'),
                    items: const [
                      DropdownMenuItem(value: 'system', child: Text('System')),
                      DropdownMenuItem(value: 'light', child: Text('Light')),
                      DropdownMenuItem(value: 'dark', child: Text('Dark')),
                    ],
                    onChanged: (value) {
                      if (value != null) {
                        set('theme', value);
                      }
                    },
                  ),
                ),
              ),
              toggle(
                'Wallpaper colors',
                'Use your device’s Material You palette.',
                'wallpaperColors',
                fallback: false,
              ),
              ListTile(
                leading: const Icon(Icons.swap_vert_rounded),
                title: const Text('Timeline'),
                subtitle: const Text('Choose where new messages appear.'),
                trailing: DropdownButtonHideUnderline(
                  child: DropdownButton<String>(
                    value: _text(prefs['timeline'], 'chat'),
                    items: const [
                      DropdownMenuItem(value: 'chat', child: Text('Chat')),
                      DropdownMenuItem(value: 'social', child: Text('Social')),
                    ],
                    onChanged: (value) {
                      if (value != null) {
                        set('timeline', value);
                      }
                    },
                  ),
                ),
              ),
            ]),
            _Section('Notifications & privacy', [
              toggle(
                'Notifications',
                'Notify me about new messages.',
                'notificationsEnabled',
              ),
              toggle(
                'Message sounds',
                'Play sounds for new messages.',
                'messageSoundsEnabled',
              ),
              toggle(
                'Read receipts',
                'Let people know when you read a message.',
                'readReceiptsEnabled',
              ),
              ListTile(
                leading: const Icon(Icons.notifications_active_outlined),
                title: const Text('Android notification settings'),
                trailing: const Icon(Icons.open_in_new_rounded),
                onTap: () => _command(context, bridge, 'push.settings'),
              ),
            ]),
            _Section('Background delivery', [
              ListTile(
                leading: const Icon(Icons.cloud_done_outlined),
                title: Text(
                  _text(delivery['distributorLabel'], 'Push delivery'),
                ),
                subtitle: Text(
                  _text(
                    delivery['status'] ?? delivery['pushStatus'],
                    'Checking delivery status',
                  ),
                ),
                trailing: const Icon(Icons.chevron_right_rounded),
                onTap: () async {
                  if (delivery['fcmAvailable'] != true) {
                    await _command(context, bridge, 'push.choose');
                    return;
                  }
                  final method = await showModalBottomSheet<String>(
                    context: context,
                    showDragHandle: true,
                    builder: (context) => SafeArea(
                      child: Column(
                        mainAxisSize: MainAxisSize.min,
                        children: [
                          ListTile(
                            leading: const Icon(Icons.notifications_outlined),
                            title: const Text('Google Play push'),
                            onTap: () => Navigator.pop(context, 'push.fcm'),
                          ),
                          ListTile(
                            leading: const Icon(Icons.cloud_outlined),
                            title: const Text('UnifiedPush distributor'),
                            onTap: () => Navigator.pop(context, 'push.choose'),
                          ),
                        ],
                      ),
                    ),
                  );
                  if (method != null && context.mounted) {
                    await _command(context, bridge, method);
                  }
                },
              ),
              if (delivery['stayConnectedSupported'] == true)
                SwitchListTile(
                  title: const Text('Stay connected'),
                  subtitle: const Text(
                    'Keep a background connection for immediate delivery.',
                  ),
                  value: delivery['stayConnected'] == true,
                  onChanged: (value) => set('stayConnected', value),
                ),
            ]),
            _Section('Account', [
              ListTile(
                leading: const Icon(Icons.person_outline_rounded),
                title: Text(_text(bridge.session['username'], 'Your profile')),
                subtitle: Text(_text(bridge.session['jid'])),
                trailing: const Icon(Icons.chevron_right_rounded),
                onTap: () =>
                    Navigator.of(context).restorablePushNamed('profile'),
              ),
              if (prefs['isCommunityOwner'] == true ||
                  bridge.home['isCommunityOwner'] == true)
                ListTile(
                  leading: const Icon(Icons.admin_panel_settings_outlined),
                  title: const Text('Community users'),
                  trailing: const Icon(Icons.chevron_right_rounded),
                  onTap: () =>
                      Navigator.of(context).restorablePushNamed('admin'),
                ),
              ListTile(
                leading: Icon(
                  Icons.logout_rounded,
                  color: Theme.of(context).colorScheme.error,
                ),
                title: const Text('Sign out'),
                onTap: () async {
                  final confirmed = await showDialog<bool>(
                    context: context,
                    builder: (context) => AlertDialog(
                      title: const Text('Sign out?'),
                      content: const Text(
                        'You can sign in again to return to your conversations.',
                      ),
                      actions: [
                        TextButton(
                          onPressed: () => Navigator.pop(context, false),
                          child: const Text('Cancel'),
                        ),
                        FilledButton(
                          onPressed: () => Navigator.pop(context, true),
                          child: const Text('Sign out'),
                        ),
                      ],
                    ),
                  );
                  if (confirmed == true && context.mounted) {
                    await _command(context, bridge, 'session.logout');
                    if (context.mounted) {
                      Navigator.of(context).popUntil((route) => route.isFirst);
                    }
                  }
                },
              ),
            ]),
          ],
        ),
      );
    },
  );
}

class ProfileScreen extends StatefulWidget {
  const ProfileScreen({super.key, required this.bridge, this.jid});
  final WaddleBridge bridge;
  final String? jid;
  @override
  State<ProfileScreen> createState() => _ProfileScreenState();
}

class _ProfileScreenState extends State<ProfileScreen> {
  final _form = GlobalKey<FormState>();
  final _fields = <String, TextEditingController>{
    for (final key in ['fullName', 'nickname', 'pronouns', 'note', 'url'])
      key: TextEditingController(),
  };
  bool _loading = true;
  bool _loaded = false;
  bool _saving = false;
  String? _error;
  Map<String, dynamic> _profile = {};
  bool get _self =>
      widget.jid == null || widget.jid == widget.bridge.session['jid'];
  @override
  void initState() {
    super.initState();
    _load();
  }

  @override
  void dispose() {
    for (final field in _fields.values) {
      field.dispose();
    }
    super.dispose();
  }

  Future<void> _load() async {
    setState(() {
      _loading = true;
      _error = null;
    });
    try {
      final result = await widget.bridge.invoke('profile.load', {
        if (widget.jid != null) 'jid': widget.jid,
      });
      if (!mounted) {
        return;
      }
      _profile = _map(result);
      if (_profile.isEmpty) {
        _profile = widget.bridge.topic('profile');
      }
      final vcard = {..._profile, ..._map(_profile['vcard'])};
      for (final entry in _fields.entries) {
        entry.value.text = _text(vcard[entry.key]);
      }
      setState(() {
        _loading = false;
        _loaded = true;
      });
    } catch (error) {
      if (mounted) {
        setState(() {
          _loading = false;
          _error = error.toString();
        });
      }
    }
  }

  Future<void> _save() async {
    if (!_form.currentState!.validate() || _saving || !_loaded) {
      return;
    }
    setState(() => _saving = true);
    try {
      await widget.bridge.invoke('profile.save', {
        for (final field in _fields.entries) field.key: field.value.text.trim(),
      });
      for (final field in _fields.entries) {
        _profile[field.key] = field.value.text.trim();
      }
      if (mounted) {
        ScaffoldMessenger.of(
          context,
        ).showSnackBar(const SnackBar(content: Text('Profile updated')));
      }
    } catch (error) {
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(
          SnackBar(content: Text('Could not save your profile: $error')),
        );
      }
    } finally {
      if (mounted) {
        setState(() => _saving = false);
      }
    }
  }

  @override
  Widget build(BuildContext context) => Scaffold(
    appBar: AppBar(
      title: Text(_self ? 'Your profile' : 'Profile'),
      actions: [
        if (_self)
          IconButton(
            tooltip: 'Revert changes',
            icon: const Icon(Icons.undo_rounded),
            onPressed: _loaded && !_saving
                ? () {
                    for (final entry in _fields.entries) {
                      entry.value.text = _text(_profile[entry.key]);
                    }
                  }
                : null,
          ),
        if (_self)
          TextButton(
            onPressed: _loaded && !_saving ? _save : null,
            child: _saving
                ? const SizedBox.square(
                    dimension: 20,
                    child: CircularProgressIndicator(strokeWidth: 2),
                  )
                : const Text('Save'),
          ),
      ],
    ),
    body: _loading
        ? const Center(child: CircularProgressIndicator())
        : _error != null
        ? Center(
            child: Column(
              mainAxisSize: MainAxisSize.min,
              children: [
                const Text('Could not load this profile.'),
                const SizedBox(height: 12),
                FilledButton.tonal(
                  onPressed: _load,
                  child: const Text('Try again'),
                ),
              ],
            ),
          )
        : Form(
            key: _form,
            child: ListView(
              padding: const EdgeInsets.fromLTRB(20, 16, 20, 40),
              children: [
                Center(
                  child: AnimatedBuilder(
                    animation: widget.bridge,
                    builder: (context, _) {
                      final bytes = (_self
                          ? widget.bridge.topic('profile')['avatar']
                          : _profile['avatar']);
                      return PersonAvatar(
                        name: _fields['fullName']!.text.isNotEmpty
                            ? _fields['fullName']!.text
                            : _text(
                                widget.jid ?? widget.bridge.session['username'],
                                'You',
                              ),
                        radius: 48,
                        bytes: bytes is Uint8List
                            ? bytes
                            : bytes is List
                            ? Uint8List.fromList(bytes.cast<int>())
                            : null,
                      );
                    },
                  ),
                ),
                const SizedBox(height: 12),
                Center(
                  child: SelectableText(
                    _text(widget.jid ?? widget.bridge.session['jid']),
                    style: Theme.of(context).textTheme.bodyMedium,
                  ),
                ),
                if (_self)
                  Wrap(
                    alignment: WrapAlignment.center,
                    children: [
                      TextButton.icon(
                        onPressed: () => _command(
                          context,
                          widget.bridge,
                          'profile.avatar',
                          {'remove': false},
                        ),
                        icon: const Icon(Icons.add_a_photo_outlined),
                        label: const Text('Change photo'),
                      ),
                      TextButton(
                        onPressed: () => _command(
                          context,
                          widget.bridge,
                          'profile.avatar',
                          {'remove': true},
                        ),
                        child: const Text('Remove'),
                      ),
                    ],
                  ),
                const SizedBox(height: 24),
                for (final spec in const [
                  ('fullName', 'Full name', 160),
                  ('nickname', 'Nickname', 80),
                  ('pronouns', 'Pronouns', 48),
                  ('note', 'About you', 500),
                  ('url', 'Website', 240),
                ])
                  Padding(
                    padding: const EdgeInsets.only(bottom: 16),
                    child: TextFormField(
                      controller: _fields[spec.$1],
                      readOnly: !_self,
                      maxLength: spec.$3,
                      maxLines: spec.$1 == 'note' ? 3 : 1,
                      keyboardType: spec.$1 == 'url'
                          ? TextInputType.url
                          : TextInputType.text,
                      decoration: InputDecoration(
                        labelText: spec.$2,
                        border: const OutlineInputBorder(),
                        counterText: '',
                      ),
                      validator: (value) {
                        if (spec.$1 != 'url' ||
                            value == null ||
                            value.trim().isEmpty) {
                          return null;
                        }
                        final url = Uri.tryParse(value.trim());
                        return url != null &&
                                url.hasAuthority &&
                                ['https', 'http'].contains(url.scheme)
                            ? null
                            : 'Enter a complete website address.';
                      },
                    ),
                  ),
                if (_self) ...[
                  const Divider(height: 32),
                  Text(
                    'Share your status',
                    style: Theme.of(context).textTheme.titleLarge,
                  ),
                  const SizedBox(height: 12),
                  _StatusEditor(
                    bridge: widget.bridge,
                    kind: 'mood',
                    title: 'Mood',
                    icon: Icons.mood_outlined,
                    values: _map(_profile['mood']),
                    fields: const {'kind': 'Mood', 'text': 'Status message'},
                  ),
                  _StatusEditor(
                    bridge: widget.bridge,
                    kind: 'activity',
                    title: 'Activity',
                    icon: Icons.directions_walk_outlined,
                    values: _map(_profile['activity']),
                    fields: const {
                      'general': 'Activity',
                      'specific': 'Specific activity',
                      'text': 'Status message',
                    },
                  ),
                  _StatusEditor(
                    bridge: widget.bridge,
                    kind: 'tune',
                    title: 'Listening to',
                    icon: Icons.music_note_outlined,
                    values: _map(_profile['tune']),
                    fields: const {
                      'artist': 'Artist',
                      'title': 'Title',
                      'source': 'Album',
                      'track': 'Track',
                      'lengthSeconds': 'Length (seconds)',
                      'rating': 'Rating (1–10)',
                      'uri': 'Link',
                    },
                  ),
                ],
              ],
            ),
          ),
  );
}

class _StatusEditor extends StatefulWidget {
  const _StatusEditor({
    required this.bridge,
    required this.kind,
    required this.title,
    required this.icon,
    required this.fields,
    required this.values,
  });
  final WaddleBridge bridge;
  final String kind, title;
  final IconData icon;
  final Map<String, String> fields;
  final Map<String, dynamic> values;
  @override
  State<_StatusEditor> createState() => _StatusEditorState();
}

class _StatusEditorState extends State<_StatusEditor> {
  late final _controllers = {
    for (final key in widget.fields.keys)
      key: TextEditingController(text: _text(widget.values[key])),
  };
  bool _busy = false;
  @override
  void dispose() {
    for (final controller in _controllers.values) {
      controller.dispose();
    }
    super.dispose();
  }

  Future<void> _submit(bool clear) async {
    final args = <String, dynamic>{'clear': clear};
    if (!clear) {
      for (final entry in _controllers.entries) {
        final text = entry.value.text.trim();
        if (['lengthSeconds', 'rating'].contains(entry.key)) {
          final number = int.tryParse(text);
          if (text.isNotEmpty &&
              (number == null ||
                  number < 1 ||
                  number > 2147483647 ||
                  (entry.key == 'rating' && (number < 1 || number > 10)))) {
            ScaffoldMessenger.of(context).showSnackBar(
              SnackBar(
                content: Text(
                  entry.key == 'rating'
                      ? 'Choose a rating from 1 to 10.'
                      : 'Enter a valid length.',
                ),
              ),
            );
            return;
          }
          args[entry.key] = number;
        } else {
          args[entry.key] = text;
        }
      }
      if (widget.kind == 'mood' && _text(args['kind']).isEmpty ||
          widget.kind == 'activity' && _text(args['general']).isEmpty ||
          widget.kind == 'tune' &&
              _controllers.values.every((field) => field.text.trim().isEmpty)) {
        ScaffoldMessenger.of(context).showSnackBar(
          const SnackBar(content: Text('Add a status before publishing.')),
        );
        return;
      }
    }
    setState(() => _busy = true);
    try {
      await widget.bridge.invoke('profile.${widget.kind}', args);
      if (!mounted) {
        return;
      }
      if (clear) {
        for (final controller in _controllers.values) {
          controller.clear();
        }
      }
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text(clear ? 'Status cleared' : 'Status published')),
      );
    } catch (error) {
      if (mounted) {
        ScaffoldMessenger.of(
          context,
        ).showSnackBar(SnackBar(content: Text(error.toString())));
      }
    } finally {
      if (mounted) {
        setState(() => _busy = false);
      }
    }
  }

  @override
  Widget build(BuildContext context) => Card.filled(
    margin: const EdgeInsets.only(bottom: 12),
    child: ExpansionTile(
      leading: Icon(widget.icon),
      title: Text(widget.title),
      childrenPadding: const EdgeInsets.fromLTRB(16, 0, 16, 16),
      children: [
        for (final entry in widget.fields.entries)
          Padding(
            padding: const EdgeInsets.only(bottom: 12),
            child: entry.key == 'kind' || entry.key == 'general'
                ? DropdownButtonFormField<String>(
                    initialValue: _controllers[entry.key]!.text.isEmpty
                        ? null
                        : _controllers[entry.key]!.text,
                    isExpanded: true,
                    menuMaxHeight: 360,
                    decoration: InputDecoration(
                      labelText: entry.value,
                      border: const OutlineInputBorder(),
                    ),
                    items: (entry.key == 'kind' ? moodKinds : generalActivities)
                        .map(
                          (value) => DropdownMenuItem(
                            value: value,
                            child: Text(
                              value
                                  .split('_')
                                  .map(
                                    (word) =>
                                        '${word[0].toUpperCase()}${word.substring(1)}',
                                  )
                                  .join(' '),
                            ),
                          ),
                        )
                        .toList(),
                    onChanged: _busy
                        ? null
                        : (value) => setState(
                            () => _controllers[entry.key]!.text = value ?? '',
                          ),
                  )
                : TextField(
                    controller: _controllers[entry.key],
                    keyboardType:
                        ['lengthSeconds', 'rating'].contains(entry.key)
                        ? TextInputType.number
                        : TextInputType.text,
                    decoration: InputDecoration(
                      labelText: entry.value,
                      border: const OutlineInputBorder(),
                    ),
                  ),
          ),
        Row(
          mainAxisAlignment: MainAxisAlignment.end,
          children: [
            TextButton(
              onPressed: _busy ? null : () => _submit(true),
              child: const Text('Clear'),
            ),
            const SizedBox(width: 8),
            FilledButton.tonal(
              onPressed: _busy ? null : () => _submit(false),
              child: Text(_busy ? 'Publishing…' : 'Publish'),
            ),
          ],
        ),
      ],
    ),
  );
}

class PeopleScreen extends StatefulWidget {
  const PeopleScreen({
    super.key,
    required this.bridge,
    required this.onMessage,
    this.onGroup,
  });
  final WaddleBridge bridge;
  final void Function(String jid, String name) onMessage;
  final void Function(String jid, String name)? onGroup;
  @override
  State<PeopleScreen> createState() => _PeopleScreenState();
}

class _PeopleScreenState extends State<PeopleScreen> {
  final _search = TextEditingController();
  final _selection = <String>{};
  Timer? _timer;
  List<Map<String, dynamic>> _results = [];
  bool _loading = false, _groupMode = false, _creating = false;
  int _ticket = 0;
  String? _error;
  @override
  void dispose() {
    _ticket++;
    _timer?.cancel();
    _search.dispose();
    super.dispose();
  }

  void _changed(String query) {
    _timer?.cancel();
    final ticket = ++_ticket;
    if (query.trim().isEmpty) {
      setState(() {
        _loading = false;
        _results = [];
        _error = null;
      });
      return;
    }
    setState(() {
      _loading = true;
      _error = null;
    });
    _timer = Timer(const Duration(milliseconds: 250), () async {
      try {
        final result = await widget.bridge.invoke('directory.search', {
          'query': query.trim(),
        });
        if (!mounted || ticket != _ticket) {
          return;
        }
        setState(() {
          _results = _rows(result is List ? result : _map(result)['users']);
          _loading = false;
        });
      } catch (error) {
        if (mounted && ticket == _ticket) {
          setState(() {
            _error = error.toString();
            _loading = false;
          });
        }
      }
    });
  }

  Future<void> _createGroup() async {
    final controller = TextEditingController();
    final name = await showDialog<String>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('New group message'),
        content: TextField(
          controller: controller,
          autofocus: true,
          maxLength: 80,
          decoration: const InputDecoration(labelText: 'Group name (optional)'),
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context),
            child: const Text('Cancel'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(context, controller.text.trim()),
            child: const Text('Create'),
          ),
        ],
      ),
    );
    // The dialog can retain its controller while its closing transition runs.
    Future<void>.delayed(const Duration(milliseconds: 300), controller.dispose);
    if (name == null || !mounted) {
      return;
    }
    setState(() => _creating = true);
    try {
      final result = _map(
        await widget.bridge.invoke('group.create', {
          'name': name,
          'members': _selection.toList(),
        }),
      );
      if (!mounted) {
        return;
      }
      final jid = _text(result['jid'] ?? result['roomJid']);
      if (jid.isNotEmpty) {
        Navigator.pop(context);
        (widget.onGroup ?? widget.onMessage)(
          jid,
          name.isEmpty ? 'Group message' : name,
        );
      }
    } catch (error) {
      if (mounted) {
        ScaffoldMessenger.of(
          context,
        ).showSnackBar(SnackBar(content: Text(error.toString())));
      }
    } finally {
      if (mounted) {
        setState(() => _creating = false);
      }
    }
  }

  @override
  Widget build(BuildContext context) => AnimatedBuilder(
    animation: widget.bridge,
    builder: (context, _) {
      final people = _search.text.trim().isEmpty
          ? _rows(widget.bridge.home['people'])
          : _results;
      return Scaffold(
        appBar: AppBar(
          title: Text(_groupMode ? 'New group message' : 'People'),
          actions: [
            if (!_groupMode)
              IconButton(
                tooltip: 'New group message',
                icon: const Icon(Icons.group_add_outlined),
                onPressed: () => setState(() => _groupMode = true),
              ),
          ],
        ),
        floatingActionButton: _groupMode
            ? FloatingActionButton.extended(
                onPressed: _selection.isEmpty || _creating
                    ? null
                    : _createGroup,
                icon: _creating
                    ? const SizedBox.square(
                        dimension: 20,
                        child: CircularProgressIndicator(strokeWidth: 2),
                      )
                    : const Icon(Icons.arrow_forward_rounded),
                label: Text('Create · ${_selection.length}'),
              )
            : null,
        body: Column(
          children: [
            Padding(
              padding: const EdgeInsets.fromLTRB(16, 8, 16, 16),
              child: SearchBar(
                controller: _search,
                hintText: 'Find someone',
                leading: const Icon(Icons.search_rounded),
                onChanged: _changed,
                trailing: [
                  if (_search.text.isNotEmpty)
                    IconButton(
                      tooltip: 'Clear search',
                      onPressed: () {
                        _search.clear();
                        _changed('');
                      },
                      icon: const Icon(Icons.close_rounded),
                    ),
                ],
              ),
            ),
            if (_loading) const LinearProgressIndicator(),
            if (_error != null)
              MaterialBanner(
                content: const Text('Could not search the directory.'),
                actions: [
                  TextButton(
                    onPressed: () => _changed(_search.text),
                    child: const Text('Retry'),
                  ),
                ],
              ),
            Expanded(
              child: people.isEmpty && !_loading
                  ? _Empty(
                      Icons.people_outline_rounded,
                      _search.text.isEmpty
                          ? 'Your community'
                          : 'No people found',
                      _search.text.isEmpty
                          ? 'Search the directory to start a conversation.'
                          : 'Try another name or address.',
                    )
                  : ListView.builder(
                      padding: const EdgeInsets.only(bottom: 100),
                      itemCount: people.length,
                      itemBuilder: (context, index) {
                        final person = people[index];
                        final jid = _text(person['jid'] ?? person['bareJid']);
                        final name = _text(
                          person['name'] ??
                              person['displayName'] ??
                              person['username'],
                          jid,
                        );
                        return ListTile(
                          leading: PersonAvatar(name: name),
                          title: Text(name),
                          subtitle: Text(
                            person['inHuddle'] == true
                                ? 'In a call'
                                : _text(person['show'], jid),
                          ),
                          trailing: _groupMode
                              ? Checkbox(
                                  value: _selection.contains(jid),
                                  onChanged: (_) => setState(() {
                                    if (!_selection.add(jid)) {
                                      _selection.remove(jid);
                                    }
                                  }),
                                )
                              : const Icon(Icons.chat_bubble_outline_rounded),
                          onTap: () {
                            if (_groupMode) {
                              setState(() {
                                if (!_selection.add(jid)) {
                                  _selection.remove(jid);
                                }
                              });
                            } else {
                              Navigator.pop(context);
                              widget.onMessage(jid, name);
                            }
                          },
                        );
                      },
                    ),
            ),
          ],
        ),
      );
    },
  );
}

class CommunityAdminScreen extends StatefulWidget {
  const CommunityAdminScreen({super.key, required this.bridge});
  final WaddleBridge bridge;
  @override
  State<CommunityAdminScreen> createState() => _CommunityAdminScreenState();
}

class _CommunityAdminScreenState extends State<CommunityAdminScreen> {
  final _query = TextEditingController();
  final _users = <Map<String, dynamic>>[];
  Timer? _timer;
  String? _cursor, _error;
  bool _loading = false;
  int _ticket = 0;
  @override
  void initState() {
    super.initState();
    _load();
  }

  @override
  void dispose() {
    _ticket++;
    _timer?.cancel();
    _query.dispose();
    super.dispose();
  }

  Future<void> _load({bool more = false}) async {
    if (more && (_cursor == null || _loading)) {
      return;
    }
    final ticket = ++_ticket;
    setState(() {
      _loading = true;
      _error = null;
    });
    try {
      final page = _map(
        await widget.bridge.invoke('admin.users', {
          'prefix': _query.text.trim(),
          if (more) 'afterCursor': _cursor,
        }),
      );
      if (!mounted || ticket != _ticket) {
        return;
      }
      setState(() {
        if (!more) {
          _users.clear();
        }
        _users.addAll(_rows(page['users'] ?? page['entries']));
        _cursor = page['nextCursor'] as String?;
        _loading = false;
      });
    } catch (error) {
      if (mounted && ticket == _ticket) {
        setState(() {
          _loading = false;
          _error = error.toString();
        });
      }
    }
  }

  @override
  Widget build(BuildContext context) => Scaffold(
    appBar: AppBar(title: const Text('Community users')),
    body: Column(
      children: [
        Padding(
          padding: const EdgeInsets.all(16),
          child: SearchBar(
            controller: _query,
            leading: const Icon(Icons.search_rounded),
            hintText: 'Search by username',
            onChanged: (_) {
              _ticket++;
              _timer?.cancel();
              _timer = Timer(const Duration(milliseconds: 250), _load);
            },
          ),
        ),
        if (_loading) const LinearProgressIndicator(),
        if (_error != null)
          MaterialBanner(
            content: const Text('Could not load community users.'),
            actions: [TextButton(onPressed: _load, child: const Text('Retry'))],
          ),
        Expanded(
          child: _users.isEmpty && !_loading && _error == null
              ? const _Empty(
                  Icons.manage_accounts_outlined,
                  'No users found',
                  'Try another username.',
                )
              : ListView.builder(
                  itemCount: _users.length + (_cursor == null ? 0 : 1),
                  itemBuilder: (context, index) {
                    if (index == _users.length) {
                      return Padding(
                        padding: const EdgeInsets.all(16),
                        child: FilledButton.tonal(
                          onPressed: _loading ? null : () => _load(more: true),
                          child: const Text('Load more'),
                        ),
                      );
                    }
                    final user = _users[index];
                    final jid = _text(user['jid']);
                    final name = _text(user['username'] ?? user['name'], jid);
                    return ListTile(
                      leading: PersonAvatar(name: name),
                      title: Text(name),
                      subtitle: Text(jid),
                      trailing: Text(_text(user['role'])),
                    );
                  },
                ),
        ),
      ],
    ),
  );
}

class ActivityScreen extends StatelessWidget {
  const ActivityScreen({
    super.key,
    required this.bridge,
    required this.onOpen,
    this.embedded = true,
  });
  final WaddleBridge bridge;
  final void Function(String jid, String title, bool group) onOpen;
  final bool embedded;
  @override
  Widget build(BuildContext context) => AnimatedBuilder(
    animation: bridge,
    builder: (context, _) {
      final activity = bridge.topic('activity');
      final mentions = _rows(activity['mentions']);
      final inbox = _rows(activity['entries']);
      final unread = [
        ..._rows(bridge.home['rooms'])
            .where((row) => (row['unread'] as num? ?? 0) > 0)
            .map((row) => {...row, 'group': true}),
        ..._rows(
          bridge.home['dmPeers'],
        ).where((row) => (row['unread'] as num? ?? 0) > 0),
      ];
      final rows = inbox.isEmpty ? unread : inbox;
      return Scaffold(
        appBar: embedded ? null : AppBar(title: const Text('Activity')),
        body: rows.isEmpty && mentions.isEmpty
            ? const _Empty(
                Icons.mark_chat_read_outlined,
                'All caught up',
                'New messages and mentions will appear here.',
              )
            : ListView(
                padding: const EdgeInsets.only(bottom: 24),
                children: [
                  if (mentions.isNotEmpty)
                    _Section(
                      'Mentions',
                      mentions
                          .map(
                            (row) => ListTile(
                              leading: const CircleAvatar(
                                child: Icon(Icons.alternate_email_rounded),
                              ),
                              title: Text(
                                _text(row['author'] ?? row['name'], 'Mention'),
                              ),
                              subtitle: Text(
                                _text(row['body']),
                                maxLines: 2,
                                overflow: TextOverflow.ellipsis,
                              ),
                              onTap: () => onOpen(
                                _text(row['jid'] ?? row['conversationJid']),
                                _text(row['title'] ?? row['name']),
                                row['group'] != false,
                              ),
                            ),
                          )
                          .toList(),
                    ),
                  if (rows.isNotEmpty)
                    _Section(
                      'Unread conversations',
                      rows.map((row) {
                        final jid = _text(row['jid'] ?? row['conversationJid']);
                        final name = _text(row['name'] ?? row['title'], jid);
                        return ListTile(
                          leading: PersonAvatar(name: name),
                          title: Text(name),
                          subtitle: row['body'] == null
                              ? null
                              : Text(
                                  _text(row['body']),
                                  maxLines: 1,
                                  overflow: TextOverflow.ellipsis,
                                ),
                          trailing: Badge.count(
                            count: (row['unread'] as num? ?? 1).toInt(),
                          ),
                          onTap: () => onOpen(
                            jid,
                            name,
                            row['group'] == true ||
                                row['groupDm'] == true ||
                                jid.contains('@muc.'),
                          ),
                        );
                      }).toList(),
                    ),
                ],
              ),
      );
    },
  );
}

class ActiveCallScreen extends StatefulWidget {
  const ActiveCallScreen({super.key, required this.bridge});
  final WaddleBridge bridge;
  @override
  State<ActiveCallScreen> createState() => _ActiveCallScreenState();
}

class _ActiveCallScreenState extends State<ActiveCallScreen> {
  late final Timer _timer;
  DateTime? _activeSince;
  WaddleBridge get bridge => widget.bridge;
  @override
  void initState() {
    super.initState();
    _trackCall();
    bridge.addListener(_trackCall);
    _timer = Timer.periodic(const Duration(seconds: 1), (_) {
      if (mounted && _activeSince != null) {
        setState(() {});
      }
    });
  }

  void _trackCall() {
    if (bridge.call['state'] == 'active') {
      _activeSince ??= DateTime.now();
    } else {
      _activeSince = null;
    }
  }

  @override
  void dispose() {
    bridge.removeListener(_trackCall);
    _timer.cancel();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => AnimatedBuilder(
    animation: bridge,
    builder: (context, _) {
      final call = bridge.call;
      final state = _text(call['state'], 'idle');
      final incoming = state == 'incoming';
      final ended = state == 'ended' || state == 'idle';
      final name = _text(
        call['peerName'] ?? call['peer'] ?? call['peerJid'],
        'Call',
      );
      final roster = _rows(call['participants'] ?? call['roster']);
      final video = call['video'] == true;
      final seconds =
          (call['durationSeconds'] as num?)?.toInt() ??
          (_activeSince == null
              ? 0
              : DateTime.now().difference(_activeSince!).inSeconds);
      final subtitle = switch (state) {
        'incoming' => video ? 'Incoming video call' : 'Incoming voice call',
        'outgoing' => 'Calling…',
        'connecting' => 'Connecting…',
        'active' =>
          '${seconds ~/ 60}:${(seconds % 60).toString().padLeft(2, '0')}',
        'ended' => 'Call ended',
        _ => 'Connecting…',
      };
      Widget control(
        String label,
        IconData icon,
        String command, {
        bool selected = false,
      }) => Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          IconButton.filledTonal(
            isSelected: selected,
            onPressed: () => _command(context, bridge, 'call.$command'),
            icon: Icon(icon),
            tooltip: label,
            iconSize: 28,
            padding: const EdgeInsets.all(20),
          ),
          const SizedBox(height: 8),
          Text(label, style: Theme.of(context).textTheme.labelMedium),
        ],
      );
      return Scaffold(
        appBar: AppBar(title: Text(incoming ? 'Incoming call' : 'Call')),
        body: SafeArea(
          child: Column(
            children: [
              Expanded(
                child:
                    video &&
                        !incoming &&
                        !ended &&
                        !kIsWeb &&
                        defaultTargetPlatform == TargetPlatform.android
                    ? Padding(
                        padding: const EdgeInsets.all(16),
                        child: Stack(
                          children: [
                            const Positioned.fill(
                              child: ClipRRect(
                                borderRadius: BorderRadius.all(
                                  Radius.circular(28),
                                ),
                                child: AndroidView(
                                  viewType: 'social.waddle.android/call-video',
                                  creationParamsCodec: StandardMessageCodec(),
                                ),
                              ),
                            ),
                            if (call['cameraEnabled'] == true)
                              const Positioned(
                                bottom: 16,
                                right: 16,
                                width: 104,
                                height: 144,
                                child: ClipRRect(
                                  borderRadius: BorderRadius.all(
                                    Radius.circular(20),
                                  ),
                                  child: AndroidView(
                                    viewType:
                                        'social.waddle.android/call-video',
                                    creationParams: {'local': true},
                                    creationParamsCodec: StandardMessageCodec(),
                                  ),
                                ),
                              ),
                            Positioned(
                              top: 16,
                              left: 16,
                              right: 16,
                              child: Material(
                                color: Theme.of(
                                  context,
                                ).colorScheme.surfaceContainer,
                                borderRadius: BorderRadius.circular(20),
                                child: Padding(
                                  padding: const EdgeInsets.all(12),
                                  child: Text(
                                    '$name · $subtitle',
                                    textAlign: TextAlign.center,
                                  ),
                                ),
                              ),
                            ),
                          ],
                        ),
                      )
                    : Center(
                        child: Column(
                          mainAxisSize: MainAxisSize.min,
                          children: [
                            PersonAvatar(name: name, radius: 64),
                            const SizedBox(height: 24),
                            Text(
                              name,
                              textAlign: TextAlign.center,
                              style: Theme.of(context).textTheme.headlineMedium,
                            ),
                            const SizedBox(height: 8),
                            Text(
                              subtitle,
                              style: Theme.of(context).textTheme.titleMedium,
                            ),
                          ],
                        ),
                      ),
              ),
              if (call['error'] != null)
                Padding(
                  padding: const EdgeInsets.all(16),
                  child: Text(
                    _text(call['error']),
                    style: TextStyle(
                      color: Theme.of(context).colorScheme.error,
                    ),
                  ),
                ),
              if (roster.isNotEmpty)
                SizedBox(
                  height: 92,
                  child: ListView.separated(
                    padding: const EdgeInsets.symmetric(horizontal: 24),
                    scrollDirection: Axis.horizontal,
                    itemCount: roster.length,
                    separatorBuilder: (_, _) => const SizedBox(width: 16),
                    itemBuilder: (context, index) {
                      final person = roster[index];
                      return Column(
                        children: [
                          Badge(
                            isLabelVisible: person['handRaised'] == true,
                            label: const Icon(
                              Icons.back_hand_rounded,
                              size: 12,
                            ),
                            child: PersonAvatar(
                              name: _text(
                                person['name'] ?? person['nick'],
                                '?',
                              ),
                            ),
                          ),
                          const SizedBox(height: 6),
                          Text(_text(person['name'] ?? person['nick'])),
                        ],
                      );
                    },
                  ),
                ),
              if (incoming)
                Padding(
                  padding: const EdgeInsets.all(32),
                  child: Row(
                    mainAxisAlignment: MainAxisAlignment.spaceEvenly,
                    children: [
                      control('Decline', Icons.call_end_rounded, 'decline'),
                      control(
                        'Answer',
                        video ? Icons.videocam_rounded : Icons.call_rounded,
                        'accept',
                      ),
                    ],
                  ),
                )
              else if (ended)
                Padding(
                  padding: const EdgeInsets.all(32),
                  child: FilledButton(
                    onPressed: () async {
                      await _command(context, bridge, 'call.dismiss');
                      if (context.mounted) {
                        Navigator.maybePop(context);
                      }
                    },
                    child: const Text('Done'),
                  ),
                )
              else ...[
                Padding(
                  padding: const EdgeInsets.fromLTRB(16, 16, 16, 24),
                  child: Wrap(
                    spacing: 20,
                    runSpacing: 20,
                    alignment: WrapAlignment.center,
                    children: [
                      control(
                        call['microphoneEnabled'] == false ? 'Unmute' : 'Mute',
                        call['microphoneEnabled'] == false
                            ? Icons.mic_off_rounded
                            : Icons.mic_rounded,
                        'microphone',
                        selected: call['microphoneEnabled'] == false,
                      ),
                      control(
                        'Speaker',
                        Icons.volume_up_rounded,
                        'speaker',
                        selected: call['speakerEnabled'] == true,
                      ),
                      if (video)
                        control(
                          'Camera',
                          call['cameraEnabled'] == false
                              ? Icons.videocam_off_rounded
                              : Icons.videocam_rounded,
                          'camera',
                          selected: call['cameraEnabled'] != false,
                        ),
                      if (video)
                        control(
                          'Flip camera',
                          Icons.cameraswitch_rounded,
                          'flipCamera',
                        ),
                      if (call['group'] == true)
                        control(
                          call['handRaised'] == true
                              ? 'Lower hand'
                              : 'Raise hand',
                          Icons.back_hand_outlined,
                          'hand',
                          selected: call['handRaised'] == true,
                        ),
                    ],
                  ),
                ),
                Padding(
                  padding: const EdgeInsets.only(bottom: 32),
                  child: FilledButton.icon(
                    style: FilledButton.styleFrom(
                      backgroundColor: Theme.of(context).colorScheme.error,
                      foregroundColor: Theme.of(context).colorScheme.onError,
                      minimumSize: const Size(160, 56),
                    ),
                    onPressed: () => _command(context, bridge, 'call.hangup'),
                    icon: const Icon(Icons.call_end_rounded),
                    label: const Text('End call'),
                  ),
                ),
              ],
            ],
          ),
        ),
      );
    },
  );
}
