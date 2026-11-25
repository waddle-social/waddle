import 'dart:io';

import 'package:connectivity_plus_platform_interface/connectivity_plus_platform_interface.dart';

/// LiveKit checks connectivity before opening its signaling socket. On Linux
/// the default provider requires NetworkManager, which is optional on Wayland.
Future<void> prepareCallConnectivity() async {
  if (!Platform.isLinux ||
      ConnectivityPlatform.instance is _InterfaceConnectivity) {
    return;
  }
  try {
    await ConnectivityPlatform.instance.checkConnectivity();
  } on Exception {
    ConnectivityPlatform.instance = _InterfaceConnectivity();
  }
}

class _InterfaceConnectivity extends ConnectivityPlatform {
  @override
  Future<List<ConnectivityResult>> checkConnectivity() async {
    final interfaces = await NetworkInterface.list();
    return [
      interfaces.isEmpty ? ConnectivityResult.none : ConnectivityResult.other,
    ];
  }

  @override
  Stream<List<ConnectivityResult>> get onConnectivityChanged =>
      Stream<void>.periodic(const Duration(seconds: 3))
          .asyncMap((_) => checkConnectivity())
          .distinct((a, b) => a.first == b.first);
}
