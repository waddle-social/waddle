import { expect, test, type Page } from "@playwright/test";
import type { ProviderFixtureSnapshot } from "./fixture/src/provider-fixture-types";

async function snapshot(page: Page): Promise<ProviderFixtureSnapshot> {
  return page.evaluate(() => window.__waddleProviderFixture.snapshot());
}

test("the mounted provider publishes auth and disposes each client exactly once", async ({
  page,
}) => {
  await page.goto("/provider");
  await expect(page.getByTestId("xmpp-provider-fixture")).toBeVisible();
  await expect.poll(async () => (await snapshot(page)).activeClientId).toBe(
    "bootstrap-session",
  );

  expect(await snapshot(page)).toMatchObject({
    appState: "ready",
    sessionId: "bootstrap-session",
    appError: "bootstrap:1",
    activeServerUrl: "https://bootstrap.example.com",
    providerIds: ["bootstrap-provider-1"],
    disposeCalls: { "bootstrap-session": 0 },
  });

  await page.evaluate(() => window.__waddleProviderFixture.login());
  expect(await snapshot(page)).toMatchObject({
    appState: "ready",
    sessionId: "login-session",
    appError: "selected-provider",
    activeServerUrl: "https://login.example.com",
    providerIds: ["login-provider"],
    activeClientId: "bootstrap-session",
  });

  await page.evaluate(() => window.__waddleProviderFixture.logout());
  expect(await snapshot(page)).toMatchObject({
    appState: "signed-out",
    sessionId: null,
    activeClientId: null,
    disposeCalls: { "bootstrap-session": 1 },
  });

  await page.evaluate(() => window.__waddleProviderFixture.bootstrap());
  expect(await snapshot(page)).toMatchObject({
    appState: "ready",
    sessionId: "second-bootstrap-session",
    appError: "bootstrap:2",
    providerIds: ["bootstrap-provider-2"],
    activeClientId: "second-bootstrap-session",
    disposeCalls: {
      "bootstrap-session": 1,
      "second-bootstrap-session": 0,
    },
  });

  await page.evaluate(() => {
    window.__waddleProviderFixture.unmount();
    window.__waddleProviderFixture.unmount();
  });
  await expect.poll(async () => (
    await snapshot(page)
  ).disposeCalls["second-bootstrap-session"]).toBe(1);

  const terminal = await snapshot(page);
  expect(terminal.activeClientId).toBeNull();
  expect(terminal.disposeCalls).toEqual({
    "bootstrap-session": 1,
    "second-bootstrap-session": 1,
  });
  expect(terminal.events).toEqual([
    "auth:bootstrap:1",
    "set:null",
    "create:bootstrap-session",
    "instrument:bootstrap-session",
    "status-handler:bootstrap-session",
    "set:bootstrap-session",
    "auth:login",
    "set:null",
    "dispose:bootstrap-session",
    "auth:logout",
    "auth:bootstrap:2",
    "set:null",
    "create:second-bootstrap-session",
    "instrument:second-bootstrap-session",
    "status-handler:second-bootstrap-session",
    "set:second-bootstrap-session",
    "set:null",
    "dispose:second-bootstrap-session",
  ]);
});
