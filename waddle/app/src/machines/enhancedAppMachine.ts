import { setup, assign, sendTo, spawnChild } from 'xstate';
import { feedActor } from './actors/feedActor';
import { enhancedChatMachine } from './enhancedChatMachine';
import { connectionManagerActor } from './actors/connectionManagerActor';
import { filterActor } from './actors/filterActor';
import type { 
  User, 
  ViewType, 
  CustomView, 
  Notification, 
  FeedItem,
  ContentItem,
  LayoutMode 
} from '../types/content';

export interface AppContext {
  // User & Authentication
  user: User | null;
  isAuthenticated: boolean;
  
  // Navigation & Views
  currentView: ViewType;
  previousView: ViewType | null;
  activeCustomView: CustomView | null;
  customViews: CustomView[];
  
  // Layout & UI
  currentLayout: LayoutMode;
  sidebarCollapsed: boolean;
  realTimeEnabled: boolean;
  
  // Global Search & Filters
  globalSearchQuery: string;
  universalFilters: {
    timeRange: 'hour' | 'day' | 'week' | 'month' | 'all';
    contentTypes: string[];
    showFollowingOnly: boolean;
  };
  
  // Notifications
  notifications: Notification[];
  unreadNotificationCount: number;
  
  // Performance & Settings
  performanceMode: 'auto' | 'high' | 'low';
  accessibilityMode: boolean;
  keyboardShortcutsEnabled: boolean;
  
  // Connection & Status
  connectionStatus: 'connected' | 'connecting' | 'disconnected';
  lastSyncTime: number;
  
  // Error Handling
  globalError: string | null;
  recoveryAttempts: number;
}

export type AppEvent = 
  // Authentication Events
  | { type: 'LOGIN'; user: User }
  | { type: 'LOGOUT' }
  | { type: 'UPDATE_USER'; updates: Partial<User> }
  
  // Navigation Events
  | { type: 'NAVIGATE_TO'; view: ViewType }
  | { type: 'NAVIGATE_BACK' }
  | { type: 'SWITCH_TO_CUSTOM_VIEW'; view: CustomView }
  | { type: 'SET_LAYOUT'; layout: LayoutMode }
  | { type: 'TOGGLE_SIDEBAR' }
  
  // Search & Filter Events
  | { type: 'GLOBAL_SEARCH'; query: string }
  | { type: 'SET_UNIVERSAL_FILTERS'; filters: Partial<AppContext['universalFilters']> }
  | { type: 'CLEAR_SEARCH' }
  
  // Custom Views Management
  | { type: 'CREATE_CUSTOM_VIEW'; view: CustomView }
  | { type: 'UPDATE_CUSTOM_VIEW'; viewId: string; updates: Partial<CustomView> }
  | { type: 'DELETE_CUSTOM_VIEW'; viewId: string }
  | { type: 'EXPORT_CUSTOM_VIEW'; viewId: string }
  | { type: 'IMPORT_CUSTOM_VIEW'; viewData: string }
  
  // Notifications
  | { type: 'ADD_NOTIFICATION'; notification: Notification }
  | { type: 'MARK_NOTIFICATION_READ'; notificationId: string }
  | { type: 'CLEAR_ALL_NOTIFICATIONS' }
  
  // Real-time & Connection
  | { type: 'TOGGLE_REAL_TIME' }
  | { type: 'CONNECTION_STATUS_CHANGED'; status: 'connected' | 'connecting' | 'disconnected' }
  | { type: 'SYNC_DATA' }
  
  // Content Events (forwarded to appropriate actors)
  | { type: 'CONTENT_ACTION'; action: string; contentId: string; data?: any }
  | { type: 'BULK_CONTENT_ACTION'; action: string; contentIds: string[]; data?: any }
  
  // Settings & Performance
  | { type: 'SET_PERFORMANCE_MODE'; mode: 'auto' | 'high' | 'low' }
  | { type: 'TOGGLE_ACCESSIBILITY_MODE' }
  | { type: 'TOGGLE_KEYBOARD_SHORTCUTS' }
  
  // Keyboard Shortcuts
  | { type: 'KEYBOARD_SHORTCUT'; key: string; modifiers: string[] }
  
  // Error Handling
  | { type: 'GLOBAL_ERROR'; error: string }
  | { type: 'CLEAR_ERROR' }
  | { type: 'RETRY_OPERATION' };

