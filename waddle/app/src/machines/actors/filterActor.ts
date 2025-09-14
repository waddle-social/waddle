import { setup, assign, fromCallback } from 'xstate';

export const CATEGORIES = [
  'Support',
  'Help Wanted',
  'Videos', 
  'Music',
  'Movies',
  'General',
  'Tech',
  'Gaming',
] as const;

export type Category = typeof CATEGORIES[number];

export interface FilterContext {
  activeFilters: Set<Category>;
  showAll: boolean;
  userId?: string;
  persistenceEnabled: boolean;
  lastSyncTime: number;
}

export type FilterEvent = 
  | { type: 'TOGGLE_FILTER'; category: Category }
  | { type: 'SHOW_ALL' }
  | { type: 'CLEAR_ALL' }
  | { type: 'SYNC_FROM_STORAGE'; filters: Set<Category>; showAll: boolean }
  | { type: 'ENABLE_PERSISTENCE'; userId: string }
  | { type: 'DISABLE_PERSISTENCE' }
  | { type: 'PERSISTENCE_SUCCESS' }
  | { type: 'PERSISTENCE_ERROR'; error: string };

// Storage utility functions
const STORAGE_KEY = 'waddle_filters';

const saveToStorage = (userId: string, activeFilters: Set<Category>, showAll: boolean) => {
  try {
    const data = {
      userId,
      activeFilters: Array.from(activeFilters),
      showAll,
      timestamp: Date.now(),
    };
    localStorage.setItem(`${STORAGE_KEY}_${userId}`, JSON.stringify(data));
    return true;
  } catch (error) {
    console.warn('Failed to save filters to storage:', error);
    return false;
  }
};

const loadFromStorage = (userId: string) => {
  try {
    const stored = localStorage.getItem(`${STORAGE_KEY}_${userId}`);
    if (!stored) return null;
    
    const data = JSON.parse(stored);
    return {
      activeFilters: new Set(data.activeFilters) as Set<Category>,
      showAll: data.showAll,
      timestamp: data.timestamp,
    };
  } catch (error) {
    console.warn('Failed to load filters from storage:', error);
    return null;
  }
};

// Persistence callback - saves state changes to localStorage
const persistenceCallback = fromCallback(({ sendBack, receive, input }) => {
  const { userId, activeFilters, showAll } = input as {
    userId: string;
    activeFilters: Set<Category>;
    showAll: boolean;
  };

  // Debounce saves to avoid excessive writes
  let timeoutId: NodeJS.Timeout;
  
  receive((event) => {
    if (event.type === 'SAVE') {
      clearTimeout(timeoutId);
      timeoutId = setTimeout(() => {
        const success = saveToStorage(userId, activeFilters, showAll);
        if (success) {
          sendBack({ type: 'PERSISTENCE_SUCCESS' });
        } else {
          sendBack({ type: 'PERSISTENCE_ERROR', error: 'Failed to save to localStorage' });
        }
      }, 300); // 300ms debounce
    }
  });

  return () => {
    clearTimeout(timeoutId);
  };
});

