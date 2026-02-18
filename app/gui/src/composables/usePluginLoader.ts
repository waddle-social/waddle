import { defineAsyncComponent, type Component } from 'vue';

import { useWaddle, type PluginInfo } from './useWaddle';

const componentCache = new Map<string, Component>();

function pluginComponentUrl(pluginId: string, componentName: string): string {
  return `/plugins/${pluginId}/vue/${componentName}`;
}

export function loadPluginComponent(pluginId: string, componentName: string): Component {
  const cacheKey = `${pluginId}/${componentName}`;

  const cached = componentCache.get(cacheKey);
  if (cached) {
    return cached;
  }

  const component = defineAsyncComponent({
    loader: () => import(/* @vite-ignore */ pluginComponentUrl(pluginId, componentName)),
    timeout: 10_000,
  });

  componentCache.set(cacheKey, component);
  return component;
}

export function clearPluginComponent(pluginId: string): void {
  for (const key of componentCache.keys()) {
    if (key.startsWith(`${pluginId}/`)) {
      componentCache.delete(key);
    }
  }
}

export async function getPluginInfo(pluginId: string): Promise<PluginInfo> {
  const waddle = useWaddle();
  return waddle.managePlugins({ action: 'get', pluginId });
}
