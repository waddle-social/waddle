import { createApp } from "vue";
import ProviderHarness from "./ProviderHarness.vue";
import type { ProviderBrowserFixture } from "./provider-fixture-types";

const host = document.querySelector<HTMLElement>("[data-provider-host]");
if (!host) throw new Error("provider fixture host is missing");

const app = createApp(ProviderHarness);
app.mount(host);
const fixture: ProviderBrowserFixture = window.__waddleProviderFixture;
fixture.unmount = () => app.unmount();
