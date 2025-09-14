import { setup, assign, fromCallback, sendTo } from 'xstate';
import type { 
  ContentItem, 
  FeedItem, 
  CustomView, 
  ViewType, 
  ContentType, 
  User,
  ContentMetrics 
} from '../../types/content';

export interface FeedContext {
  currentView: ViewType;
  customViews: CustomView[];
  activeCustomView?: CustomView;
  feedItems: FeedItem[];
  loading: boolean;
  error: string | null;
  hasMore: boolean;
  lastFetchTime: number;
  realTimeUpdates: boolean;
  user: User | null;
  filters: {
    contentTypes: ContentType[];
    timeRange: 'hour' | 'day' | 'week' | 'month' | 'all';
    sortBy: 'timestamp' | 'trending' | 'relevance';
    showFollowingOnly: boolean;
  };
  searchQuery: string;
  selectedTags: string[];
  bookmarkedItems: Set<string>;
  pinnedItems: Set<string>;
}

export type FeedEvent = 
  | { type: 'SWITCH_VIEW'; viewType: ViewType }
  | { type: 'SWITCH_TO_CUSTOM_VIEW'; customView: CustomView }
  | { type: 'LOAD_FEED' }
  | { type: 'LOAD_MORE' }
  | { type: 'REFRESH' }
  | { type: 'REAL_TIME_UPDATE'; items: FeedItem[] }
  | { type: 'CONTENT_UPDATED'; item: ContentItem }
  | { type: 'CONTENT_DELETED'; itemId: string }
  | { type: 'SET_FILTERS'; filters: Partial<FeedContext['filters']> }
  | { type: 'SEARCH'; query: string }
  | { type: 'TOGGLE_BOOKMARK'; itemId: string }
  | { type: 'TOGGLE_PIN'; itemId: string }
  | { type: 'REACT_TO_CONTENT'; itemId: string; reaction: string }
  | { type: 'CREATE_CUSTOM_VIEW'; view: CustomView }
  | { type: 'UPDATE_CUSTOM_VIEW'; viewId: string; updates: Partial<CustomView> }
  | { type: 'DELETE_CUSTOM_VIEW'; viewId: string }
  | { type: 'FEED_LOADED'; items: FeedItem[]; hasMore: boolean }
  | { type: 'FEED_ERROR'; error: string }
  | { type: 'ENABLE_REAL_TIME' }
  | { type: 'DISABLE_REAL_TIME' };

// Mock API functions - replace with real API calls
const mockFeedAPI = {
  async fetchFeed(
    view: ViewType | CustomView,
    filters: FeedContext['filters'],
    searchQuery: string,
    offset: number = 0,
    limit: number = 20
  ): Promise<{ items: FeedItem[]; hasMore: boolean }> {
    // Simulate API call
    await new Promise(resolve => setTimeout(resolve, 1000));
    
    // Generate mock feed items based on view and filters
    const mockItems: FeedItem[] = [];
    
    for (let i = 0; i < limit; i++) {
      const contentTypes: ContentType[] = typeof view === 'string' 
        ? view === 'feed' ? ['chat', 'event', 'link'] : [view as ContentType]
        : view.contentTypes;
      
      const randomType = contentTypes[Math.floor(Math.random() * contentTypes.length)];
      
      mockItems.push({
        content: {
          id: `${randomType}_${offset + i}_${Date.now()}`,
          type: randomType,
          userId: `user_${Math.floor(Math.random() * 100)}`,
          username: `User${Math.floor(Math.random() * 100)}`,
          timestamp: Date.now() - Math.random() * 86400000, // Random within last day
          visibility: 'public',
          ...(randomType === 'chat' && {
            content: `This is a mock ${randomType} message ${offset + i + 1}`,
          }),
          ...(randomType === 'event' && {
            title: `Event ${offset + i + 1}`,
            description: `This is a mock event description`,
            startTime: Date.now() + Math.random() * 86400000,
            endTime: Date.now() + Math.random() * 86400000 + 3600000,
            rsvpStatus: 'none' as const,
            attendeeCount: Math.floor(Math.random() * 100),
          }),
          ...(randomType === 'link' && {
            title: `Interesting Link ${offset + i + 1}`,
            url: `https://example.com/link${offset + i + 1}`,
            domain: 'example.com',
            votes: Math.floor(Math.random() * 1000),
            commentCount: Math.floor(Math.random() * 50),
          }),
        } as ContentItem,
        score: Math.random(),
        isTrending: Math.random() > 0.8,
      });
    }
    
    return {
      items: mockItems,
      hasMore: offset + limit < 100, // Mock pagination
    };
  },

  async reactToContent(itemId: string, reaction: string): Promise<void> {
    await new Promise(resolve => setTimeout(resolve, 300));
  },

  async bookmarkContent(itemId: string, bookmarked: boolean): Promise<void> {
    await new Promise(resolve => setTimeout(resolve, 300));
  },

  async pinContent(itemId: string, pinned: boolean): Promise<void> {
    await new Promise(resolve => setTimeout(resolve, 300));
  },
};

