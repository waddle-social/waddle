/**
 * Shared no-op stubs for the handler-registration surface of
 * `BrowserXmppClient`. `useChannelMessages` and `ChatApp` register handlers inside
 * watchers that now run on mount (`{ immediate: true }`), so any mock client
 * passed to those composables must at minimum accept every `set*Handler` call
 * without throwing. Tests that care about a specific handler override the
 * relevant stub on top of this.
 */
export function handlerStubs() {
  return {
    setMessageHandler: () => {},
    setChatStateHandler: () => {},
    setReactionHandler: () => {},
    setDisplayedHandler: () => {},
    setHatsHandler: () => {},
    setAuthorityHandler: () => {},
    setPresenceHandler: () => {},
    setLastSeenHandler: () => {},
    setActivityHandler: () => {},
    setRoomAvatarHandler: () => {},
    listRoomAccessRequirements: () => [],
    onRoomAccessChanged: () => () => {},
  };
}