export const filterActor = setup({
  types: {
    context: {} as FilterContext,
    events: {} as FilterEvent,
    input: {} as { userId?: string; initialFilters?: Set<Category>; initialShowAll?: boolean },
  },
  actors: {
    persistenceCallback,
  },
  actions: {
    initializeFromInput: assign(({ context, event }) => {
      if (event.type === 'xstate.init') {
        const { userId, initialFilters, initialShowAll } = event.input || {};
        
        // Try to load from storage if userId provided
        if (userId) {
          const stored = loadFromStorage(userId);
          if (stored) {
            return {
              ...context,
              userId,
              activeFilters: stored.activeFilters,
              showAll: stored.showAll,
              persistenceEnabled: true,
              lastSyncTime: stored.timestamp,
            };
          }
        }
        
        // Use provided initial values or defaults
        return {
          ...context,
          userId,
          activeFilters: initialFilters || new Set(),
          showAll: initialShowAll ?? true,
          persistenceEnabled: !!userId,
          lastSyncTime: Date.now(),
        };
      }
      return context;
    }),

    toggleFilter: assign({
      activeFilters: ({ context, event }) => {
        if (event.type === 'TOGGLE_FILTER') {
          const newFilters = new Set(context.activeFilters);
          if (newFilters.has(event.category)) {
            newFilters.delete(event.category);
          } else {
            newFilters.add(event.category);
          }
          return newFilters;
        }
        return context.activeFilters;
      },
      showAll: ({ context, event }) => {
        if (event.type === 'TOGGLE_FILTER') {
          const newFilters = new Set(context.activeFilters);
          if (newFilters.has(event.category)) {
            newFilters.delete(event.category);
          } else {
            newFilters.add(event.category);
          }
          return newFilters.size === 0;
        }
        return context.showAll;
      },
      lastSyncTime: () => Date.now(),
    }),

    showAll: assign({
      activeFilters: () => new Set(),
      showAll: () => true,
      lastSyncTime: () => Date.now(),
    }),

    clearAll: assign({
      activeFilters: () => new Set(CATEGORIES),
      showAll: () => false,
      lastSyncTime: () => Date.now(),
    }),

    syncFromStorage: assign({
      activeFilters: ({ context, event }) => {
        if (event.type === 'SYNC_FROM_STORAGE') {
          return event.filters;
        }
        return context.activeFilters;
      },
      showAll: ({ context, event }) => {
        if (event.type === 'SYNC_FROM_STORAGE') {
          return event.showAll;
        }
        return context.showAll;
      },
      lastSyncTime: () => Date.now(),
    }),

    enablePersistence: assign({
      userId: ({ context, event }) => {
        if (event.type === 'ENABLE_PERSISTENCE') {
          return event.userId;
        }
        return context.userId;
      },
      persistenceEnabled: () => true,
    }),

    disablePersistence: assign({
      userId: () => undefined,
      persistenceEnabled: () => false,
    }),

    triggerPersistence: ({ context }) => {
      // This action will be used to trigger persistence after state changes
    },
  },
}).createMachine({
  id: 'filterActor',
  initial: 'initializing',
  context: {
    activeFilters: new Set(),
    showAll: true,
    userId: undefined,
    persistenceEnabled: false,
    lastSyncTime: 0,
  },
  states: {
    initializing: {
      entry: 'initializeFromInput',
      always: [
        {
          guard: ({ context }) => context.persistenceEnabled && !!context.userId,
          target: 'active.withPersistence',
        },
        {
          target: 'active.withoutPersistence',
        },
      ],
    },

    active: {
      initial: 'withoutPersistence',
      states: {
        withoutPersistence: {
          on: {
            TOGGLE_FILTER: {
              actions: 'toggleFilter',
            },
            SHOW_ALL: {
              actions: 'showAll',
            },
            CLEAR_ALL: {
              actions: 'clearAll',
            },
            ENABLE_PERSISTENCE: {
              actions: 'enablePersistence',
              target: 'withPersistence',
            },
            SYNC_FROM_STORAGE: {
              actions: 'syncFromStorage',
            },
          },
        },

        withPersistence: {
          invoke: {
            id: 'persistence',
            src: 'persistenceCallback',
            input: ({ context }) => ({
              userId: context.userId!,
              activeFilters: context.activeFilters,
              showAll: context.showAll,
            }),
          },
          on: {
            TOGGLE_FILTER: {
              actions: ['toggleFilter', 'triggerPersistence'],
              // Send save event to persistence callback
              entry: ({ self }) => {
                self.system.get('persistence')?.send({ type: 'SAVE' });
              },
            },
            SHOW_ALL: {
              actions: ['showAll', 'triggerPersistence'],
              entry: ({ self }) => {
                self.system.get('persistence')?.send({ type: 'SAVE' });
              },
            },
            CLEAR_ALL: {
              actions: ['clearAll', 'triggerPersistence'],
              entry: ({ self }) => {
                self.system.get('persistence')?.send({ type: 'SAVE' });
              },
            },
            DISABLE_PERSISTENCE: {
              actions: 'disablePersistence',
              target: 'withoutPersistence',
            },
            SYNC_FROM_STORAGE: {
              actions: 'syncFromStorage',
            },
            PERSISTENCE_SUCCESS: {
              // Handle successful persistence if needed
            },
            PERSISTENCE_ERROR: {
              // Handle persistence errors - could retry or show notification
            },
          },
        },
      },

      // Common events for all substates
      on: {
        ENABLE_PERSISTENCE: {
          actions: 'enablePersistence',
          target: '.withPersistence',
        },
        DISABLE_PERSISTENCE: {
          actions: 'disablePersistence',
          target: '.withoutPersistence',
        },
      },
    },
  },
});

// Utility functions for working with the filter actor
export const createFilterActorInput = (
  userId?: string,
  initialFilters?: Category[],
  initialShowAll?: boolean
) => ({
  userId,
  initialFilters: initialFilters ? new Set(initialFilters) : new Set(),
  initialShowAll: initialShowAll ?? true,
});

export const getActiveFilterCount = (context: FilterContext): number => {
  return context.showAll ? 0 : context.activeFilters.size;
};

export const isFilterActive = (context: FilterContext, category: Category): boolean => {
  return context.showAll || context.activeFilters.has(category);
};

export const shouldShowMessage = (
  context: FilterContext,
  messageCategory: Category
): boolean => {
  return context.showAll || context.activeFilters.has(messageCategory);
};