import { setup, assign } from 'xstate';

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
}

export type FilterEvent = 
  | { type: 'TOGGLE_FILTER'; category: Category }
  | { type: 'SHOW_ALL' }
  | { type: 'CLEAR_ALL' };

export const filterMachine = setup({
  types: {
    context: {} as FilterContext,
    events: {} as FilterEvent,
  },
  actions: {
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
    }),
    showAll: assign({
      activeFilters: () => new Set(),
      showAll: () => true,
    }),
    clearAll: assign({
      activeFilters: () => new Set(CATEGORIES),
      showAll: () => false,
    }),
  },
}).createMachine({
  id: 'filter',
  initial: 'active',
  context: {
    activeFilters: new Set(),
    showAll: true,
  },
  states: {
    active: {
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
      },
    },
  },
});