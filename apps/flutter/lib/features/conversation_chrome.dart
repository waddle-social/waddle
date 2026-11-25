part of 'conversation.dart';

class _MessageFrame extends StatefulWidget {
  const _MessageFrame({
    super.key,
    required this.wide,
    required this.actions,
    required this.padding,
    required this.timeInGutter,
    required this.child,
  });

  final bool wide;
  final EdgeInsets padding;
  final List<Widget> actions;
  final String timeInGutter;
  final Widget child;

  @override
  State<_MessageFrame> createState() => _MessageFrameState();
}

class _MessageFrameState extends State<_MessageFrame> {
  bool _hover = false;
  bool _focus = false;

  @override
  Widget build(BuildContext context) {
    final row = Padding(padding: widget.padding, child: widget.child);
    if (!widget.wide) return row;
    final colors = Theme.of(context).colorScheme;
    final active = _hover || _focus;
    return MouseRegion(
      onEnter: (_) => setState(() => _hover = true),
      onExit: (_) => setState(() => _hover = false),
      child: Focus(
        canRequestFocus: false,
        onFocusChange: (value) => setState(() => _focus = value),
        child: Material(
          color: active ? colors.surfaceContainerHigh : colors.surface,
          borderRadius: BorderRadius.circular(8),
          child: Stack(
            children: [
              row,
              if (active && widget.timeInGutter.isNotEmpty)
                PositionedDirectional(
                  start: 8,
                  top: 4,
                  width: 48,
                  child: FittedBox(
                    fit: BoxFit.scaleDown,
                    child: Text(
                      widget.timeInGutter,
                      style: Theme.of(context).textTheme.labelSmall?.copyWith(
                        color: colors.onSurfaceVariant,
                      ),
                    ),
                  ),
                ),
              if (widget.actions.isNotEmpty)
                PositionedDirectional(
                  end: 8,
                  top: 0,
                  child: Opacity(
                    opacity: active ? 1 : 0,
                    alwaysIncludeSemantics: true,
                    child: Material(
                      color: colors.surfaceContainer,
                      shape: RoundedRectangleBorder(
                        side: BorderSide(color: colors.outlineVariant),
                        borderRadius: BorderRadius.circular(8),
                      ),
                      child: IconButtonTheme(
                        data: IconButtonThemeData(
                          style: IconButton.styleFrom(
                            minimumSize: const Size(36, 32),
                            padding: EdgeInsets.zero,
                            tapTargetSize: MaterialTapTargetSize.shrinkWrap,
                            iconSize: 18,
                          ),
                        ),
                        child: Row(
                          mainAxisSize: MainAxisSize.min,
                          children: widget.actions,
                        ),
                      ),
                    ),
                  ),
                ),
            ],
          ),
        ),
      ),
    );
  }
}

class _ConversationEmptyState extends StatelessWidget {
  const _ConversationEmptyState({
    required this.title,
    required this.room,
    required this.thread,
  });

  final String title;
  final bool room;
  final bool thread;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = theme.colorScheme;
    return Center(
      child: SingleChildScrollView(
        padding: const EdgeInsets.all(24),
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 480),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              if (room && !thread)
                Image.asset(
                  'assets/brand/waddle-mark.png',
                  width: 88,
                  height: 88,
                  excludeFromSemantics: true,
                )
              else
                CircleAvatar(
                  radius: 40,
                  backgroundColor: colors.primaryContainer,
                  child: Icon(
                    thread ? Icons.forum_outlined : Icons.chat_bubble_outline,
                    size: 34,
                    color: colors.onPrimaryContainer,
                  ),
                ),
              const SizedBox(height: 16),
              Text(
                thread
                    ? 'No replies yet'
                    : room
                    ? "It’s quiet in #$title"
                    : 'Just you and $title',
                textAlign: TextAlign.center,
                style: theme.textTheme.headlineMedium,
              ),
              const SizedBox(height: 8),
              Text(
                thread
                    ? 'Send the first reply.'
                    : room
                    ? "Say hello, share what you’re working on, or ask the room something."
                    : 'Send the first message to get the conversation going.',
                textAlign: TextAlign.center,
                style: theme.textTheme.bodyMedium?.copyWith(
                  color: colors.onSurfaceVariant,
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}
