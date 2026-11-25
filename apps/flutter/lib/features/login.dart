import 'dart:async';

import 'package:flutter/material.dart';

import '../app.dart' show perform, records;
import '../bridge.dart';
import '../runtime/session_auth.dart';

class LoginScreen extends StatefulWidget {
  const LoginScreen({super.key, required this.bridge});
  final WaddleBridge bridge;

  @override
  State<LoginScreen> createState() => _LoginState();
}

class _LoginState extends State<LoginScreen> {
  final _address = TextEditingController();
  Timer? _debounce;
  bool _editing = false;
  bool _other = false;
  String? _error;
  String? _pendingServer;
  int _revision = 0;
  int _request = 0;

  String get _default => HttpAuthGateway.defaultBaseUri.origin;
  String get _active =>
      widget.bridge.session['serverOrigin'] as String? ?? _default;
  String get _draft => _other ? _address.text.trim() : _default;

  bool get _currentServer {
    if (_pendingServer != null) return false;
    if (!_editing) return true;
    try {
      return HttpAuthGateway.serverOrigin(_draft).origin == _active;
    } on FormatException {
      return false;
    }
  }

  @override
  void initState() {
    super.initState();
    if (widget.bridge.session['canSwitchAccounts'] == true) {
      unawaited(
        widget.bridge.invoke('session.accounts').catchError((Object _) => null),
      );
    }
    unawaited(
      widget.bridge.invoke('login.providers').catchError((Object _) => null),
    );
  }

  @override
  void dispose() {
    _debounce?.cancel();
    _address.dispose();
    super.dispose();
  }

  void _toggleEditor() {
    _debounce?.cancel();
    ++_revision;
    setState(() {
      _editing = !_editing;
      _other = _active != _default;
      _address.text = _other ? _active : '';
      _error = null;
    });
  }

  void _editAddress(String _) {
    _debounce?.cancel();
    ++_revision;
    setState(() => _error = null);
    if (_draft.isNotEmpty) {
      _debounce = Timer(const Duration(milliseconds: 500), _applyAddress);
    }
  }

  Future<void> _applyAddress() async {
    _debounce?.cancel();
    final revision = ++_revision;
    try {
      final address = HttpAuthGateway.serverOrigin(_draft).origin;
      if (address == _pendingServer ||
          (address == _active && _pendingServer == null)) {
        return;
      }
      final request = ++_request;
      setState(() => _pendingServer = address);
      try {
        await widget.bridge.invoke('login.server', {'address': address});
      } finally {
        if (mounted && request == _request) {
          setState(() => _pendingServer = null);
        }
      }
    } catch (error) {
      if (mounted && revision == _revision) {
        setState(
          () => _error = error is FormatException
              ? error.message
              : WaddleBridge.describeError(error),
        );
      }
    }
  }

