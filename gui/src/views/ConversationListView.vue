<script setup lang="ts">
// This view is the empty state shown when no conversation is selected.
// The conversation list itself is now in the sidebar.
</script>

<template>
  <div class="flex h-full flex-col items-center justify-center text-center">
    <div class="mb-6 flex h-20 w-20 items-center justify-center rounded-full bg-surface text-4xl">
      🐧
    </div>
    <h2 class="text-2xl font-semibold text-foreground">Welcome to Waddle</h2>
    <p class="mt-2 max-w-sm text-sm text-muted">
      Select a conversation from the sidebar to start chatting, or start a new direct message.
    </p>
  </div>
</template>
