import { expect, test } from "@playwright/test";
import type { DurableBrowserResult } from "./fixture/src/durable-main";

test("the real browser commits and serializes durable runtime transactions", async ({
  page,
}) => {
  await page.goto("/durable");
  await expect(page.getByTestId("durable-runtime-fixture")).toHaveText("ready");

  const result = await page.evaluate(async () => (
    window.__waddleDurableFixture.commitRace()
  )) as DurableBrowserResult;

  expect(result).toEqual({
    outcomes: ["existing", "inserted"],
    ids: ["browser-message"],
    revisions: [1, 1],
  });
});
