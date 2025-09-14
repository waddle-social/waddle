import { ref, onMounted, onUnmounted, type Ref } from 'vue';

export interface UseIntersectionObserverOptions extends IntersectionObserverInit {
  freezeOnceVisible?: boolean;
}

export function useIntersectionObserver(
  target: Ref<Element | null>,
  callback: IntersectionObserverCallback,
  options: UseIntersectionObserverOptions = {}
) {
  const { freezeOnceVisible = false, ...observerOptions } = options;
  
  const isIntersecting = ref(false);
  const isVisible = ref(false);
  
  let observer: IntersectionObserver | null = null;
  let frozen = false;

  const observe = () => {
    if (observer || !target.value || frozen) return;

    observer = new IntersectionObserver((entries) => {
      const entry = entries[0];
      isIntersecting.value = entry.isIntersecting;
      
      if (entry.isIntersecting) {
        isVisible.value = true;
        
        if (freezeOnceVisible) {
          frozen = true;
          unobserve();
        }
      }
      
      callback(entries, observer!);
    }, observerOptions);

    observer.observe(target.value);
  };

  const unobserve = () => {
    if (observer && target.value) {
      observer.unobserve(target.value);
    }
  };

  const disconnect = () => {
    if (observer) {
      observer.disconnect();
      observer = null;
    }
  };

  onMounted(() => {
    observe();
  });

  onUnmounted(() => {
    disconnect();
  });

  return {
    isIntersecting,
    isVisible,
    observe,
    unobserve,
    disconnect,
  };
}

// Specialized composable for infinite scroll
export function useInfiniteScroll(
  target: Ref<Element | null>,
  callback: () => void | Promise<void>,
  options: IntersectionObserverOptions = {}
) {
  const isLoading = ref(false);
  const hasMore = ref(true);
  
  const { isIntersecting } = useIntersectionObserver(
    target,
    async (entries) => {
      const entry = entries[0];
      
      if (entry.isIntersecting && !isLoading.value && hasMore.value) {
        isLoading.value = true;
        
        try {
          await callback();
        } catch (error) {
          console.error('Error loading more items:', error);
        } finally {
          isLoading.value = false;
        }
      }
    },
    {
      rootMargin: '100px',
      ...options,
    }
  );

  const setHasMore = (value: boolean) => {
    hasMore.value = value;
  };

  const reset = () => {
    isLoading.value = false;
    hasMore.value = true;
  };

  return {
    isIntersecting,
    isLoading,
    hasMore,
    setHasMore,
    reset,
  };
}

// Specialized composable for lazy loading images/content
export function useLazyLoad(
  target: Ref<Element | null>,
  options: UseIntersectionObserverOptions = {}
) {
  const isLoaded = ref(false);
  const error = ref<Error | null>(null);
  
  const { isVisible } = useIntersectionObserver(
    target,
    () => {
      // Content becomes visible for the first time
      if (!isLoaded.value) {
        isLoaded.value = true;
      }
    },
    {
      freezeOnceVisible: true,
      rootMargin: '50px',
      ...options,
    }
  );

  const loadImage = (src: string): Promise<void> => {
    return new Promise((resolve, reject) => {
      const img = new Image();
      img.onload = () => resolve();
      img.onerror = () => reject(new Error(`Failed to load image: ${src}`));
      img.src = src;
    });
  };

  const loadContent = async (loader: () => Promise<void>) => {
    if (!isVisible.value || isLoaded.value) return;
    
    try {
      await loader();
      isLoaded.value = true;
      error.value = null;
    } catch (err) {
      error.value = err as Error;
    }
  };

  return {
    isVisible,
    isLoaded,
    error,
    loadImage,
    loadContent,
  };
}

// Composable for viewport-based animations
export function useViewportAnimation(
  target: Ref<Element | null>,
  animationClass = 'animate-fade-in',
  options: UseIntersectionObserverOptions = {}
) {
  const isAnimated = ref(false);
  
  const { isVisible } = useIntersectionObserver(
    target,
    () => {
      if (!isAnimated.value && target.value) {
        target.value.classList.add(animationClass);
        isAnimated.value = true;
      }
    },
    {
      freezeOnceVisible: true,
      threshold: 0.1,
      ...options,
    }
  );

  return {
    isVisible,
    isAnimated,
  };
}