// Keyboard shortcut mapping
const KEYBOARD_SHORTCUTS = {
  'k': { action: 'GLOBAL_SEARCH', description: 'Open search' },
  '1': { action: 'NAVIGATE_TO', data: { view: 'feed' }, description: 'Go to Feed' },
  '2': { action: 'NAVIGATE_TO', data: { view: 'chat' }, description: 'Go to Chat' },
  '3': { action: 'NAVIGATE_TO', data: { view: 'events' }, description: 'Go to Events' },
  '4': { action: 'NAVIGATE_TO', data: { view: 'people' }, description: 'Go to People' },
  '5': { action: 'NAVIGATE_TO', data: { view: 'links' }, description: 'Go to Links' },
  '6': { action: 'NAVIGATE_TO', data: { view: 'hangouts' }, description: 'Go to Hangouts' },
  '7': { action: 'NAVIGATE_TO', data: { view: 'messages' }, description: 'Go to Messages' },
  'l': { action: 'TOGGLE_REAL_TIME', description: 'Toggle live updates' },
  'g': { action: 'SET_LAYOUT', data: { layout: 'grid' }, description: 'Grid layout' },
  'f': { action: 'SET_LAYOUT', data: { layout: 'feed' }, description: 'Feed layout' },
  't': { action: 'SET_LAYOUT', data: { layout: 'timeline' }, description: 'Timeline layout' },
  'Escape': { action: 'CLEAR_SEARCH', description: 'Clear search' },
} as const;

