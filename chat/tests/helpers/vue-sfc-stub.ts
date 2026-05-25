import { h } from "vue";

export default {
  name: "VueSfcTestStub",
  setup(_: unknown, { slots }: { slots: { default?: () => unknown } }) {
    return () => h("span", { "data-vue-stub": "true" }, slots.default?.());
  },
};
