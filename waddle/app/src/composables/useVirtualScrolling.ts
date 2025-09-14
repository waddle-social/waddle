import { ref, computed, onMounted, onUnmounted, nextTick, type Ref } from 'vue';

export interface VirtualScrollItem {
  id: string;
  height?: number;
}

export interface VirtualScrollOptions {
  itemHeight: number;
  buffer: number;
  scrollContainer?: Ref<HTMLElement | null>;
  estimateItemHeight?: (item: any, index: number) => number;
  onScroll?: (scrollTop: number, isScrollingDown: boolean) => void;
  onLoadMore?: () => void;
  loadMoreThreshold?: number;
}

export interface VirtualScrollReturn<T extends VirtualScrollItem> {
  containerRef: Ref<HTMLElement | null>;
  listRef: Ref<HTMLElement | null>;
  visibleItems: Ref<Array<{ item: T; index: number; top: number; height: number }>>;
  scrollToItem: (index: number, behavior?: ScrollBehavior) => void;
  scrollToTop: (behavior?: ScrollBehavior) => void;
  scrollToBottom: (behavior?: ScrollBehavior) => void;
  totalHeight: Ref<number>;
  scrollTop: Ref<number>;
  isScrolling: Ref<boolean>;
  isAtTop: Ref<boolean>;
  isAtBottom: Ref<boolean>;
  visibleRange: Ref<{ start: number; end: number }>;
}