export const enhancedAppMachine = setup({
  types: {
    context: {} as AppContext,
    events: {} as AppEvent,
    input: {} as { 
      user?: User;
      initialView?: ViewType;
      initialLayout?: LayoutMode;
    },
  },
  actors: {
    feedActor,
    chatActor: enhancedChatMachine,
    connectionManagerActor,
    filterActor,
  },
  actions: {
    // Initialization
    initializeFromInput: assign(({ event }) => {
      if (event.type === 'xstate.init') {
        const { user, initialView, initialLayout } = event.input || {};
        return {
          user,
          isAuthenticated: !!user,
          currentView: initialView || user?.preferences.defaultView || 'feed',
          previousView: null,
          activeCustomView: null,
          customViews: user?.preferences.customViews || [],
          currentLayout: initialLayout || 'feed',
          sidebarCollapsed: false,
          realTimeEnabled: false,
          globalSearchQuery: '',
          universalFilters: {
            timeRange: 'day' as const,
            contentTypes: [],
            showFollowingOnly: false,
          },
          notifications: [],
          unreadNotificationCount: 0,
          performanceMode: 'auto' as const,
          accessibilityMode: false,
          keyboardShortcutsEnabled: true,
          connectionStatus: 'disconnected' as const,
          lastSyncTime: 0,
          globalError: null,
          recoveryAttempts: 0,
        };
      }
      return {};
    }),

    // Authentication
    setUser: assign({
      user: ({ event }) => {
        if (event.type === 'LOGIN') return event.user;
        return null;
      },
      isAuthenticated: ({ event }) => event.type === 'LOGIN',
      customViews: ({ event }) => {
        if (event.type === 'LOGIN') return event.user.preferences.customViews || [];
        return [];
      },
    }),

    updateUser: assign({
      user: ({ context, event }) => {
        if (event.type === 'UPDATE_USER' && context.user) {
          return { ...context.user, ...event.updates };
        }
        return context.user;
      },
    }),

    logout: assign({
      user: () => null,
      isAuthenticated: () => false,
      customViews: () => [],
      notifications: () => [],
      unreadNotificationCount: () => 0,
      currentView: () => 'feed',
      activeCustomView: () => null,
    }),

    // Navigation
    navigateToView: assign({
      previousView: ({ context }) => context.currentView,
      currentView: ({ event }) => {
        if (event.type === 'NAVIGATE_TO') return event.view;
        return 'feed';
      },
      activeCustomView: () => null,
    }),

    navigateBack: assign({
      currentView: ({ context }) => context.previousView || 'feed',
      previousView: () => null,
    }),

    switchToCustomView: assign({
      previousView: ({ context }) => context.currentView,
      currentView: () => 'feed',
      activeCustomView: ({ event }) => {
        if (event.type === 'SWITCH_TO_CUSTOM_VIEW') return event.view;
        return null;
      },
    }),

    // Layout & UI
    setLayout: assign({
      currentLayout: ({ event }) => {
        if (event.type === 'SET_LAYOUT') return event.layout;
        return 'feed';
      },
    }),

    toggleSidebar: assign({
      sidebarCollapsed: ({ context }) => !context.sidebarCollapsed,
    }),

    toggleRealTime: assign({
      realTimeEnabled: ({ context }) => !context.realTimeEnabled,
    }),

    // Search & Filters
    setGlobalSearch: assign({
      globalSearchQuery: ({ event }) => {
        if (event.type === 'GLOBAL_SEARCH') return event.query;
        return '';
      },
    }),

    clearSearch: assign({
      globalSearchQuery: () => '',
    }),

    setUniversalFilters: assign({
      universalFilters: ({ context, event }) => {
        if (event.type === 'SET_UNIVERSAL_FILTERS') {
          return { ...context.universalFilters, ...event.filters };
        }
        return context.universalFilters;
      },
    }),

    // Custom Views
    createCustomView: assign({
      customViews: ({ context, event }) => {
        if (event.type === 'CREATE_CUSTOM_VIEW') {
          return [...context.customViews, event.view];
        }
        return context.customViews;
      },
    }),

    updateCustomView: assign({
      customViews: ({ context, event }) => {
        if (event.type === 'UPDATE_CUSTOM_VIEW') {
          return context.customViews.map(view =>
            view.id === event.viewId ? { ...view, ...event.updates } : view
          );
        }
        return context.customViews;
      },
      activeCustomView: ({ context, event }) => {
        if (event.type === 'UPDATE_CUSTOM_VIEW' && 
            context.activeCustomView?.id === event.viewId) {
          return { ...context.activeCustomView, ...event.updates };
        }
        return context.activeCustomView;
      },
    }),

    deleteCustomView: assign({
      customViews: ({ context, event }) => {
        if (event.type === 'DELETE_CUSTOM_VIEW') {
          return context.customViews.filter(view => view.id !== event.viewId);
        }
        return context.customViews;
      },
      activeCustomView: ({ context, event }) => {
        if (event.type === 'DELETE_CUSTOM_VIEW' && 
            context.activeCustomView?.id === event.viewId) {
          return null;
        }
        return context.activeCustomView;
      },
      currentView: ({ context, event }) => {
        if (event.type === 'DELETE_CUSTOM_VIEW' && 
            context.activeCustomView?.id === event.viewId) {
          return 'feed';
        }
        return context.currentView;
      },
    }),

    // Notifications
    addNotification: assign({
      notifications: ({ context, event }) => {
        if (event.type === 'ADD_NOTIFICATION') {
          return [event.notification, ...context.notifications];
        }
        return context.notifications;
      },
      unreadNotificationCount: ({ context, event }) => {
        if (event.type === 'ADD_NOTIFICATION' && !event.notification.isRead) {
          return context.unreadNotificationCount + 1;
        }
        return context.unreadNotificationCount;
      },
    }),

    markNotificationRead: assign({
      notifications: ({ context, event }) => {
        if (event.type === 'MARK_NOTIFICATION_READ') {
          return context.notifications.map(notification =>
            notification.id === event.notificationId
              ? { ...notification, isRead: true }
              : notification
          );
        }
        return context.notifications;
      },
      unreadNotificationCount: ({ context, event }) => {
        if (event.type === 'MARK_NOTIFICATION_READ') {
          const notification = context.notifications.find(n => n.id === event.notificationId);
          if (notification && !notification.isRead) {
            return Math.max(0, context.unreadNotificationCount - 1);
          }
        }
        return context.unreadNotificationCount;
      },
    }),

    clearAllNotifications: assign({
      notifications: () => [],
      unreadNotificationCount: () => 0,
    }),

    // Connection & Sync
    updateConnectionStatus: assign({
      connectionStatus: ({ event }) => {
        if (event.type === 'CONNECTION_STATUS_CHANGED') return event.status;
        return 'disconnected';
      },
    }),

    updateSyncTime: assign({
      lastSyncTime: () => Date.now(),
    }),

    // Settings
    setPerformanceMode: assign({
      performanceMode: ({ event }) => {
        if (event.type === 'SET_PERFORMANCE_MODE') return event.mode;
        return 'auto';
      },
    }),

    toggleAccessibilityMode: assign({
      accessibilityMode: ({ context }) => !context.accessibilityMode,
    }),

    toggleKeyboardShortcuts: assign({
      keyboardShortcutsEnabled: ({ context }) => !context.keyboardShortcutsEnabled,
    }),

    // Error Handling
    setGlobalError: assign({
      globalError: ({ event }) => {
        if (event.type === 'GLOBAL_ERROR') return event.error;
        return null;
      },
      recoveryAttempts: ({ context, event }) => {
        if (event.type === 'GLOBAL_ERROR') return context.recoveryAttempts + 1;
        return context.recoveryAttempts;
      },
    }),

    clearError: assign({
      globalError: () => null,
      recoveryAttempts: () => 0,
    }),

    // Keyboard Shortcut Handler
    handleKeyboardShortcut: ({ context, event, self }) => {
      if (event.type === 'KEYBOARD_SHORTCUT' && context.keyboardShortcutsEnabled) {
        const shortcut = KEYBOARD_SHORTCUTS[event.key as keyof typeof KEYBOARD_SHORTCUTS];
        if (shortcut) {
          const { action, data } = shortcut;
          
          // Convert shortcut action to appropriate event
          switch (action) {
            case 'NAVIGATE_TO':
              self.send({ type: 'NAVIGATE_TO', view: data.view });
              break;
            case 'SET_LAYOUT':
              self.send({ type: 'SET_LAYOUT', layout: data.layout });
              break;
            case 'TOGGLE_REAL_TIME':
              self.send({ type: 'TOGGLE_REAL_TIME' });
              break;
            case 'CLEAR_SEARCH':
              self.send({ type: 'CLEAR_SEARCH' });
              break;
            case 'GLOBAL_SEARCH':
              // Focus search input - handled by UI
              break;
          }
        }
      }
    },

    // Forward events to child actors
    forwardToFeedActor: sendTo('feedActor', ({ event }) => event),
    forwardToChatActor: sendTo('chatActor', ({ event }) => event),
    forwardToConnectionManager: sendTo('connectionManager', ({ event }) => event),
    forwardToFilterActor: sendTo('filterActor', ({ event }) => event),
  },
}).createMachine({
  id: 'enhancedApp',
  initial: 'initializing',
  context: {
    user: null,
    isAuthenticated: false,
    currentView: 'feed',
    previousView: null,
    activeCustomView: null,
    customViews: [],
    currentLayout: 'feed',
    sidebarCollapsed: false,
    realTimeEnabled: false,
    globalSearchQuery: '',
    universalFilters: {
      timeRange: 'day',
      contentTypes: [],
      showFollowingOnly: false,
    },
    notifications: [],
    unreadNotificationCount: 0,
    performanceMode: 'auto',
    accessibilityMode: false,
    keyboardShortcutsEnabled: true,
    connectionStatus: 'disconnected',
    lastSyncTime: 0,
    globalError: null,
    recoveryAttempts: 0,
  },
  states: {
    initializing: {
      entry: 'initializeFromInput',
      always: [
        {
          guard: ({ context }) => context.isAuthenticated,
          target: 'authenticated',
        },
        {
          target: 'unauthenticated',
        }
      ],
    },

    unauthenticated: {
      on: {
        LOGIN: {
          actions: 'setUser',
          target: 'authenticated',
        },
      },
    },

    authenticated: {
      invoke: [
        {
          id: 'feedActor',
          src: 'feedActor',
          input: ({ context }) => ({
            user: context.user!,
            initialView: context.currentView,
          }),
        },
        {
          id: 'chatActor',
          src: 'chatActor',
          input: ({ context }) => ({
            username: context.user!.username,
            userId: context.user!.id,
          }),
        },
        {
          id: 'connectionManager',
          src: 'connectionManagerActor',
          input: ({ context }) => ({
            userId: context.user!.id,
            autoConnect: true,
          }),
        },
        {
          id: 'filterActor',
          src: 'filterActor',
          input: ({ context }) => ({
            userId: context.user!.id,
            initialFilters: context.universalFilters.contentTypes,
          }),
        },
      ],
      
      // Global event handlers
      on: {
        // Authentication
        LOGOUT: {
          actions: 'logout',
          target: 'unauthenticated',
        },
        UPDATE_USER: {
          actions: 'updateUser',
        },

        // Navigation
        NAVIGATE_TO: {
          actions: ['navigateToView', 'forwardToFeedActor'],
        },
        NAVIGATE_BACK: {
          actions: 'navigateBack',
        },
        SWITCH_TO_CUSTOM_VIEW: {
          actions: ['switchToCustomView', 'forwardToFeedActor'],
        },

        // Layout & UI
        SET_LAYOUT: {
          actions: 'setLayout',
        },
        TOGGLE_SIDEBAR: {
          actions: 'toggleSidebar',
        },
        TOGGLE_REAL_TIME: {
          actions: ['toggleRealTime', 'forwardToFeedActor'],
        },

        // Search & Filters
        GLOBAL_SEARCH: {
          actions: ['setGlobalSearch', 'forwardToFeedActor'],
        },
        CLEAR_SEARCH: {
          actions: ['clearSearch', 'forwardToFeedActor'],
        },
        SET_UNIVERSAL_FILTERS: {
          actions: ['setUniversalFilters', 'forwardToFeedActor', 'forwardToFilterActor'],
        },

        // Custom Views
        CREATE_CUSTOM_VIEW: {
          actions: ['createCustomView', 'forwardToFeedActor'],
        },
        UPDATE_CUSTOM_VIEW: {
          actions: ['updateCustomView', 'forwardToFeedActor'],
        },
        DELETE_CUSTOM_VIEW: {
          actions: ['deleteCustomView', 'forwardToFeedActor'],
        },

        // Notifications
        ADD_NOTIFICATION: {
          actions: 'addNotification',
        },
        MARK_NOTIFICATION_READ: {
          actions: 'markNotificationRead',
        },
        CLEAR_ALL_NOTIFICATIONS: {
          actions: 'clearAllNotifications',
        },

        // Connection & Sync
        CONNECTION_STATUS_CHANGED: {
          actions: 'updateConnectionStatus',
        },
        SYNC_DATA: {
          actions: ['updateSyncTime', 'forwardToFeedActor', 'forwardToChatActor'],
        },

        // Settings
        SET_PERFORMANCE_MODE: {
          actions: 'setPerformanceMode',
        },
        TOGGLE_ACCESSIBILITY_MODE: {
          actions: 'toggleAccessibilityMode',
        },
        TOGGLE_KEYBOARD_SHORTCUTS: {
          actions: 'toggleKeyboardShortcuts',
        },

        // Keyboard Shortcuts
        KEYBOARD_SHORTCUT: {
          actions: 'handleKeyboardShortcut',
        },

        // Content Actions (forward to appropriate actors)
        CONTENT_ACTION: [
          {
            guard: ({ context }) => ['feed', 'links', 'events', 'people', 'hangouts'].includes(context.currentView),
            actions: 'forwardToFeedActor',
          },
          {
            guard: ({ context }) => context.currentView === 'chat',
            actions: 'forwardToChatActor',
          },
        ],

        // Error Handling
        GLOBAL_ERROR: {
          actions: 'setGlobalError',
          target: '.error',
        },
      },

      initial: 'active',
      states: {
        active: {
          // Normal operation
        },

        error: {
          on: {
            CLEAR_ERROR: {
              actions: 'clearError',
              target: 'active',
            },
            RETRY_OPERATION: {
              actions: ['clearError', 'updateSyncTime'],
              target: 'active',
            },
          },
        },
      },
    },
  },
});

