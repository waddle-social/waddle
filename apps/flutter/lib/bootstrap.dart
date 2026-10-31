import 'dart:async';

import 'package:dynamic_color/dynamic_color.dart';
import 'package:flutter/material.dart';

import 'bridge.dart';
import 'main.dart';
import 'runtime/context.dart';
import 'runtime/notification_owner.dart';
import 'runtime/notification_service.dart';
import 'runtime/platform_services.dart';
import 'runtime/secure_storage.dart';
import 'runtime/session_auth.dart';
import 'runtime/session_runtime.dart';
import 'transport/transport.dart';

/// The OS starts this entry point for a notification action when the UI isolate
/// does not exist. Ownership is acquired before restoring an account or socket.
@pragma('vm:entry-point')
Future<void> notificationBootstrap(Map<String, dynamic> event) async {
  WidgetsFlutterBinding.ensureInitialized();
  await bootWaddle(headless: true, initialEvent: event);
}

Future<void> bootWaddle({
  bool headless = false,
  Map<String, dynamic>? initialEvent,
}) async {
  late final SessionRuntime runtime;
  late final PlatformServices platform;
  late final NotificationService notifications;
  final platformEvents = StreamController<Map<String, dynamic>>.broadcast();
  final subscriptions = <StreamSubscription<Map<String, dynamic>>>[];
  final navigator = GlobalKey<NavigatorState>();
  Future<void>? closing;
  var ownsRuntime = false;
  WaddleBridge? bridge;

  void effect(FutureOr<void> Function() action) {
    unawaited(
      Future<void>.sync(action).catchError((Object error) {
        runtime.emit('error', {'message': WaddleBridge.describeError(error)});
      }),
    );
  }

  Future<void> close() {
    final current = closing;
    if (current != null) return current;
    final attempt = () async {
      await runtime.dispose();
      await platform.dispose();
      await notifications.dispose();
      for (final subscription in subscriptions) {
        await subscription.cancel();
      }
      await platformEvents.close();
      if (ownsRuntime) {
        await Future<void>.sync(NotificationOwner.release);
        ownsRuntime = false;
      }
    }();
    closing = attempt;
    return attempt.then<void>(
      (_) {},
      onError: (Object error, StackTrace stack) {
        // Keep ownership on failure, but let a later handoff retry the drain.
        if (identical(closing, attempt)) closing = null;
        Error.throwWithStackTrace(error, stack);
      },
    );
  }

  runtime = SessionRuntime(
    transportFactory: createRustTransport,
    initiallyVisible: !headless,
    storage: const SecureRuntimeStorage(),
    auth: HttpAuthGateway(baseUri: HttpAuthGateway.defaultBaseUri),
    platformInvoker: (method, args) =>
        method.startsWith('push.') || method == 'app.visibility'
        ? notifications.invoke(method, args)
        : platform.invoke(method, args),
    rawEventObserver: (event) {
      effect(() => platform.onEvent(event));
      effect(() => notifications.onEvent(event));
    },
    accountChangedObserver: () {
      effect(platform.onAccountChanged);
      effect(notifications.onAccountChanged);
    },
  );
  platform = PlatformServices(
    runtime,
    callForeground: (active, video, sid) async {
      await notifications.invoke('push.callForeground', {
        'active': active,
        'video': video,
        'sid': sid,
      });
    },
  );
  notifications = NotificationService(
    runtime,
    initiallyVisible:
        !headless ||
        WidgetsBinding.instance.lifecycleState == AppLifecycleState.resumed,
    callAction: (action, sid) => runtime.withConnectionDemand(() async {
      await platform.invoke(
        action == 'answer' ? 'call.accept' : 'call.decline',
        {'expectedSid': sid},
      );
    }),
    reply:
        ({
          required String jid,
          required bool group,
          required String body,
          String? threadId,
        }) => runtime.withConnectionDemand(
          () => runtime.conversations.notificationReply(
            jid: jid,
            group: group,
            body: body,
            threadId: threadId,
            waitForConnection: true,
          ),
          waitForConnection: false,
        ),
    selectDistributor: (choices) async {
      final context = navigator.currentContext;
      if (context == null) return null;
      return showModalBottomSheet<String>(
        context: context,
        showDragHandle: true,
        builder: (context) => SafeArea(
          child: ListView(
            shrinkWrap: true,
            children: [
              const ListTile(title: Text('Choose a push distributor')),
              for (final choice in choices)
                ListTile(
                  title: Text(choice),
                  onTap: () => Navigator.pop(context, choice),
                ),
            ],
          ),
        ),
      );
    },
  );

  try {
    ownsRuntime = await NotificationOwner.acquire(
      headless: headless,
      onEvent: notifications.handleOwnerEvent,
      onRetire: close,
    );
    if (!ownsRuntime) {
      // UnifiedPush can create a short-lived engine while the UI engine already
      // owns the account. Forward its platform callbacks without restoring Rust.
      await close();
      if (!headless) {
        throw StateError('The application is already active.');
      }
      if (initialEvent != null) {
        await Future<void>.sync(() => NotificationOwner.deliver(initialEvent));
      } else {
        await initializeNotificationForwarder();
      }
      return;
    }

    await Future<void>.sync(
      () => NotificationOwner.configureBootstrap(notificationBootstrap),
    );
    subscriptions.add(
      platform.events.listen((event) {
        platformEvents.add(event);
        if (event['topic'] == 'call') {
          effect(
            () => notifications.invoke('push.callState', {
              ...runtimeMap(event['data']),
              'generation': event['generation'],
            }),
          );
        }
      }),
    );
    subscriptions.add(notifications.events.listen(platformEvents.add));
    runtime.attachPlatformEvents(platformEvents.stream);
    bridge = WaddleBridge(
      commandInvoker: (method, args) async {
        if (method == 'session.logout') {
          // Endpoint retirement is best effort; local sign-out always completes.
          try {
            await notifications
                .invoke('push.retire', const {})
                .timeout(const Duration(seconds: 3));
          } catch (_) {
            // Account generation invalidation also rejects any late callback.
          }
        }
        final result = await runtime.invoke(method, args);
        if (method == 'preferences.set' &&
            {
              'notificationsEnabled',
              'messageSoundsEnabled',
              'stayConnected',
            }.contains(args['key'])) {
          await notifications.invoke('push.reconcile', const {});
        }
        return result;
      },
      events: runtime.events,
      onDispose: () => effect(close),
      callVideoBuilder: (context, local) =>
          platform.buildCallVideo(context, local: local),
    );

    await notifications.initialize();
    runApp(
      _RuntimeVisibility(
        initiallyVisible: !headless,
        onDispose: () => effect(close),
        onVisibility: (visible) => effect(() async {
          // Set notification visibility before retiring a hidden stream, so a
          // push arriving during its close cannot be suppressed as foreground.
          final notice = notifications.invoke('app.visibility', {
            'visible': visible,
          });
          final demand = runtime.onVisibility(visible);
          await Future.wait<dynamic>([notice, demand]);
        }),
        child: DynamicColorBuilder(
          builder: (light, dark) => WaddleApp(
            bridge: bridge!,
            navigatorKey: navigator,
            wallpaperLight: light,
            wallpaperDark: dark,
          ),
        ),
      ),
    );
    if (headless) {
      await runtime.start();
      if (initialEvent != null) {
        await notifications.handleOwnerEvent(initialEvent);
      }
    }
  } catch (_) {
    bridge?.dispose();
    await close();
    rethrow;
  }
}

class _RuntimeVisibility extends StatefulWidget {
  const _RuntimeVisibility({
    required this.child,
    required this.onVisibility,
    required this.onDispose,
    required this.initiallyVisible,
  });
  final Widget child;
  final ValueChanged<bool> onVisibility;
  final VoidCallback onDispose;
  final bool initiallyVisible;

  @override
  State<_RuntimeVisibility> createState() => _RuntimeVisibilityState();
}

class _RuntimeVisibilityState extends State<_RuntimeVisibility>
    with WidgetsBindingObserver {
  late bool _showUI;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    _showUI =
        widget.initiallyVisible ||
        WidgetsBinding.instance.lifecycleState == AppLifecycleState.resumed;
    widget.onVisibility(_showUI);
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state == AppLifecycleState.resumed && !_showUI) {
      setState(() => _showUI = true);
    }
    widget.onVisibility(state == AppLifecycleState.resumed);
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    widget.onDispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) =>
      _showUI ? widget.child : const SizedBox.shrink();
}
