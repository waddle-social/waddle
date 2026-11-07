import 'package:flutter/widgets.dart';

/// Reports page coverage without treating menus and sheets as navigation away.
class PageCoverageObserver extends NavigatorObserver {
  final _routes = <Route<dynamic>>[];
  final _listeners = <Route<dynamic>, Set<RouteAware>>{};

  Route<dynamic>? get _visiblePage {
    for (final route in _routes.reversed) {
      if (route is PageRoute<dynamic>) return route;
    }
    return null;
  }

  void subscribe(RouteAware listener, ModalRoute<void> route) {
    final listeners = _listeners.putIfAbsent(route, () => <RouteAware>{});
    if (listeners.add(listener)) {
      listener.didPush();
      if (_routes.contains(route) && _visiblePage != route) {
        listener.didPushNext();
      }
    }
  }

  void unsubscribe(RouteAware listener) {
    for (final listeners in _listeners.values) {
      listeners.remove(listener);
    }
    _listeners.removeWhere((_, listeners) => listeners.isEmpty);
  }

  void _covered(Route<dynamic>? route) {
    for (final listener in List<RouteAware>.of(_listeners[route] ?? const {})) {
      listener.didPushNext();
    }
  }

  void _uncovered(Route<dynamic>? route) {
    for (final listener in List<RouteAware>.of(_listeners[route] ?? const {})) {
      listener.didPopNext();
    }
  }

  @override
  void didPush(Route<dynamic> route, Route<dynamic>? previousRoute) {
    if (previousRoute == null) {
      _routes.clear();
      _listeners.clear();
    }
    final before = _visiblePage;
    _routes.add(route);
    if (_visiblePage != before) _covered(before);
  }

  @override
  void didPop(Route<dynamic> route, Route<dynamic>? previousRoute) {
    final before = _visiblePage;
    _routes.remove(route);
    final after = _visiblePage;
    if (before != after) _uncovered(after);
    _listeners.remove(route);
  }

  @override
  void didRemove(Route<dynamic> route, Route<dynamic>? previousRoute) {
    final before = _visiblePage;
    _routes.remove(route);
    final after = _visiblePage;
    if (before != after) _uncovered(after);
    _listeners.remove(route);
  }

  @override
  void didReplace({Route<dynamic>? newRoute, Route<dynamic>? oldRoute}) {
    final before = _visiblePage;
    final index = oldRoute == null ? -1 : _routes.indexOf(oldRoute);
    if (index >= 0) {
      if (newRoute == null) {
        _routes.removeAt(index);
      } else {
        _routes[index] = newRoute;
      }
    }
    final after = _visiblePage;
    if (before != after) {
      _covered(before);
      _uncovered(after);
    }
    if (oldRoute != null) _listeners.remove(oldRoute);
  }
}
