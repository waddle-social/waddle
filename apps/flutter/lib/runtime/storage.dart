/// All values may contain account-private data. Production adapters use the
/// platform credential store rather than preferences or plaintext files.
abstract interface class RuntimeStorage {
  Future<String?> read(String key);
  Future<void> write(String key, String? value);
}

/// Enumerates independently stored account keys without exposing their values.
abstract interface class AccountKeyStorage implements RuntimeStorage {
  Future<Iterable<String>> keysWithPrefix(String prefix);
}
