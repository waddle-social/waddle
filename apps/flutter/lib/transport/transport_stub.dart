import 'transport.dart';

Future<RustTransport> createTransport() => Future.error(
  const RustTransportException(
    'unsupported_platform',
    'This platform cannot load the Waddle Rust client.',
  ),
);
