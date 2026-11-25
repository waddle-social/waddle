import 'package:flutter/material.dart';

class SessionLanding extends StatelessWidget {
  const SessionLanding({
    super.key,
    required this.title,
    this.message,
    this.onRetry,
    this.onSignOut,
  });

  final String title;
  final String? message;
  final VoidCallback? onRetry;
  final VoidCallback? onSignOut;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Scaffold(
      body: SafeArea(
        child: Center(
          child: SingleChildScrollView(
            padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 48),
            child: ConstrainedBox(
              constraints: const BoxConstraints(maxWidth: 384),
              child: Column(
                mainAxisSize: MainAxisSize.min,
                children: [
                  Image.asset(
                    'assets/brand/spot-conversation.png',
                    width: 120,
                    height: 120,
                    excludeFromSemantics: true,
                  ),
                  const SizedBox(height: 20),
                  Semantics(
                    header: true,
                    liveRegion: true,
                    child: Text(
                      title,
                      textAlign: TextAlign.center,
                      style: theme.textTheme.headlineLarge,
                    ),
                  ),
                  if (message != null) ...[
                    const SizedBox(height: 12),
                    Text(
                      message!,
                      textAlign: TextAlign.center,
                      style: theme.textTheme.bodyMedium?.copyWith(
                        color: theme.colorScheme.onSurfaceVariant,
                      ),
                    ),
                  ],
                  const SizedBox(height: 12),
                  if (onRetry != null)
                    SizedBox(
                      width: 160,
                      child: FilledButton(
                        onPressed: onRetry,
                        child: const Text('Try again'),
                      ),
                    )
                  else
                    const SizedBox(width: 48, child: LinearProgressIndicator()),
                  if (onSignOut != null)
                    TextButton(
                      onPressed: onSignOut,
                      child: const Text('Sign out'),
                    ),
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }
}