// Real-time updates callback
const realTimeCallback = fromCallback(({ sendBack, receive }) => {
  // Mock WebSocket connection for real-time updates
  const interval = setInterval(() => {
    if (Math.random() > 0.7) { // 30% chance of update every 5 seconds
      const mockUpdate: FeedItem = {
        content: {
          id: `realtime_${Date.now()}`,
          type: 'chat',
          userId: 'realtime_user',
          username: 'LiveUser',
          timestamp: Date.now(),
          visibility: 'public',
          content: `Live update: ${new Date().toLocaleTimeString()}`,
        } as ContentItem,
        score: 1,
        isPromoted: true,
      };
      
      sendBack({ type: 'REAL_TIME_UPDATE', items: [mockUpdate] });
    }
  }, 5000);

  receive((event) => {
    if (event.type === 'STOP') {
      clearInterval(interval);
    }
  });

  return () => {
    clearInterval(interval);
  };
});

// Feed scoring and ranking algorithm
const calculateFeedScore = (item: ContentItem, metrics?: ContentMetrics): number => {
  const age = Date.now() - item.timestamp;
  const ageHours = age / (1000 * 60 * 60);
  
  let score = 1;
  
  // Time decay
  score *= Math.exp(-ageHours / 24); // Decay over 24 hours
  
  // Engagement boost
  if (metrics) {
    score += (metrics.views / 1000) * 0.1;
    score += (metrics.engagement / 100) * 0.2;
    score += metrics.shareCount * 0.3;
    score += metrics.trendingScore * 0.4;
  }
  
  // Content type weights
  const typeWeights = {
    chat: 1.0,
    event: 1.2,
    link: 0.9,
    person: 0.8,
    hangout: 1.1,
    message: 0.7,
  };
  
  score *= typeWeights[item.type] || 1.0;
  
  return Math.max(0, score);
};