export function useVirtualScrolling<T extends VirtualScrollItem>(
  items: Ref<T[]>,
  options: VirtualScrollOptions
): VirtualScrollReturn<T> {
  const {
    itemHeight: defaultItemHeight,
    buffer = 5,
    scrollContainer,
    estimateItemHeight,
    onScroll,
    onLoadMore,
    loadMoreThreshold = 200,
  } = options;

  // Refs for DOM elements
  const containerRef = ref<HTMLElement | null>(null);
  const listRef = ref<HTMLElement | null>(null);
  
  // Scroll state
  const scrollTop = ref(0);
  const containerHeight = ref(0);
  const isScrolling = ref(false);
  const lastScrollTime = ref(0);
  
  // Item heights cache
  const itemHeights = ref(new Map<string, number>());
  const measuredItems = ref(new Set<string>());

  // Calculate item positions and heights
  const itemPositions = computed(() => {
    const positions: Array<{ top: number; height: number }> = [];
    let currentTop = 0;

    for (let i = 0; i < items.value.length; i++) {
      const item = items.value[i];
      let height = defaultItemHeight;
      
      // Use cached height if available
      if (itemHeights.value.has(item.id)) {
        height = itemHeights.value.get(item.id)!;
      } else if (estimateItemHeight) {
        height = estimateItemHeight(item, i);
      }

      positions.push({ top: currentTop, height });
      currentTop += height;
    }

    return positions;
  });

  // Total scrollable height
  const totalHeight = computed(() => {
    const positions = itemPositions.value;
    return positions.length > 0 
      ? positions[positions.length - 1].top + positions[positions.length - 1].height
      : 0;
  });

  // Visible range calculation
  const visibleRange = computed(() => {
    const positions = itemPositions.value;
    if (positions.length === 0) return { start: 0, end: 0 };

    const startY = scrollTop.value;
    const endY = startY + containerHeight.value;

    // Binary search for start index
    let start = 0;
    let end = positions.length - 1;
    
    while (start <= end) {
      const mid = Math.floor((start + end) / 2);
      const position = positions[mid];
      
      if (position.top + position.height < startY) {
        start = mid + 1;
      } else {
        end = mid - 1;
      }
    }

    const startIndex = Math.max(0, start - buffer);

    // Binary search for end index
    start = 0;
    end = positions.length - 1;
    
    while (start <= end) {
      const mid = Math.floor((start + end) / 2);
      const position = positions[mid];
      
      if (position.top <= endY) {
        start = mid + 1;
      } else {
        end = mid - 1;
      }
    }

    const endIndex = Math.min(positions.length - 1, end + buffer);

    return { start: startIndex, end: endIndex };
  });

  // Visible items with their positions
  const visibleItems = computed(() => {
    const range = visibleRange.value;
    const positions = itemPositions.value;
    const result: Array<{ item: T; index: number; top: number; height: number }> = [];

    for (let i = range.start; i <= range.end; i++) {
      if (i < items.value.length) {
        const item = items.value[i];
        const position = positions[i];
        
        result.push({
          item,
          index: i,
          top: position.top,
          height: position.height,
        });
      }
    }

    return result;
  });

  // Scroll position flags
  const isAtTop = computed(() => scrollTop.value <= 0);
  const isAtBottom = computed(() => 
    scrollTop.value + containerHeight.value >= totalHeight.value - 1
  );

  // Scroll handlers
  let scrollTimeout: NodeJS.Timeout;
  let lastScrollTop = 0;

  const handleScroll = (event: Event) => {
    const target = event.target as HTMLElement;
    const newScrollTop = target.scrollTop;
    const isScrollingDown = newScrollTop > lastScrollTop;
    
    scrollTop.value = newScrollTop;
    isScrolling.value = true;
    lastScrollTime.value = Date.now();
    
    // Clear existing timeout
    clearTimeout(scrollTimeout);
    
    // Set isScrolling to false after scrolling stops
    scrollTimeout = setTimeout(() => {
      isScrolling.value = false;
    }, 150);

    // Call scroll callback
    if (onScroll) {
      onScroll(newScrollTop, isScrollingDown);
    }

    // Check for load more
    if (onLoadMore && isScrollingDown && 
        (totalHeight.value - newScrollTop - containerHeight.value) < loadMoreThreshold) {
      onLoadMore();
    }

    lastScrollTop = newScrollTop;
  };

  // Resize observer for container height
  let resizeObserver: ResizeObserver | null = null;

  const observeContainer = () => {
    const container = scrollContainer?.value || containerRef.value;
    if (!container) return;

    if (resizeObserver) {
      resizeObserver.disconnect();
    }

    resizeObserver = new ResizeObserver((entries) => {
      for (const entry of entries) {
        containerHeight.value = entry.contentRect.height;
      }
    });

    resizeObserver.observe(container);
  };

  // Item height measurement
  const measureItemHeight = (itemId: string, element: HTMLElement) => {
    if (!measuredItems.value.has(itemId)) {
      const rect = element.getBoundingClientRect();
      const height = rect.height;
      
      if (height > 0) {
        itemHeights.value.set(itemId, height);
        measuredItems.value.add(itemId);
      }
    }
  };

  // Scroll control functions
  const scrollToItem = async (index: number, behavior: ScrollBehavior = 'smooth') => {
    const container = scrollContainer?.value || containerRef.value;
    if (!container || index < 0 || index >= items.value.length) return;

    await nextTick();
    
    const positions = itemPositions.value;
    const targetTop = positions[index]?.top ?? 0;
    
    container.scrollTo({
      top: targetTop,
      behavior,
    });
  };

  const scrollToTop = (behavior: ScrollBehavior = 'smooth') => {
    const container = scrollContainer?.value || containerRef.value;
    if (!container) return;
    
    container.scrollTo({
      top: 0,
      behavior,
    });
  };

  const scrollToBottom = (behavior: ScrollBehavior = 'smooth') => {
    const container = scrollContainer?.value || containerRef.value;
    if (!container) return;
    
    container.scrollTo({
      top: totalHeight.value,
      behavior,
    });
  };

  // Lifecycle
  onMounted(() => {
    nextTick(() => {
      const container = scrollContainer?.value || containerRef.value;
      if (container) {
        container.addEventListener('scroll', handleScroll, { passive: true });
        scrollTop.value = container.scrollTop;
        containerHeight.value = container.clientHeight;
      }
      
      observeContainer();
    });
  });

  onUnmounted(() => {
    const container = scrollContainer?.value || containerRef.value;
    if (container) {
      container.removeEventListener('scroll', handleScroll);
    }
    
    if (resizeObserver) {
      resizeObserver.disconnect();
    }
    
    clearTimeout(scrollTimeout);
  });

  return {
    containerRef,
    listRef,
    visibleItems,
    scrollToItem,
    scrollToTop,
    scrollToBottom,
    totalHeight,
    scrollTop,
    isScrolling,
    isAtTop,
    isAtBottom,
    visibleRange,
    // Expose measurement function for manual height updates
    measureItemHeight: (itemId: string, element: HTMLElement) => {
      measureItemHeight(itemId, element);
    },
  };
}

// Hook for infinite scrolling specifically
export function useInfiniteScroll<T extends VirtualScrollItem>(
  items: Ref<T[]>,
  loadMore: () => Promise<void> | void,
  options: Omit<VirtualScrollOptions, 'onLoadMore'> & {
    hasMore?: Ref<boolean>;
    isLoading?: Ref<boolean>;
    threshold?: number;
  } = {}
) {
  const hasMore = options.hasMore ?? ref(true);
  const isLoading = options.isLoading ?? ref(false);
  const threshold = options.threshold ?? 200;

  const handleLoadMore = async () => {
    if (isLoading.value || !hasMore.value) return;
    
    isLoading.value = true;
    try {
      await loadMore();
    } finally {
      isLoading.value = false;
    }
  };

  const virtualScroll = useVirtualScrolling(items, {
    ...options,
    onLoadMore: handleLoadMore,
    loadMoreThreshold: threshold,
  });

  return {
    ...virtualScroll,
    hasMore,
    isLoading,
  };
}