// Utility functions for working with the app machine
export const createAppMachineInput = (
  user?: User,
  initialView?: ViewType,
  initialLayout?: LayoutMode
) => ({
  user,
  initialView,
  initialLayout,
});

export const getKeyboardShortcuts = () => KEYBOARD_SHORTCUTS;

export const generateNotificationId = () => 
  `notification_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`;

export const createNotification = (
  type: Notification['type'],
  title: string,
  message: string,
  options: Partial<Notification> = {}
): Notification => ({
  id: generateNotificationId(),
  userId: 'current-user', // This would come from context in real implementation
  type,
  title,
  message,
  timestamp: Date.now(),
  isRead: false,
  ...options,
});

// Export action types for use in other parts of the app
export const APP_ACTIONS = {
  NAVIGATE_TO: 'NAVIGATE_TO',
  SWITCH_TO_CUSTOM_VIEW: 'SWITCH_TO_CUSTOM_VIEW',
  SET_LAYOUT: 'SET_LAYOUT',
  TOGGLE_REAL_TIME: 'TOGGLE_REAL_TIME',
  GLOBAL_SEARCH: 'GLOBAL_SEARCH',
  CREATE_CUSTOM_VIEW: 'CREATE_CUSTOM_VIEW',
  ADD_NOTIFICATION: 'ADD_NOTIFICATION',
} as const;