  void _select(bool other) {
    if (_other == other) return;
    _debounce?.cancel();
    ++_revision;
    setState(() {
      _other = other;
      _error = null;
      if (!other) _address.clear();
    });
    if (!other) unawaited(_applyAddress());
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final type = Theme.of(context).textTheme;
    return Scaffold(
      body: SafeArea(
        child: Center(
          child: SingleChildScrollView(
            padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 48),
            child: ConstrainedBox(
              constraints: const BoxConstraints(maxWidth: 384),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  Center(
                    child: Image.asset(
                      'assets/brand/spot-conversation.png',
                      width: 120,
                      height: 120,
                      semanticLabel: 'Waddle',
                    ),
                  ),
                  const SizedBox(height: 20),
                  Semantics(
                    header: true,
                    child: Text(
                      'Sign in to Waddle',
                      textAlign: TextAlign.center,
                      style: type.headlineLarge?.copyWith(
                        fontFamily: 'Bricolage Grotesque',
                        fontSize: 36,
                        height: 1.1,
                        letterSpacing: -1.08,
                        fontWeight: FontWeight.w700,
                      ),
                    ),
                  ),
                  const SizedBox(height: 8),
                  Text(
                    'Rooms, discussions, and the people in them.',
                    textAlign: TextAlign.center,
                    style: type.bodyLarge?.copyWith(
                      fontSize: 16,
                      color: scheme.onSurfaceVariant,
                    ),
                  ),
                  const SizedBox(height: 20),
                  _card(scheme, type),
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }

  Widget _card(ColorScheme scheme, TextTheme type) {
    final login = widget.bridge.login;
    final state = login['state'];
    final locked = state == 'verifying' || state == 'completing';
    final accounts = records(widget.bridge.topic('accounts')['saved']);
    final providers = records(login['providers']);
    final label = type.labelSmall?.copyWith(
      fontFamily: 'Atkinson Hyperlegible Mono',
      fontSize: 11,
      fontWeight: FontWeight.w600,
      letterSpacing: 1.1,
      color: scheme.onSurfaceVariant,
    );
    final hostStyle = type.bodyMedium?.copyWith(
      fontFamily: 'Atkinson Hyperlegible Mono',
      fontSize: 14,
    );
    final error =
        _error ??
        (state == 'failed'
            ? '${login['error'] ?? 'Sign-in failed. Please try again.'}'
            : null);

    return Material(
      color: scheme.surfaceContainer,
      shape: RoundedRectangleBorder(
        borderRadius: BorderRadius.circular(16),
        side: BorderSide(color: scheme.outlineVariant),
      ),
      child: Padding(
        padding: const EdgeInsets.all(20),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            if (widget.bridge.session['serverOrigin'] is String) ...[
              ..._serverFields(scheme, hostStyle, label, locked),
              Padding(
                padding: const EdgeInsets.symmetric(vertical: 16),
                child: Divider(height: 1, color: scheme.outlineVariant),
              ),
            ],
            if (error != null)
              Padding(
                padding: const EdgeInsets.only(bottom: 12),
                child: Semantics(
                  liveRegion: true,
                  child: DecoratedBox(
                    decoration: BoxDecoration(
                      color: scheme.error.withValues(alpha: .1),
                      border: Border.all(color: scheme.error),
                      borderRadius: BorderRadius.circular(10),
                    ),
                    child: Padding(
                      padding: const EdgeInsets.symmetric(
                        horizontal: 12,
                        vertical: 10,
                      ),
                      child: Text(
                        error,
                        style: type.bodySmall?.copyWith(
                          color: scheme.onErrorContainer,
                        ),
                      ),
                    ),
                  ),
                ),
              ),
            ..._actions(login, providers, label),
            if (accounts.isNotEmpty && !locked) ...[
              const SizedBox(height: 16),
              Text('SAVED ACCOUNTS', style: label),
              for (final account in accounts)
                ListTile(
                  contentPadding: EdgeInsets.zero,
                  leading: const Icon(Icons.account_circle_outlined),
                  title: Text('${account['jid']}'),
                  subtitle: Text('${account['origin']}'),
                  onTap: () => perform(
                    context,
                    () => widget.bridge.invoke('session.resume', account),
                  ),
                ),
            ],
          ],
        ),
      ),
    );
  }

  List<Widget> _serverFields(
    ColorScheme scheme,
    TextStyle? hostStyle,
    TextStyle? label,
    bool locked,
  ) => [
    Row(
      children: [
        Expanded(child: Text('HOMESERVER', style: label)),
        Semantics(
          expanded: _editing,
          child: OutlinedButton.icon(
            onPressed: locked ? null : _toggleEditor,
            style: OutlinedButton.styleFrom(
              minimumSize: const Size(0, 36),
              visualDensity: VisualDensity.standard,
              foregroundColor: scheme.onSurface,
              side: BorderSide(color: scheme.outlineVariant),
              padding: const EdgeInsets.symmetric(horizontal: 12),
            ),
            iconAlignment: IconAlignment.end,
            icon: Icon(
              _editing ? Icons.keyboard_arrow_up : Icons.keyboard_arrow_down,
              size: 16,
            ),
            label: Text(_editing ? 'Hide' : 'Edit'),
          ),
        ),
      ],
    ),
    if (!_editing || locked)
      Padding(
        padding: const EdgeInsets.only(top: 8),
        child: Row(
          children: [
            Container(
              width: 8,
              height: 8,
              decoration: BoxDecoration(
                shape: BoxShape.circle,
                color: scheme.tertiary,
              ),
            ),
            const SizedBox(width: 8),
            Expanded(
              child: Text(Uri.parse(_active).authority, style: hostStyle),
            ),
          ],
        ),
      )
    else ...[
      const SizedBox(height: 12),
      RadioGroup<bool>(
        groupValue: _other,
        onChanged: (value) {
          if (value != null) _select(value);
        },
        child: Column(
          spacing: 8,
          children: [
            _ServerChoice(
              value: false,
              selected: !_other,
              child: Wrap(
                alignment: WrapAlignment.spaceBetween,
                spacing: 8,
                children: [
                  Text(
                    HttpAuthGateway.defaultBaseUri.authority,
                    style: hostStyle,
                  ),
                  Text('DEFAULT', style: label),
                ],
              ),
            ),
            _ServerChoice(
              value: true,
              selected: _other,
              child: const Text('Other server'),
            ),
          ],
        ),
      ),
      if (_other) ...[
        const SizedBox(height: 8),
        TextField(
          controller: _address,
          keyboardType: TextInputType.url,
          autocorrect: false,
          enableSuggestions: false,
          style: hostStyle,
          decoration: const InputDecoration(
            labelText: 'Custom homeserver URL',
            hintText: 'https://server.example.com',
          ),
          onChanged: _editAddress,
          onSubmitted: (_) => _applyAddress(),
        ),
      ],
    ],
  ];

  List<Widget> _actions(
    Map<String, dynamic> login,
    List<Map<String, dynamic>> providers,
    TextStyle? label,
  ) {
    final state = login['state'];
    final type = Theme.of(context).textTheme;
    final scheme = Theme.of(context).colorScheme;
    return [
      if (state == 'verifying') ...[
        const Text('Approve sign-in in your browser using this code.'),
        const SizedBox(height: 12),
        DecoratedBox(
          decoration: BoxDecoration(
            color: scheme.surface,
            border: Border.all(color: scheme.outlineVariant),
            borderRadius: BorderRadius.circular(12),
          ),
          child: Padding(
            padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 12),
            child: SelectableText(
              '${login['code'] ?? ''}',
              textAlign: TextAlign.center,
              style: type.headlineSmall?.copyWith(
                fontFamily: 'Atkinson Hyperlegible Mono',
                fontSize: 24,
                fontWeight: FontWeight.w700,
                letterSpacing: 1.2,
              ),
            ),
          ),
        ),
        const SizedBox(height: 16),
        const LinearProgressIndicator(semanticsLabel: 'Waiting for approval'),
        const SizedBox(height: 12),
        OutlinedButton(
          onPressed: () =>
              perform(context, () => widget.bridge.invoke('login.cancel')),
          child: const Text('Cancel'),
        ),
      ] else if (!_currentServer) ...[
        const OutlinedButton(
          onPressed: null,
          child: Text('No sign-in methods found'),
        ),
      ] else if (state == 'failed') ...[
        OutlinedButton(
          onPressed: () =>
              perform(context, () => widget.bridge.invoke('login.providers')),
          child: const Text('Try again'),
        ),
      ] else if (state == 'providers') ...[
        if (providers.length > 1) ...[
          Text('SIGN IN WITH', style: label),
          const SizedBox(height: 8),
        ],
        for (final (index, provider) in providers.indexed)
          Padding(
            padding: EdgeInsets.only(top: index == 0 ? 0 : 8),
            child: _providerButton(
              provider,
              primary: index == 0,
              single: providers.length == 1,
            ),
          ),
        if (providers.isEmpty)
          const OutlinedButton(
            onPressed: null,
            child: Text('No sign-in methods found'),
          ),
      ] else ...[
        const LinearProgressIndicator(),
        const SizedBox(height: 8),
        Text(
          state == 'completing'
              ? 'Finishing sign-in…'
              : 'Finding sign-in methods…',
          style: type.bodySmall,
        ),
      ],
    ];
  }

  Widget _providerButton(
    Map<String, dynamic> provider, {
    required bool primary,
    required bool single,
  }) {
    final name = provider['name'] ?? provider['id'];
    final text = Text(
      single ? 'Sign in with $name' : '$name',
      textAlign: TextAlign.center,
    );
    const minimum = Size.fromHeight(44);
    final theme = Theme.of(context);
    void select() => perform(
      context,
      () => widget.bridge.invoke('login.select', {'id': provider['id']}),
    );
    return primary
        ? FilledButton(
            style: FilledButton.styleFrom(
              minimumSize: minimum,
              visualDensity: VisualDensity.standard,
              textStyle: theme.textTheme.labelLarge?.copyWith(
                fontWeight: FontWeight.w700,
              ),
            ),
            onPressed: select,
            child: text,
          )
        : OutlinedButton(
            style: OutlinedButton.styleFrom(
              minimumSize: minimum,
              visualDensity: VisualDensity.standard,
              foregroundColor: theme.colorScheme.onSurface,
              side: BorderSide(color: theme.colorScheme.outlineVariant),
            ),
            onPressed: select,
            child: text,
          );
  }
}

class _ServerChoice extends StatelessWidget {
  const _ServerChoice({
    required this.value,
    required this.selected,
    required this.child,
  });
  final bool value, selected;
  final Widget child;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return RadioListTile<bool>(
      value: value,
      selected: selected,
      title: child,
      tileColor: selected ? scheme.primaryContainer : scheme.surfaceContainer,
      contentPadding: const EdgeInsets.symmetric(horizontal: 12),
      visualDensity: VisualDensity.compact,
      shape: RoundedRectangleBorder(
        borderRadius: BorderRadius.circular(12),
        side: BorderSide(
          color: selected ? scheme.primary : scheme.outlineVariant,
        ),
      ),
    );
  }
}
