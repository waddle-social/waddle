<template>
  <div class="attachment-preview">
    <div class="attachment-info">
      <span class="attachment-type">{{ getTypeIcon(attachment.type) }}</span>
      <span class="attachment-name">{{ attachment.fileName || 'Attachment' }}</span>
      <span v-if="attachment.fileSize" class="attachment-size">{{ formatFileSize(attachment.fileSize) }}</span>
    </div>
  </div>
</template>

<script setup lang="ts">
import type { Attachment } from '../types/content'

interface Props {
  attachment: Attachment
}

const props = defineProps<Props>()

const getTypeIcon = (type: string) => {
  const icons = {
    image: '🖼️',
    video: '🎥',
    audio: '🎵',
    document: '📄',
    link: '🔗',
  }
  return icons[type as keyof typeof icons] || '📎'
}

const formatFileSize = (bytes: number) => {
  if (bytes === 0) return '0 B'
  const k = 1024
  const sizes = ['B', 'KB', 'MB', 'GB']
  const i = Math.floor(Math.log(bytes) / Math.log(k))
  return parseFloat((bytes / Math.pow(k, i)).toFixed(1)) + ' ' + sizes[i]
}
</script>

<style scoped>
.attachment-preview {
  display: flex;
  align-items: center;
  padding: 0.5rem;
  background: rgba(255, 255, 255, 0.05);
  border-radius: 8px;
  border: 1px solid rgba(255, 255, 255, 0.1);
}

.attachment-info {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  flex: 1;
}

.attachment-type {
  font-size: 1.2rem;
}

.attachment-name {
  color: white;
  font-size: 0.9rem;
  font-weight: 500;
}

.attachment-size {
  color: rgba(255, 255, 255, 0.5);
  font-size: 0.8rem;
  margin-left: auto;
}
</style>