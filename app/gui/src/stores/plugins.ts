import { computed, ref } from 'vue';
import { defineStore } from 'pinia';

export interface PluginRecord {
  id: string;
  name: string;
  version: string;
  status: 'active' | 'disabled' | 'error';
}

export const usePluginsStore = defineStore('plugins', () => {
  const plugins = ref<PluginRecord[]>([
    {
      id: 'org.waddle.omemo',
      name: 'OMEMO',
      version: '0.1.0',
      status: 'active',
    },
    {
      id: 'org.waddle.translate',
      name: 'Auto Translate',
      version: '0.2.0',
      status: 'disabled',
    },
  ]);

  const activePlugins = computed(() => plugins.value.filter((plugin) => plugin.status === 'active'));

  function getById(id: string): PluginRecord | undefined {
    return plugins.value.find((plugin) => plugin.id === id);
  }

  return {
    plugins,
    activePlugins,
    getById,
  };
});