export const feedActor = setup({
  types: {
    context: {} as FeedContext,
    events: {} as FeedEvent,
    input: {} as { user: User; initialView?: ViewType },
  },
  actors: {
    realTimeCallback,
  },
  actions: {
    initializeFromInput: assign(({ event }) => {
      if (event.type === 'xstate.init') {
        const { user, initialView } = event.input || {};
        return {
          user,
          currentView: initialView || user?.preferences.defaultView || 'feed',
          customViews: user?.preferences.customViews || [],
          feedItems: [],
          loading: false,
          error: null,
          hasMore: true,
          lastFetchTime: 0,
          realTimeUpdates: false,
          filters: {
            contentTypes: ['chat', 'event', 'link'],
            timeRange: 'day' as const,
            sortBy: 'timestamp' as const,
            showFollowingOnly: false,
          },
          searchQuery: '',
          selectedTags: [],
          bookmarkedItems: new Set(),
          pinnedItems: new Set(),
        };
      }
      return {};
    }),

    switchView: assign({
      currentView: ({ event }) => {
        if (event.type === 'SWITCH_VIEW') {
          return event.viewType;
        }
        return 'feed';
      },
      activeCustomView: () => undefined,
      feedItems: () => [],
      hasMore: () => true,
    }),

    switchToCustomView: assign({
      currentView: () => 'feed',
      activeCustomView: ({ event }) => {
        if (event.type === 'SWITCH_TO_CUSTOM_VIEW') {
          return event.customView;
        }
        return undefined;
      },
      feedItems: () => [],
      hasMore: () => true,
    }),

    setLoading: assign({ loading: () => true, error: () => null }),
    
    setFeedData: assign({
      feedItems: ({ context, event }) => {
        if (event.type === 'FEED_LOADED') {
          return context.feedItems.length === 0 
            ? event.items 
            : [...context.feedItems, ...event.items];
        }
        return context.feedItems;
      },
      hasMore: ({ event }) => {
        if (event.type === 'FEED_LOADED') {
          return event.hasMore;
        }
        return true;
      },
      loading: () => false,
      lastFetchTime: () => Date.now(),
    }),

    setError: assign({
      error: ({ event }) => {
        if (event.type === 'FEED_ERROR') {
          return event.error;
        }
        return null;
      },
      loading: () => false,
    }),

    refreshFeed: assign({
      feedItems: () => [],
      hasMore: () => true,
      error: () => null,
    }),

    updateRealTime: assign({
      feedItems: ({ context, event }) => {
        if (event.type === 'REAL_TIME_UPDATE') {
          // Add new items to the top of the feed
          return [...event.items, ...context.feedItems];
        }
        return context.feedItems;
      },
    }),

    updateContent: assign({
      feedItems: ({ context, event }) => {
        if (event.type === 'CONTENT_UPDATED') {
          return context.feedItems.map(feedItem => 
            feedItem.content.id === event.item.id 
              ? { ...feedItem, content: event.item }
              : feedItem
          );
        }
        return context.feedItems;
      },
    }),

    deleteContent: assign({
      feedItems: ({ context, event }) => {
        if (event.type === 'CONTENT_DELETED') {
          return context.feedItems.filter(item => item.content.id !== event.itemId);
        }
        return context.feedItems;
      },
    }),

    setFilters: assign({
      filters: ({ context, event }) => {
        if (event.type === 'SET_FILTERS') {
          return { ...context.filters, ...event.filters };
        }
        return context.filters;
      },
      feedItems: () => [],
      hasMore: () => true,
    }),

    setSearchQuery: assign({
      searchQuery: ({ event }) => {
        if (event.type === 'SEARCH') {
          return event.query;
        }
        return '';
      },
      feedItems: () => [],
      hasMore: () => true,
    }),

    toggleBookmark: assign({
      bookmarkedItems: ({ context, event }) => {
        if (event.type === 'TOGGLE_BOOKMARK') {
          const newBookmarks = new Set(context.bookmarkedItems);
          if (newBookmarks.has(event.itemId)) {
            newBookmarks.delete(event.itemId);
          } else {
            newBookmarks.add(event.itemId);
          }
          return newBookmarks;
        }
        return context.bookmarkedItems;
      },
    }),

    togglePin: assign({
      pinnedItems: ({ context, event }) => {
        if (event.type === 'TOGGLE_PIN') {
          const newPinned = new Set(context.pinnedItems);
          if (newPinned.has(event.itemId)) {
            newPinned.delete(event.itemId);
          } else {
            newPinned.add(event.itemId);
          }
          return newPinned;
        }
        return context.pinnedItems;
      },
    }),

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
            view.id === event.viewId 
              ? { ...view, ...event.updates }
              : view
          );
        }
        return context.customViews;
      },
    }),

    deleteCustomView: assign({
      customViews: ({ context, event }) => {
        if (event.type === 'DELETE_CUSTOM_VIEW') {
          return context.customViews.filter(view => view.id !== event.viewId);
        }
        return context.customViews;
      },
    }),

    enableRealTime: assign({ realTimeUpdates: () => true }),
    disableRealTime: assign({ realTimeUpdates: () => false }),
  },
}).createMachine({
  id: 'feedActor',
  initial: 'initializing',
  context: {
    currentView: 'feed',
    customViews: [],
    activeCustomView: undefined,
    feedItems: [],
    loading: false,
    error: null,
    hasMore: true,
    lastFetchTime: 0,
    realTimeUpdates: false,
    user: null,
    filters: {
      contentTypes: ['chat', 'event', 'link'],
      timeRange: 'day',
      sortBy: 'timestamp',
      showFollowingOnly: false,
    },
    searchQuery: '',
    selectedTags: [],
    bookmarkedItems: new Set(),
    pinnedItems: new Set(),
  },
  states: {
    initializing: {
      entry: 'initializeFromInput',
      always: {
        target: 'idle',
      },
    },

    idle: {
      on: {
        LOAD_FEED: {
          target: 'loading',
        },
        SWITCH_VIEW: {
          actions: 'switchView',
          target: 'loading',
        },
        SWITCH_TO_CUSTOM_VIEW: {
          actions: 'switchToCustomView',
          target: 'loading',
        },
        REFRESH: {
          actions: 'refreshFeed',
          target: 'loading',
        },
        SET_FILTERS: {
          actions: 'setFilters',
          target: 'loading',
        },
        SEARCH: {
          actions: 'setSearchQuery',
          target: 'loading',
        },
        TOGGLE_BOOKMARK: {
          actions: 'toggleBookmark',
        },
        TOGGLE_PIN: {
          actions: 'togglePin',
        },
        REACT_TO_CONTENT: {
          // Handle reactions optimistically
        },
        CREATE_CUSTOM_VIEW: {
          actions: 'createCustomView',
        },
        UPDATE_CUSTOM_VIEW: {
          actions: 'updateCustomView',
        },
        DELETE_CUSTOM_VIEW: {
          actions: 'deleteCustomView',
        },
        ENABLE_REAL_TIME: {
          actions: 'enableRealTime',
          target: 'realTime',
        },
        CONTENT_UPDATED: {
          actions: 'updateContent',
        },
        CONTENT_DELETED: {
          actions: 'deleteContent',
        },
      },
    },

    loading: {
      entry: 'setLoading',
      invoke: {
        id: 'loadFeed',
        src: async ({ context }) => {
          const view = context.activeCustomView || context.currentView;
          const offset = context.feedItems.length;
          
          return mockFeedAPI.fetchFeed(
            view,
            context.filters,
            context.searchQuery,
            offset
          );
        },
        onDone: {
          actions: 'setFeedData',
          target: 'idle',
        },
        onError: {
          actions: 'setError',
          target: 'idle',
        },
      },
      on: {
        LOAD_MORE: {
          target: 'loadingMore',
        },
      },
    },

    loadingMore: {
      invoke: {
        id: 'loadMoreFeed',
        src: async ({ context }) => {
          const view = context.activeCustomView || context.currentView;
          const offset = context.feedItems.length;
          
          return mockFeedAPI.fetchFeed(
            view,
            context.filters,
            context.searchQuery,
            offset
          );
        },
        onDone: {
          actions: 'setFeedData',
          target: 'idle',
        },
        onError: {
          actions: 'setError',
          target: 'idle',
        },
      },
    },

    realTime: {
      entry: 'enableRealTime',
      exit: 'disableRealTime',
      invoke: {
        id: 'realTimeUpdates',
        src: 'realTimeCallback',
      },
      on: {
        REAL_TIME_UPDATE: {
          actions: 'updateRealTime',
        },
        DISABLE_REAL_TIME: {
          target: 'idle',
        },
        LOAD_FEED: {
          target: 'loading',
        },
        LOAD_MORE: {
          target: 'loadingMore',
        },
        SWITCH_VIEW: {
          actions: 'switchView',
          target: 'loading',
        },
        SWITCH_TO_CUSTOM_VIEW: {
          actions: 'switchToCustomView',
          target: 'loading',
        },
        REFRESH: {
          actions: 'refreshFeed',
          target: 'loading',
        },
        SET_FILTERS: {
          actions: 'setFilters',
          target: 'loading',
        },
        SEARCH: {
          actions: 'setSearchQuery',
          target: 'loading',
        },
        TOGGLE_BOOKMARK: {
          actions: 'toggleBookmark',
        },
        TOGGLE_PIN: {
          actions: 'togglePin',
        },
        CONTENT_UPDATED: {
          actions: 'updateContent',
        },
        CONTENT_DELETED: {
          actions: 'deleteContent',
        },
      },
    },
  },
});

// Utility functions for feed management
export const createFeedActorInput = (user: User, initialView?: ViewType) => ({
  user,
  initialView,
});

export const getFeedItemScore = (item: FeedItem): number => {
  return calculateFeedScore(item.content) * item.score;
};

export const sortFeedItems = (
  items: FeedItem[], 
  sortBy: 'timestamp' | 'trending' | 'relevance',
  pinnedItems: Set<string>
): FeedItem[] => {
  // Separate pinned and regular items
  const pinned = items.filter(item => pinnedItems.has(item.content.id));
  const regular = items.filter(item => !pinnedItems.has(item.content.id));
  
  // Sort regular items
  regular.sort((a, b) => {
    switch (sortBy) {
      case 'timestamp':
        return b.content.timestamp - a.content.timestamp;
      case 'trending':
        return (b.isTrending ? 1 : 0) - (a.isTrending ? 1 : 0) || 
               b.content.timestamp - a.content.timestamp;
      case 'relevance':
        return getFeedItemScore(b) - getFeedItemScore(a);
      default:
        return 0;
    }
  });
  
  return [...pinned, ...regular];
};