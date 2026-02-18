import { createRouter, createWebHistory, type RouteRecordRaw } from 'vue-router';

import ChatView from '../views/ChatView.vue';
import ConversationListView from '../views/ConversationListView.vue';
import LoginView from '../views/LoginView.vue';
import PluginView from '../views/PluginView.vue';
import RoomsView from '../views/RoomsView.vue';
import RosterView from '../views/RosterView.vue';
import SettingsView from '../views/SettingsView.vue';

const routes: RouteRecordRaw[] = [
  {
    path: '/login',
    name: 'login',
    component: LoginView,
    meta: { public: true },
  },
  {
    path: '/',
    name: 'conversations',
    component: ConversationListView,
  },
  {
    path: '/chat/:jid',
    name: 'chat',
    component: ChatView,
    props: true,
  },
  {
    path: '/roster',
    name: 'roster',
    component: RosterView,
  },
  {
    path: '/rooms',
    name: 'rooms',
    component: RoomsView,
  },
  {
    path: '/settings',
    name: 'settings',
    component: SettingsView,
  },
  {
    path: '/plugin/:id',
    name: 'plugin',
    component: PluginView,
    props: true,
  },
];

export const router = createRouter({
  history: createWebHistory(),
  routes,
});

/*
 * Auth guard — lazy-import the auth store to avoid circular dependency
 * with Pinia (store not yet created when this module first loads).
 */
router.beforeEach(async (to) => {
  if (to.meta.public) return true;

  const { useAuthStore } = await import('../stores/auth');
  const auth = useAuthStore();

  if (!auth.isAuthenticated) {
    return { name: 'login' };
  }

  return true;
});
