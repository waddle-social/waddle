<script setup lang="ts">
import { computed } from 'vue';
import Avatar from './ui/Avatar.vue';
import AvatarFallback from './ui/AvatarFallback.vue';
import type { Reply } from '../types/chat';

defineOptions({ name: 'ThreadTree' });

const props = withDefaults(
  defineProps<{
    replies: Reply[];
    depth?: number;
    getAvatar?: (author: string) => string;
  }>(),
  {
    depth: 0,
    getAvatar: () => '/placeholder-user.jpg',
  },
);

const getInitials = (name: string) =>
  name
    .split(' ')
    .map((n) => n[0])
    .join('')
    .toUpperCase();

const depthArray = computed(() => Array.from({ length: props.depth }));

const getAvatar = (author: string) => props.getAvatar?.(author) ?? '/placeholder-user.jpg';
</script>

<template>
  <div>
    <div v-for="(reply, index) in props.replies" :key="reply.id" class="relative">
      <div class="flex gap-0">
        <template v-if="props.depth > 0">
          <div v-for="(_, depthIndex) in depthArray" :key="depthIndex" class="w-12 flex-shrink-0 relative">
            <div class="absolute left-6 top-0 bottom-0 w-px bg-foreground/30"></div>
          </div>
          <div class="w-12 flex-shrink-0 relative">
            <div v-if="index < props.replies.length - 1" class="absolute left-6 top-0 bottom-0 w-px bg-foreground/30"></div>
            <div class="absolute left-6 top-5 w-6 h-px bg-foreground/30"></div>
            <div class="absolute left-6 top-0 w-px h-5 bg-foreground/30"></div>
          </div>
        </template>

        <div class="flex-1 min-w-0 mb-4">
          <div class="border border-foreground p-4 bg-background hover:bg-muted/30 transition-colors">
            <div class="flex gap-3">
              <Avatar class="w-8 h-8 rounded-none border border-foreground flex-shrink-0" :src="getAvatar(reply.author)" :alt="reply.author">
                <AvatarFallback class="rounded-none bg-primary text-primary-foreground font-mono text-xs font-bold">
                  {{ getInitials(reply.author) }}
                </AvatarFallback>
              </Avatar>
              <div class="flex-1 min-w-0">
                <div class="flex items-baseline gap-2 mb-1">
                  <span class="font-mono font-bold text-sm">{{ reply.author }}</span>
                  <span class="text-xs font-mono text-muted-foreground">{{ reply.time }}</span>
                </div>
                <p class="text-sm leading-relaxed">{{ reply.content }}</p>
              </div>
            </div>
          </div>

          <ThreadTree
            v-if="reply.replies && reply.replies.length"
            :replies="reply.replies"
            :depth="props.depth + 1"
            :get-avatar="props.getAvatar"
            class="mt-4"
          />
        </div>
      </div>
    </div>
  </div>
</template>
