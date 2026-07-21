import { afterEach, describe, expect, test } from "bun:test";
import {
  __formatUuidV4ForTesting,
  secureRandomUuid,
} from "../src/lib/xmpp/secure-random";

const originalCrypto = Object.getOwnPropertyDescriptor(globalThis, "crypto");

function installCrypto(value: Crypto | undefined): void {
  Object.defineProperty(globalThis, "crypto", {
    configurable: true,
    value,
  });
}

afterEach(() => {
  if (originalCrypto) Object.defineProperty(globalThis, "crypto", originalCrypto);
  else Reflect.deleteProperty(globalThis, "crypto");
});

describe("secureRandomUuid", () => {
  test("uses randomUUID when Web Crypto provides it", () => {
    installCrypto({ randomUUID: () => "11111111-2222-4333-8444-555555555555" } as Crypto);

    expect(secureRandomUuid()).toBe("11111111-2222-4333-8444-555555555555");
  });

  test("uses getRandomValues with RFC 4122 version and variant bits", () => {
    installCrypto({
      getRandomValues: (bytes: Uint8Array) => {
        bytes.fill(0xff);
        return bytes;
      },
    } as Crypto);

    expect(secureRandomUuid()).toBe("ffffffff-ffff-4fff-bfff-ffffffffffff");
  });

  test("formats deterministic random bytes as an RFC 4122 v4 UUID", () => {
    expect(__formatUuidV4ForTesting(new Uint8Array(16))).toBe(
      "00000000-0000-4000-8000-000000000000",
    );
  });

  test("fails closed when Web Crypto is unavailable", () => {
    installCrypto(undefined);

    expect(() => secureRandomUuid()).toThrow(DOMException);
    try {
      secureRandomUuid();
    } catch (error) {
      expect((error as DOMException).name).toBe("NotSupportedError");
    }
  });
});
