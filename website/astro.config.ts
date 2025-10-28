import { defineConfig } from 'astro/config';
import tailwindcss from '@tailwindcss/vite';
import vue from '@astrojs/vue';

export default defineConfig({
  output: 'static',
  integrations: [vue()],
  vite: {
    plugins: [tailwindcss()],
  },
  devToolbar: {
    enabled: false
  }
});
