import 'package:flutter/widgets.dart';

/// Reports popup coverage independently of page navigation.
abstract interface class PopupCoverageAware {
  void didChangePopupCoverage(bool covered);
}

/// Reports page and popup coverage without reopening pages for overlays.
class PageCoverageObserver extends NavigatorObserver {
  final _routes = <Route<dynamic>>[];
  final _listeners = <Route<dynamic>, Set<RouteAware>>{};
  final _popupCoverage = <Route<dynamic>, bool>{};

  Route<dynamic>? get _visiblePage {
    for (final route in _routes.reversed) {
      if (route is PageRoute<dynamic>) return route;
    }
    return null;
  }

  bool _hasPopupAbove(Route<dynamic> route) {
    final index = _routes.indexOf(route);
    return index >= 0 &&
        _routes.skip(index + 1).any((route) => route is PopupRoute<dynamic>);
  }

  void subscribe(RouteAware listener, ModalRoute<void> route) {
    final listeners = _listeners.putIfAbsent(route, () => <RouteAware>{});
    if (listeners.add(listener)) {
      listener.didPush();
      if (_routes.contains(route) && _visiblePage != route) {
        listener.didPushNext();
      }
      final covered = _hasPopupAbove(route);
      _popupCoverage[route] = covered;
      if (listener is PopupCoverageAware) {
        (listener as PopupCoverageAware).didChangePopupCoverage(covered);
      }
    }
  }

  void unsubscribe(RouteAware listener) {
    for (final listeners in _listeners.values) {
      listeners.remove(listener);
    }
    _listeners.removeWhere((_, listeners) => listeners.isEmpty);
    _popupCoverage.removeWhere((route, _) => !_listeners.containsKey(route));
  }

  void _updatePopupCoverage() {
    for (final entry in List.of(_listeners.entries)) {
      final covered = _hasPopupAbove(entry.key);
      if (_popupCoverage[entry.key] == covered) continue;
      _popupCoverage[entry.key] = covered;
      for (final listener in List<RouteAware>.of(entry.value)) {
        if (listener is PopupCoverageAware) {
          (listener as PopupCoverageAware).didChangePopupCoverage(covered);
        }
      }
    }
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
      _popupCoverage.clear();
    }
    final before = _visiblePage;
    _routes.add(route);
    if (_visiblePage != before) _covered(before);
    _updatePopupCoverage();
  }

  @override
  void didPop(Route<dynamic> route, Route<dynamic>? previousRoute) {
    final before = _visiblePage;
    _routes.remove(route);
    final after = _visiblePage;
    if (before != after) _uncovered(after);
    _listeners.remove(route);
    _popupCoverage.remove(route);
    _updatePopupCoverage();
  }

  @override
  void didRemove(Route<dynamic> route, Route<dynamic>? previousRoute) {
    final before = _visiblePage;
    _routes.remove(route);
    final after = _visiblePage;
    if (before != after) _uncovered(after);
    _listeners.remove(route);
    _popupCoverage.remove(route);
    _updatePopupCoverage();
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
    if (oldRoute != null) {
      _listeners.remove(oldRoute);
      _popupCoverage.remove(oldRoute);
    }
    _updatePopupCoverage();
  }
}
