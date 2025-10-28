<template>
  <div class="h-screen flex bg-background">
    <div class="w-48 border-r border-foreground bg-background flex flex-col">
      <div class="h-20 border-b border-foreground px-6 flex items-center justify-between">
        <span class="text-sm font-mono font-bold uppercase tracking-wider">Waddles</span>
        <div class="flex gap-1">
          <Button variant="ghost" size="icon" class="h-7 w-7" @click="isSearchingWaddles = true">
            <Search class="w-3.5 h-3.5" />
          </Button>
          <Button variant="ghost" size="icon" class="h-7 w-7" @click="isSearchingWaddles = true">
            <Plus class="w-3.5 h-3.5" />
          </Button>
        </div>
      </div>

      <ScrollArea class="flex-1">
        <div class="p-4">
          <div class="space-y-1">
            <div v-for="(group, groupIndex) in dynamicWaddleGroups" :key="group.id" class="mb-6">
              <div class="px-2 mb-3">
                <span class="text-[10px] font-mono uppercase tracking-widest text-muted-foreground/60">{{ group.name }}</span>
              </div>

              <div class="space-y-1">
                <div v-for="waddle in group.waddles" :key="waddle.id" class="relative group/waddle">
                  <button
                    class="w-full flex items-center gap-2 px-2 py-2 transition-colors"
                    :class="activeWaddle === waddle.id ? 'bg-foreground text-background' : 'hover:bg-muted'"
                    @click="selectWaddle(waddle.id)"
                  >
                    <div class="w-1 h-6" :style="{ backgroundColor: waddle.color }"></div>
                    <span class="text-sm font-mono uppercase tracking-wider truncate flex-1 text-left">{{ waddle.name }}</span>
                    <Lock v-if="waddle.isPrivate" class="w-3 h-3 opacity-60" />
                  </button>
                  <button
                    v-if="waddle.id !== 'personal'"
                    class="absolute right-1 top-1/2 -translate-y-1/2 opacity-0 group-hover/waddle:opacity-100 transition-opacity p-1 hover:bg-destructive hover:text-destructive-foreground rounded"
                    @click.stop="waddleToRemove = waddle.id"
                    aria-label="Leave waddle"
                  >
                    <X class="w-3 h-3" />
                  </button>
                </div>
              </div>

              <div v-if="groupIndex < dynamicWaddleGroups.length - 1" class="h-px bg-foreground/20 my-4"></div>
            </div>
          </div>
        </div>
      </ScrollArea>
    </div>

    <div class="flex-1 flex overflow-hidden">
      <div class="w-80 border-r border-foreground bg-background flex flex-col">
        <div class="h-20 px-8 border-b border-foreground flex items-center">
          <div>
            <h2 class="text-xs font-mono uppercase tracking-widest text-muted-foreground mb-1">Topics</h2>
            <div class="text-2xl font-mono font-bold">{{ activeWaddleData?.name }}</div>
          </div>
        </div>

        <ScrollArea class="flex-1">
          <div class="p-4">
            <div class="space-y-1">
              <button
                v-for="tag in sortedTags"
                :key="tag.id"
                class="w-full flex items-center gap-3 px-4 py-3 transition-colors"
                :class="activeTag === tag.id ? 'bg-foreground text-background' : 'hover:bg-muted'"
                @click="selectTag(tag.id)"
              >
                <div class="flex items-center gap-2 flex-1 min-w-0">
                  <component :is="getTagIcon(tag.type)" class="w-3 h-3" />
                  <span class="text-sm font-mono truncate">{{ tag.name }}</span>
                </div>
                  <Badge
                    v-if="getMessageCountForTag(tag.name, activeWaddle) > 0"
                    variant="secondary"
                    class="h-5 min-w-5 px-1.5 text-xs font-mono"
                    :class="activeTag === tag.id ? 'bg-background text-foreground' : 'bg-foreground text-background'"
                  >
                    {{ getMessageCountForTag(tag.name, activeWaddle) }}
                  </Badge>
              </button>
            </div>
          </div>
        </ScrollArea>
      </div>

      <div class="flex-1 flex flex-col">
        <div class="h-20 border-b border-foreground px-8 flex items-center justify-between bg-background">
          <div class="flex items-center gap-4">
            <Button
              v-if="activeThreadData"
              variant="ghost"
              size="icon"
              class="mr-2"
              @click="clearActiveThread()"
            >
              <ArrowLeft class="w-5 h-5" />
            </Button>
            <div class="flex items-center gap-2">
              <component v-if="activeTagData" :is="getTagIcon(activeTagData.type)" class="w-3 h-3" />
              <h1 class="text-xl font-mono font-bold">{{ activeTagData?.name || 'Select a topic' }}</h1>
            </div>
            <div v-if="activeThreadData" class="flex items-center gap-2 text-sm font-mono text-muted-foreground">
              <CornerDownRight class="w-4 h-4" />
              <span>Thread</span>
            </div>
            <Badge v-if="activeTagData?.type === 'private'" variant="outline" class="font-mono text-xs">Private</Badge>
        </div>
        <div class="flex items-center gap-2">
          <Button
            variant="ghost"
            size="icon"
            :aria-label="themeButtonLabel"
            :title="themeButtonLabel"
            @click="toggleTheme"
          >
            <Sun v-if="theme === 'light'" class="w-5 h-5" />
            <Moon v-else class="w-5 h-5" />
          </Button>
          <Button v-if="isAdmin && !activeThreadData" variant="ghost" size="icon" @click="openWaddleSettings">
            <Settings class="w-5 h-5" />
          </Button>
        </div>
      </div>

      <ScrollArea class="flex-1 px-8 py-8">
          <div v-if="!activeThreadData" class="space-y-4 max-w-4xl">
            <div v-if="!hasActiveTopic" class="text-center py-12">
              <p class="font-mono text-sm text-muted-foreground">Select a topic to view messages.</p>
            </div>
            <template v-else>
              <div v-if="filteredMessages.length === 0" class="text-center py-12">
                <p class="font-mono text-sm text-muted-foreground">No messages in this topic yet. Start the conversation!</p>
              </div>
              <div
                v-for="msg in filteredMessages"
                :key="msg.id"
                class="flex gap-4 border border-foreground p-4 hover:bg-muted/50 transition-colors cursor-pointer"
                @click="setActiveThread(msg)"
              >
              <Avatar class="w-10 h-10 rounded-none border border-foreground flex-shrink-0" :src="getAvatar(msg.author)" :alt="msg.author">
                <AvatarFallback class="rounded-none bg-primary text-primary-foreground font-mono font-bold">
                  {{ getInitials(msg.author) }}
                </AvatarFallback>
              </Avatar>
                <div class="flex-1 min-w-0">
                  <div class="flex items-baseline gap-3 mb-1">
                    <span class="font-mono font-bold text-sm">{{ msg.author }}</span>
                    <span class="text-xs font-mono text-muted-foreground">{{ msg.time }}</span>
                  </div>
                  <p class="text-sm leading-relaxed">{{ msg.content }}</p>
                  <div class="mt-3 flex items-center gap-4">
                    <div class="flex items-center gap-2 text-xs font-mono text-muted-foreground">
                      <MessageSquare class="w-3.5 h-3.5" />
                      <span>
                        <template v-if="countAllReplies(msg) > 0">
                          {{ countAllReplies(msg) }} {{ countAllReplies(msg) === 1 ? 'reply' : 'replies' }}
                        </template>
                        <template v-else>Reply</template>
                      </span>
                    </div>
                  </div>
                </div>
              </div>
            </template>
          </div>

          <div v-else class="space-y-8 max-w-5xl">
            <div class="flex gap-4 border-2 border-foreground p-6 bg-muted">
              <Avatar
                class="w-10 h-10 rounded-none border border-foreground flex-shrink-0"
                :src="activeThreadData ? getAvatar(activeThreadData.author) : null"
                :alt="activeThreadData?.author ?? ''"
              >
                <AvatarFallback class="rounded-none bg-primary text-primary-foreground font-mono font-bold">
                  {{ activeThreadData ? getInitials(activeThreadData.author) : '' }}
                </AvatarFallback>
              </Avatar>
              <div class="flex-1 min-w-0">
                <div class="flex items-baseline gap-3 mb-1">
                  <span class="font-mono font-bold text-sm">{{ activeThreadData?.author }}</span>
                  <span class="text-xs font-mono text-muted-foreground">{{ activeThreadData?.time }}</span>
                </div>
                <p class="text-sm leading-relaxed">{{ activeThreadData?.content }}</p>
              </div>
            </div>

            <div v-if="activeThreadData?.replies && activeThreadData.replies.length">
              <div class="flex items-center gap-2 mb-8 px-2">
                <div class="h-px flex-1 bg-foreground/20"></div>
                <span class="text-xs font-mono uppercase tracking-widest text-muted-foreground">
                  {{ activeThreadData.replies.length }}
                  {{ activeThreadData.replies.length === 1 ? 'Reply' : 'Replies' }}
                </span>
                <div class="h-px flex-1 bg-foreground/20"></div>
              </div>
              <ThreadTree :replies="activeThreadData.replies" :get-avatar="getAvatar" />
            </div>
          </div>
        </ScrollArea>

        <div class="border-t border-foreground bg-background">
          <div class="px-8 py-6">
            <div v-if="activeThreadData && replyActiveTags.length" class="mb-4 flex flex-wrap gap-2">
              <Badge
                v-for="(tag, index) in replyActiveTags"
                :key="`reply-${index}`"
                variant="outline"
                class="font-mono text-xs border-foreground px-2 py-1 flex items-center gap-2"
              >
                <Hash class="w-3 h-3" />
                {{ tag }}
                <button
                  v-if="!replyBaseTags.includes(tag)"
                  class="hover:text-destructive transition-colors"
                  @click="removeReplyTag(tag)"
                  aria-label="Remove tag"
                >
                  ×
                </button>
              </Badge>
            </div>
            <div v-else-if="!activeThreadData && composerTagChips.length" class="mb-4 flex flex-wrap gap-2">
              <Badge
                v-for="chip in composerTagChips"
                :key="chip.name"
                variant="outline"
                class="font-mono text-xs border-foreground px-2 py-1 flex items-center gap-2"
              >
                <Hash class="w-3 h-3" />
                {{ chip.name }}
                <button
                  v-if="chip.removable"
                  class="hover:text-destructive transition-colors"
                  @click="removeTag(chip.name)"
                  aria-label="Remove tag"
                >
                  ×
                </button>
              </Badge>
            </div>

            <div class="flex gap-4 relative">
              <div class="flex-1 relative">
                <template v-if="activeThreadData">
                  <div
                    ref="replyBoxRef"
                    contenteditable="true"
                    @input="handleReplyInput"
                    @keydown="handleReplyKeyDown"
                    class="w-full font-mono border border-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-foreground px-3 py-2.5 h-11 overflow-hidden whitespace-nowrap bg-background"
                    style="caret-color: black"
                    data-placeholder="Reply to thread..."
                  ></div>

                  <div
                    v-if="isCreatingReplyTag"
                    class="absolute left-0 right-0 -top-16 bg-background border border-foreground p-3 z-20"
                  >
                    <div class="flex items-center gap-2">
                      <Hash class="w-4 h-4" />
                      <Input
                        ref="replyTagInputRef"
                        :value="replyTagInput"
                        @input="handleReplyTagInputChange"
                        @keydown="handleReplyTagInputKeyDown"
                        placeholder="Enter tag name (spaces allowed)"
                        class="flex-1 font-mono border-foreground focus-visible:ring-foreground rounded-none h-9 text-sm"
                      />
                      <span class="text-xs font-mono text-muted-foreground">Enter to add</span>
                    </div>
                  </div>
                </template>
                <template v-else>
                  <template v-if="hasActiveTopic">
                    <div
                      ref="messageBoxRef"
                      contenteditable="true"
                      @input="handleInput"
                      @keydown="handleKeyDown"
                      class="w-full font-mono border border-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-foreground px-3 py-2.5 h-11 overflow-hidden whitespace-nowrap bg-background"
                      style="caret-color: black"
                      :data-placeholder="`Message #${activeTagData?.name ?? ''}`"
                    ></div>

                    <div v-if="isCreatingTag" class="absolute left-0 right-0 -top-16 bg-background border border-foreground p-3 z-20">
                      <div class="flex items-center gap-2">
                        <Hash class="w-4 h-4" />
                        <Input
                          ref="tagInputRef"
                          :value="tagInput"
                          @input="handleTagInputChange"
                          @keydown="handleTagInputKeyDown"
                          placeholder="Enter tag name (spaces allowed)"
                          class="flex-1 font-mono border-foreground focus-visible:ring-foreground rounded-none h-9 text-sm"
                        />
                        <span class="text-xs font-mono text-muted-foreground">Enter to add</span>
                      </div>
                    </div>
                  </template>
                  <div
                    v-else
                    class="w-full font-mono border border-dashed border-muted-foreground/60 px-3 py-2.5 h-11 flex items-center text-sm text-muted-foreground bg-muted/40 justify-center"
                  >
                    Select a topic to start a message.
                  </div>
                </template>
              </div>
              <Button
                size="icon"
                class="h-11 w-11 rounded-none bg-foreground hover:bg-foreground/90 text-background"
                :disabled="!activeThreadData && !hasActiveTopic"
                @click="activeThreadData ? handleSendReply() : handleSendMessage()"
              >
                <Send class="w-4 h-4" />
              </Button>
            </div>
          </div>
        </div>
      </div>
    </div>

    <div
      v-if="waddleToRemove"
      class="fixed inset-0 bg-background/80 backdrop-blur-sm z-50 flex items-center justify-center"
    >
      <div class="w-full max-w-md bg-background border-2 border-foreground">
        <div class="border-b border-foreground p-6">
          <h2 class="text-xl font-mono font-bold uppercase tracking-wider">Leave Waddle?</h2>
        </div>
        <div class="p-6">
          <p class="font-mono text-sm leading-relaxed">
            Are you sure you want to leave
            <span class="font-bold">{{ waddleToRemoveData ? waddleToRemoveData.name : '' }}</span>? You will no longer have access to its topics and messages.
          </p>
        </div>
        <div class="border-t border-foreground p-6 flex justify-end gap-3">
          <Button variant="outline" class="font-mono uppercase tracking-wider rounded-none" @click="waddleToRemove = null">
            Cancel
          </Button>
          <Button
            class="font-mono uppercase tracking-wider rounded-none bg-destructive hover:bg-destructive/90 text-destructive-foreground"
            @click="removeWaddle(waddleToRemove)"
          >
            Leave Waddle
          </Button>
        </div>
      </div>
    </div>

    <div
      v-if="isSearchingWaddles"
      class="fixed inset-0 bg-background/80 backdrop-blur-sm z-50 flex items-start justify-center pt-20"
    >
      <div class="w-full max-w-2xl bg-background border-2 border-foreground">
        <div class="border-b border-foreground p-6 flex items-center justify-between">
          <h2 class="text-xl font-mono font-bold uppercase tracking-wider">Browse Waddles</h2>
          <Button
            variant="ghost"
            size="icon"
            @click="closeWaddleSearch"
          >
            <X class="w-5 h-5" />
          </Button>
        </div>

        <div class="p-6 border-b border-foreground">
          <div class="relative">
            <Search class="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-muted-foreground" />
            <Input
              :value="waddleSearchQuery"
              @input="handleWaddleSearchInput"
              placeholder="Search waddles..."
              class="pl-10 font-mono border-foreground focus-visible:ring-foreground rounded-none h-9 text-sm"
              autofocus
            />
          </div>
        </div>

        <ScrollArea class="max-h-96">
          <div class="p-6 space-y-2">
            <div v-if="filteredWaddles.length === 0" class="text-sm font-mono text-muted-foreground">
              No waddles found.
            </div>
            <div
              v-for="waddle in filteredWaddles"
              :key="waddle.id"
              class="border border-foreground px-4 py-3 flex items-center justify-between hover:bg-muted transition-colors"
            >
              <div>
                <div class="text-sm font-mono font-bold">{{ waddle.name }}</div>
                <div class="text-xs font-mono text-muted-foreground">
                  {{ waddle.isPrivate ? 'Private' : 'Public' }} · {{ waddle.memberCount }} members
                </div>
              </div>
              <Button variant="outline" class="rounded-none font-mono uppercase tracking-wider" @click="addWaddle(waddle.id)">
                Join
              </Button>
            </div>
          </div>
        </ScrollArea>
      </div>
    </div>

    <div
      v-if="isEditingWaddle"
      class="fixed inset-0 bg-background/80 backdrop-blur-sm z-50 flex items-start justify-center pt-20"
    >
      <div class="w-full max-w-2xl bg-background border-2 border-foreground">
        <div class="border-b border-foreground p-6 flex items-center justify-between">
          <h2 class="text-xl font-mono font-bold uppercase tracking-wider">Waddle Settings</h2>
          <Button variant="ghost" size="icon" @click="closeWaddleSettings">
            <X class="w-5 h-5" />
          </Button>
        </div>

        <ScrollArea class="max-h-[600px]">
          <div class="p-6 space-y-6">
            <div>
              <label class="block text-xs font-mono uppercase tracking-widest text-muted-foreground mb-2">Waddle Name</label>
              <Input
                :value="waddleSettings.name"
                @input="handleWaddleNameInput"
                class="font-mono border-foreground focus-visible:ring-foreground rounded-none"
              />
            </div>

            <div>
              <label class="block text-xs font-mono uppercase tracking-widest text-muted-foreground mb-2">Description</label>
              <textarea
                :value="waddleSettings.description"
                @input="handleWaddleDescriptionInput"
                class="w-full font-mono border border-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-foreground px-3 py-2 min-h-24 bg-background"
              ></textarea>
            </div>

            <div>
              <label class="block text-xs font-mono uppercase tracking-widest text-muted-foreground mb-2">Privacy</label>
              <div class="flex gap-2">
                <Button
                  :variant="waddleSettings.isPrivate ? 'default' : 'outline'"
                  class="font-mono uppercase tracking-wider rounded-none flex-1"
                  @click="waddleSettings.isPrivate = true"
                >
                  <Lock class="w-4 h-4 mr-2" />
                  Private
                </Button>
                <Button
                  :variant="!waddleSettings.isPrivate ? 'default' : 'outline'"
                  class="font-mono uppercase tracking-wider rounded-none flex-1"
                  @click="waddleSettings.isPrivate = false"
                >
                  <Globe class="w-4 h-4 mr-2" />
                  Public
                </Button>
              </div>
            </div>

            <div>
              <label class="block text-xs font-mono uppercase tracking-widest text-muted-foreground mb-2">Default Tags</label>
              <div class="space-y-3">
                <div class="flex gap-2">
                  <Input
                    :value="newDefaultTag"
                    @input="handleNewDefaultTagInput"
                    @keydown="(event) => {
                      if (event.key === 'Enter') {
                        event.preventDefault();
                        addDefaultTag();
                      }
                    }"
                    placeholder="Add a default tag..."
                    class="font-mono border-foreground focus-visible:ring-foreground rounded-none flex-1"
                  />
                  <Button variant="outline" class="font-mono uppercase tracking-wider rounded-none bg-transparent" @click="addDefaultTag">
                    Add
                  </Button>
                </div>
                <div v-if="waddleSettings.defaultTags.length" class="flex flex-wrap gap-2">
                  <Badge
                    v-for="tag in waddleSettings.defaultTags"
                    :key="tag"
                    variant="outline"
                    class="font-mono text-xs border-foreground px-2 py-1 flex items-center gap-2"
                  >
                    <Hash class="w-3 h-3" />
                    {{ tag }}
                    <button class="hover:text-destructive transition-colors" @click="removeDefaultTag(tag)" aria-label="Remove tag">
                      ×
                    </button>
                  </Badge>
                </div>
              </div>
            </div>

            <div>
              <label class="block text-xs font-mono uppercase tracking-widest text-muted-foreground mb-2">Invite Link</label>
              <div class="flex gap-2">
                <Input
                  :value="waddleSettings.inviteLink"
                  readonly
                  class="font-mono border-foreground focus-visible:ring-foreground rounded-none flex-1 bg-muted"
                />
                <Button variant="outline" class="font-mono uppercase tracking-wider rounded-none bg-transparent" @click="copyInviteLink">
                  <Check v-if="inviteLinkCopied" class="w-4 h-4" />
                  <Copy v-else class="w-4 h-4" />
                </Button>
              </div>
              <p class="text-xs font-mono text-muted-foreground mt-2">Share this link to invite people to this waddle</p>
            </div>
          </div>
        </ScrollArea>

        <div class="border-t border-foreground p-6 flex justify-end gap-3">
          <Button variant="outline" class="font-mono uppercase tracking-wider rounded-none" @click="closeWaddleSettings">
            Cancel
          </Button>
          <Button class="font-mono uppercase tracking-wider rounded-none" @click="closeWaddleSettings">
            Save Changes
          </Button>
        </div>
      </div>
    </div>
  </div>
</template>


<script setup lang="ts">
import { computed, nextTick, onMounted, reactive, ref, watch } from 'vue';
import Button from './ui/Button.vue';
import Input from './ui/Input.vue';
import Badge from './ui/Badge.vue';
import Avatar from './ui/Avatar.vue';
import AvatarFallback from './ui/AvatarFallback.vue';
import ScrollArea from './ui/ScrollArea.vue';
import ThreadTree from './ThreadTree.vue';
import type { Message, Reply, TagDictionary, TagMeta, TagType } from '../types/chat';
import {
  Hash,
  Lock,
  Rss,
  Radio,
  Send,
  Plus,
  Search,
  X,
  Globe,
  Settings,
  Copy,
  Check,
  MessageSquare,
  ArrowLeft,
  CornerDownRight,
  Moon,
  Sun,
} from 'lucide-vue-next';
import BlueskyIcon from './icons/BlueskyIcon.vue';

const availableWaddles = [
  { id: 'personal', name: 'Personal', color: '#000000', isPrivate: true, memberCount: 1 },
  { id: 'design', name: 'Design Team', color: '#E63946', isPrivate: true, memberCount: 12 },
  { id: 'engineering', name: 'Engineering', color: '#457B9D', isPrivate: true, memberCount: 45 },
  { id: 'random', name: 'Random', color: '#2A9D8F', isPrivate: false, memberCount: 234 },
  { id: 'marketing', name: 'Marketing', color: '#F77F00', isPrivate: true, memberCount: 8 },
  { id: 'product', name: 'Product', color: '#06FFA5', isPrivate: false, memberCount: 156 },
  { id: 'sales', name: 'Sales', color: '#9D4EDD', isPrivate: true, memberCount: 23 },
  { id: 'support', name: 'Support', color: '#FF006E', isPrivate: false, memberCount: 89 },
  { id: 'general', name: 'General', color: '#8338EC', isPrivate: false, memberCount: 567 },
  { id: 'announcements', name: 'Announcements', color: '#FB5607', isPrivate: false, memberCount: 892 },
  { id: 'leadership', name: 'Leadership', color: '#3A86FF', isPrivate: true, memberCount: 5 },
  { id: 'bluesky', name: 'Bluesky', color: '#1185F7', isPrivate: false, memberCount: 128 },
] as const;

const authorAvatars: Record<string, string> = {};

type InputInstance = {
  focus: () => void;
  el: HTMLInputElement | null;
};




const initialTags: TagDictionary = {
  personal: [
    { id: 'dm-1', name: 'alice', type: 'dm', unread: 3 },
    { id: 'dm-2', name: 'bob', type: 'dm', unread: 0 },
    { id: 'rss-1', name: 'hacker-news', type: 'rss', unread: 12 },
    { id: 'broadcast-1', name: 'announcements', type: 'broadcast', unread: 1 },
  ],
  design: [
    { id: 'tag-1', name: 'ui-reviews', type: 'public', unread: 5 },
    { id: 'tag-2', name: 'design-system', type: 'public', unread: 0 },
    { id: 'tag-3', name: 'leadership', type: 'private', unread: 2 },
  ],
  engineering: [
    { id: 'tag-4', name: 'backend', type: 'public', unread: 8 },
    { id: 'tag-5', name: 'frontend', type: 'public', unread: 3 },
    { id: 'tag-6', name: 'incidents', type: 'private', unread: 0 },
    { id: 'tag-7', name: 'architecture', type: 'public', unread: 1 },
  ],
  random: [
    { id: 'tag-8', name: 'watercooler', type: 'public', unread: 15 },
    { id: 'tag-9', name: 'pets', type: 'public', unread: 7 },
  ],
  bluesky: [
    { id: 'bsky-1', name: 'fediverse', type: 'bluesky', unread: 6 },
    { id: 'bsky-2', name: 'dev-updates', type: 'bluesky', unread: 3 },
    { id: 'bsky-rss', name: 'bsky-dev', type: 'bluesky', unread: 4 },
  ],
};

const initialMessages: Message[] = [
  {
    id: 1,
    waddleId: 'design',
    author: 'Alice Chen',
    content: 'The new grid system is working really well',
    time: '14:23',
    replyCount: 3,
    tags: ['ui-reviews'],
    replies: [
      {
        id: 11,
        author: 'Bob Smith',
        content: 'Totally agree! The spacing feels much more consistent now.',
        time: '14:24',
        tags: ['ui-reviews'],
      },
      {
        id: 12,
        author: 'Carol White',
        content: 'Should we apply this to the mobile views too?',
        time: '14:26',
        replyCount: 2,
        tags: ['ui-reviews'],
        replies: [
          {
            id: 121,
            author: 'Alice Chen',
            content: 'Yes, I think we should maintain consistency across all breakpoints',
            time: '14:27',
            tags: ['ui-reviews'],
          },
          {
            id: 122,
            author: 'Bob Smith',
            content: 'I can help with the mobile implementation',
            time: '14:28',
            tags: ['ui-reviews'],
          },
        ],
      },
      {
        id: 13,
        author: 'You',
        content: 'The modular approach makes everything so much clearer',
        time: '14:29',
        tags: ['ui-reviews'],
      },
    ],
  },
  {
    id: 2,
    waddleId: 'design',
    author: 'Bob Smith',
    content: 'Agreed, the modular approach makes everything clearer',
    time: '14:25',
    replyCount: 1,
    tags: ['ui-reviews', 'design-system'],
    replies: [
      {
        id: 21,
        author: 'Alice Chen',
        content: 'Thanks! It took a while to get the proportions right',
        time: '14:26',
        tags: ['ui-reviews', 'design-system'],
      },
    ],
  },
  {
    id: 3,
    waddleId: 'design',
    author: 'You',
    content: 'Should we document the spacing system?',
    time: '14:27',
    replyCount: 2,
    tags: ['design-system'],
    replies: [
      {
        id: 31,
        author: 'Alice Chen',
        content: 'Yes, let me create a spec doc with all the measurements',
        time: '14:28',
        tags: ['design-system'],
      },
      {
        id: 32,
        author: 'Bob Smith',
        content: 'I can add some visual examples to the doc',
        time: '14:30',
        tags: ['design-system'],
      },
    ],
  },
  {
    id: 4,
    waddleId: 'design',
    author: 'Alice Chen',
    content: 'Yes, let me create a spec doc',
    time: '14:28',
    tags: ['design-system'],
  },
  {
    id: 5,
    waddleId: 'personal',
    author: 'reader.bot',
    content: 'HN #42388901 · “Astro Islands at scale” is climbing this morning — worth a skim for component isolation tips.',
    time: '07:45',
    tags: ['hacker-news'],
    replies: [
      {
        id: 51,
        author: 'Naomi Holt',
        content: 'Pinned it to #dev-updates so folks can compare with our current hydration setup.',
        time: '07:52',
        tags: ['hacker-news'],
      },
    ],
  },
  {
    id: 6,
    waddleId: 'bluesky',
    author: 'Mira Patel',
    content: 'Bluesky graph sync now handles 5x more subscriptions — rollout to production starts tomorrow.',
    time: '09:14',
    tags: ['dev-updates'],
    replies: [
      {
        id: 61,
        author: 'Ken Brooks',
        content: 'QA says the staging edge nodes look good. I’ll post the changelog once logs settle.',
        time: '09:18',
        tags: ['dev-updates'],
      },
      {
        id: 62,
        author: 'Quincy Ly',
        content: 'Let’s include migration steps for self-hosted relays in the doc.',
        time: '09:20',
        tags: ['dev-updates'],
      },
    ],
  },
  {
    id: 7,
    waddleId: 'bluesky',
    author: 'Evelyn Tan',
    content: 'Weekly federated bridge sync completed — 1.2M posts federated into Bluesky from Mastodon without drops.',
    time: '10:05',
    tags: ['fediverse'],
  },
  {
    id: 8,
    waddleId: 'bluesky',
    author: 'Sol Ortega',
    content: 'Bluesky 1.74.2 is rolling out with accessible composer previews and faster PDS indexing.',
    time: '10:22',
    tags: ['bsky-dev'],
  },
];

const storageKey = 'waddle:theme';
const theme = ref<'light' | 'dark'>('light');

const applyTheme = (value: 'light' | 'dark', persist = true) => {
  theme.value = value;
  const root = document.documentElement;
  root.classList.toggle('dark', value === 'dark');
  root.dataset.theme = value;
  if (persist) {
    try {
      localStorage.setItem(storageKey, value);
    } catch (err) {
      // Ignore environments where localStorage is unavailable.
    }
  }
};

const toggleTheme = () => {
  applyTheme(theme.value === 'dark' ? 'light' : 'dark');
};

const themeButtonLabel = computed(() =>
  theme.value === 'dark' ? 'Switch to light mode' : 'Switch to dark mode',
);

const activeWaddle = ref('design');
const activeTag = ref('tag-1');
const message = ref('');
const activeTags = ref<string[]>([]);
const isCreatingTag = ref(false);
const tagInput = ref('');
const hashPosition = ref(-1);
const isSearchingWaddles = ref(false);
const waddleSearchQuery = ref('');
const joinedWaddleIds = ref<string[]>(['personal', 'design', 'engineering', 'random', 'bluesky']);
const isEditingWaddle = ref(false);
const waddleSettings = reactive({
  name: '',
  description: '',
  isPrivate: false,
  defaultTags: [] as string[],
  inviteLink: '',
});
const newDefaultTag = ref('');
const inviteLinkCopied = ref(false);
const waddleToRemove = ref<string | null>(null);
const activeThreadId = ref<number | null>(null);
const activeThreadSourceWaddle = ref<string | null>(null);
const activeThreadData = computed(() => {
  if (activeThreadId.value === null) return null;
  const baseWaddle = activeThreadSourceWaddle.value ?? activeWaddle.value;
  const found = findItemById(messages.value, activeThreadId.value, baseWaddle);
  return (found as Message | Reply | null) ?? null;
});
const messages = ref<Message[]>(JSON.parse(JSON.stringify(initialMessages)));
const replyMessage = ref('');
const replyActiveTags = ref<string[]>([]);
const isCreatingReplyTag = ref(false);
const replyTagInput = ref('');
const nextMessageId = ref(200);
const tags = ref<TagDictionary>(JSON.parse(JSON.stringify(initialTags)));
const nextTagId = ref(100);
const lastActiveTagByWaddle = ref<Record<string, string>>({});
const lastActiveThreadByTag = ref<Record<string, { waddleId: string; threadId: number }>>({});

onMounted(() => {
  let initial: 'light' | 'dark' = 'light';
  const root = document.documentElement;
  const datasetTheme = root.dataset.theme;
  if (datasetTheme === 'dark' || datasetTheme === 'light') {
    initial = datasetTheme;
  } else if (root.classList.contains('dark')) {
    initial = 'dark';
  } else {
    try {
      const stored = localStorage.getItem(storageKey);
      if (stored === 'light' || stored === 'dark') {
        initial = stored;
      } else if (window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches) {
        initial = 'dark';
      }
    } catch (err) {
      if (window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches) {
        initial = 'dark';
      }
    }
  }
  applyTheme(initial, false);
});

const messageBoxRef = ref<HTMLDivElement | null>(null);
const tagInputRef = ref<InputInstance | null>(null);
const replyBoxRef = ref<HTMLDivElement | null>(null);
const replyTagInputRef = ref<InputInstance | null>(null);

const currentTags = computed(() => tags.value[activeWaddle.value] ?? []);
const activeTagData = computed(() => currentTags.value.find((t) => t.id === activeTag.value));
const hasActiveTopic = computed(() => Boolean(activeTagData.value));
const sortedTags = computed(() => [...currentTags.value].sort((a, b) => a.name.localeCompare(b.name)));
const filteredMessages = computed(() => {
  if (!hasActiveTopic.value) return [];
  return messages.value.filter(
    (msg) =>
      msg.waddleId === activeWaddle.value &&
      (msg.tags ? msg.tags.includes(activeTagData.value!.name) : false),
  );
});
const composerTagChips = computed(() => {
  const chips: { name: string; removable: boolean }[] = [];
  if (activeTagData.value) {
    chips.push({ name: activeTagData.value.name, removable: false });
  }

  activeTags.value.forEach((tag) => {
    if (!chips.some((chip) => chip.name === tag)) {
      chips.push({ name: tag, removable: true });
    }
  });

  return chips;
});
const replyBaseTags = computed(() => activeThreadData.value?.tags ?? []);
const activeWaddleData = computed(() => availableWaddles.find((w) => w.id === activeWaddle.value) ?? null);
const filteredWaddles = computed(() =>
  availableWaddles.filter(
    (waddle) =>
      waddle.id !== 'personal' &&
      waddle.name.toLowerCase().includes(waddleSearchQuery.value.toLowerCase()),
  ),
);
const dynamicWaddleGroups = computed(() =>
  [
    {
      id: 'personal',
      name: 'Personal',
      waddles: availableWaddles.filter(
        (w) => w.id === 'personal' && joinedWaddleIds.value.includes(w.id),
      ),
    },
    {
      id: 'work',
      name: 'Work',
      waddles: availableWaddles.filter(
        (w) => w.isPrivate && w.id !== 'personal' && joinedWaddleIds.value.includes(w.id),
      ),
    },
    {
      id: 'communities',
      name: 'Communities',
      waddles: availableWaddles.filter((w) => !w.isPrivate && joinedWaddleIds.value.includes(w.id)),
    },
  ].filter((group) => group.waddles.length > 0),
);

const isAdmin = computed(() => ['design', 'engineering'].includes(activeWaddle.value));
const waddleToRemoveData = computed(
  () => availableWaddles.find((w) => w.id === waddleToRemove.value) ?? null,
);

const getTagIcon = (type: TagType) => {
  switch (type) {
    case 'dm':
      return Hash;
    case 'rss':
      return Rss;
    case 'bluesky':
      return BlueskyIcon;
    case 'broadcast':
      return Radio;
    case 'private':
      return Lock;
    default:
      return Hash;
  }
};

const countTagOccurrences = (items: (Message | Reply)[], tagName: string, waddleId: string): number =>
  items.reduce((total, item) => {
    if ('waddleId' in item && item.waddleId !== waddleId) {
      return total;
    }
    const selfCount = item.tags && item.tags.includes(tagName) ? 1 : 0;
    const replyCount = item.replies ? countTagOccurrences(item.replies, tagName, waddleId) : 0;
    return total + selfCount + replyCount;
  }, 0);

const getMessageCountForTag = (tagName: string, waddleId: string) =>
  countTagOccurrences(messages.value, tagName, waddleId);

const highlightHashtags = () => {
  if (!messageBoxRef.value || isCreatingTag.value) return;

  const selection = window.getSelection();
  let caretOffset = 0;
  if (selection && selection.rangeCount > 0) {
    const range = selection.getRangeAt(0);
    caretOffset = range.startOffset;
    const preCaretRange = range.cloneRange();
    preCaretRange.selectNodeContents(messageBoxRef.value);
    preCaretRange.setEnd(range.startContainer, range.startOffset);
    caretOffset = preCaretRange.toString().length;
  }

  const text = messageBoxRef.value.textContent ?? '';
  const tagList = [...activeTags.value].sort((a, b) => b.length - a.length);

  let html = '';
  let remainingText = text;

  while (remainingText.length > 0) {
    let foundMatch = false;

    for (const tag of tagList) {
      const hashtagWithHash = `#${tag}`;
      if (remainingText.startsWith(hashtagWithHash)) {
        html += `<span style="background-color: black; color: white; padding: 0 2px;">${hashtagWithHash}</span>`;
        remainingText = remainingText.substring(hashtagWithHash.length);
        foundMatch = true;
        break;
      }
    }

    if (!foundMatch) {
      const char = remainingText[0];
      html += char === ' ' ? '&nbsp;' : char;
      remainingText = remainingText.substring(1);
    }
  }

  messageBoxRef.value.innerHTML = html;

  if (selection) {
    const range = document.createRange();
    let currentOffset = 0;
    let found = false;

    const traverseNodes = (node: Node) => {
      if (found) return;

      if (node.nodeType === Node.TEXT_NODE) {
        const textLength = node.textContent?.length ?? 0;
        if (currentOffset + textLength >= caretOffset) {
          range.setStart(node, caretOffset - currentOffset);
          range.collapse(true);
          found = true;
          return;
        }
        currentOffset += textLength;
      } else {
        node.childNodes.forEach((child) => {
          traverseNodes(child);
        });
      }
    };

    traverseNodes(messageBoxRef.value);

    if (found) {
      selection.removeAllRanges();
      selection.addRange(range);
    }
  }
};

const highlightReplyHashtags = () => {
  if (!replyBoxRef.value || isCreatingReplyTag.value) return;

  const selection = window.getSelection();
  let caretOffset = 0;
  if (selection && selection.rangeCount > 0) {
    const range = selection.getRangeAt(0);
    caretOffset = range.startOffset;
    const preCaretRange = range.cloneRange();
    preCaretRange.selectNodeContents(replyBoxRef.value);
    preCaretRange.setEnd(range.startContainer, range.startOffset);
    caretOffset = preCaretRange.toString().length;
  }

  const text = replyBoxRef.value.textContent ?? '';
  const tagList = [...replyActiveTags.value].sort((a, b) => b.length - a.length);

  let html = '';
  let remainingText = text;

  while (remainingText.length > 0) {
    let foundMatch = false;

    for (const tag of tagList) {
      const hashtagWithHash = `#${tag}`;
      if (remainingText.startsWith(hashtagWithHash)) {
        html += `<span style="background-color: black; color: white; padding: 0 2px;">${hashtagWithHash}</span>`;
        remainingText = remainingText.substring(hashtagWithHash.length);
        foundMatch = true;
        break;
      }
    }

    if (!foundMatch) {
      const char = remainingText[0];
      html += char === ' ' ? '&nbsp;' : char;
      remainingText = remainingText.substring(1);
    }
  }

  replyBoxRef.value.innerHTML = html;

  if (selection) {
    const range = document.createRange();
    let currentOffset = 0;
    let found = false;

    const traverseNodes = (node: Node) => {
      if (found) return;

      if (node.nodeType === Node.TEXT_NODE) {
        const textLength = node.textContent?.length ?? 0;
        if (currentOffset + textLength >= caretOffset) {
          range.setStart(node, caretOffset - currentOffset);
          range.collapse(true);
          found = true;
          return;
        }
        currentOffset += textLength;
      } else {
        node.childNodes.forEach((child) => {
          traverseNodes(child);
        });
      }
    };

    traverseNodes(replyBoxRef.value);

    if (found) {
      selection.removeAllRanges();
      selection.addRange(range);
    }
  }
};

const addNewTag = (tagName: string, waddleId: string) => {
  const waddleTags = tags.value[waddleId] ?? [];
  const tagExists = waddleTags.some((t) => t.name === tagName);

  if (!tagExists) {
    const newTag: TagMeta = {
      id: `tag-${nextTagId.value}`,
      name: tagName,
      type: 'public',
      unread: 0,
    };

    tags.value = {
      ...tags.value,
      [waddleId]: [...waddleTags, newTag],
    };

    nextTagId.value += 1;
  }
};

const addWaddle = (waddleId: string) => {
  if (waddleId === 'personal') {
    return;
  }
  if (!joinedWaddleIds.value.includes(waddleId)) {
    joinedWaddleIds.value = [...joinedWaddleIds.value, waddleId];
  }
  isSearchingWaddles.value = false;
  waddleSearchQuery.value = '';
};

const removeWaddle = (waddleId: string) => {
  if (waddleId !== 'personal') {
    joinedWaddleIds.value = joinedWaddleIds.value.filter((id) => id !== waddleId);
    if (activeWaddle.value === waddleId) {
      activeWaddle.value = 'personal';
    }
  }
  waddleToRemove.value = null;
};

const handleInput = (event: Event) => {
  const target = event.currentTarget as HTMLDivElement;
  const text = target.textContent ?? '';
  message.value = text;

  if (!isCreatingTag.value) {
    nextTick(() => highlightHashtags());
  }
};

const handleReplyInput = (event: Event) => {
  const target = event.currentTarget as HTMLDivElement;
  const text = target.textContent ?? '';
  replyMessage.value = text;

  if (!isCreatingReplyTag.value) {
    nextTick(() => highlightReplyHashtags());
  }
};

const getCurrentTime = () => {
  const now = new Date();
  return `${now.getHours()}:${now.getMinutes().toString().padStart(2, '0')}`;
};

const countAllReplies = (item: Message | Reply): number => {
  if (!item.replies || item.replies.length === 0) {
    return item.replyCount ?? 0;
  }
  return item.replies.reduce((acc, reply) => acc + 1 + countAllReplies(reply), 0);
};

const findItemById = (items: (Message | Reply)[], targetId: number, waddleId?: string): Message | Reply | null => {
  for (const item of items) {
    if ('waddleId' in item && waddleId && item.waddleId !== waddleId) {
      continue;
    }
    if (item.id === targetId) {
      return item;
    }
    if (item.replies) {
      const nested = findItemById(item.replies, targetId, waddleId);
      if (nested) {
        return nested;
      }
    }
  }
  return null;
};

const restoreActiveThreadForTag = (tagId: string) => {
  if (!tagId) {
    activeThreadId.value = null;
    activeThreadSourceWaddle.value = null;
    return;
  }
  const stored = lastActiveThreadByTag.value[tagId];
  if (stored && stored.waddleId === activeWaddle.value) {
    const found = findItemById(messages.value, stored.threadId, activeWaddle.value);
    if (found) {
      activeThreadId.value = stored.threadId;
      activeThreadSourceWaddle.value = activeWaddle.value;
      return;
    }
    delete lastActiveThreadByTag.value[tagId];
  }
  activeThreadId.value = null;
  activeThreadSourceWaddle.value = null;
};

const handleSendMessage = () => {
  const trimmedMessage = message.value.trim();
  if (!trimmedMessage) {
    console.log('[v0] Cannot send empty message');
    return;
  }

  const fallbackTag = activeTagData.value?.name;
  const messageTags = activeTags.value.length > 0 ? [...activeTags.value] : fallbackTag ? [fallbackTag] : [];

  if (messageTags.length === 0) {
    console.log('[v0] Cannot send message without tags');
    return;
  }

  console.log('[v0] Sending message:', trimmedMessage, 'with tags:', messageTags);

  messageTags.forEach((tag) => {
    addNewTag(tag, activeWaddle.value);
  });

  const newMessage: Message = {
    id: nextMessageId.value,
    waddleId: activeWaddle.value,
    author: 'You',
    content: trimmedMessage,
    time: getCurrentTime(),
    tags: [...messageTags],
    replyCount: 0,
  };

  messages.value = [...messages.value, newMessage];

  nextMessageId.value += 1;
  message.value = '';
  activeTags.value = [];

  if (messageBoxRef.value) {
    messageBoxRef.value.textContent = '';
    messageBoxRef.value.innerHTML = '';
  }
};

const handleSendReply = () => {
  const currentThread = activeThreadData.value;
  if (!currentThread) return;

  const currentThreadId = currentThread.id;

  const trimmedMessage = replyMessage.value.trim();
  if (!trimmedMessage) {
    console.log('[v0] Cannot send empty reply');
    return;
  }

  const threadTags = currentThread.tags ? [...currentThread.tags] : [];
  const replyTags = replyActiveTags.value.length > 0 ? [...replyActiveTags.value] : threadTags;

  if (replyTags.length === 0) {
    console.log('[v0] Cannot send reply without tags');
    return;
  }

  replyTags.forEach((tag) => addNewTag(tag, activeWaddle.value));

  const newReply: Reply = {
    id: nextMessageId.value,
    author: 'You',
    content: trimmedMessage,
    time: getCurrentTime(),
    tags: [...replyTags],
  };

  const updateReplies = (items: (Message | Reply)[]): (Message | Reply)[] =>
    items.map((item) => {
      if (item.id === currentThreadId) {
        const updatedReplies = [...(item.replies ?? []), newReply];
        const totalReplyCount = countAllReplies({ ...item, replies: updatedReplies });
        return {
          ...item,
          replies: updatedReplies,
          replyCount: totalReplyCount,
        } as Message | Reply;
      }
      if (item.replies && item.replies.length > 0) {
        const updatedItem = {
          ...item,
          replies: updateReplies(item.replies) as Reply[],
        } as Message | Reply;
        (updatedItem as Message).replyCount = countAllReplies(updatedItem);
        return updatedItem;
      }
      return item;
    });

  const updatedMessages = updateReplies(messages.value) as Message[];
  messages.value = updatedMessages;

  const findUpdatedThread = (items: (Message | Reply)[], targetId: number): Message | Reply | null => {
    for (const item of items) {
      if (item.id === targetId) {
        return item;
      }
      if (item.replies && item.replies.length > 0) {
        const found = findUpdatedThread(item.replies, targetId);
        if (found) return found;
      }
    }
    return null;
  };

  const storedThreadId = activeThreadId.value;
  if (storedThreadId) {
    const updatedThread = findUpdatedThread(updatedMessages, storedThreadId);
    if (updatedThread) {
      activeThreadId.value = updatedThread.id;
      activeThreadSourceWaddle.value = activeWaddle.value;
    }
  }

  nextMessageId.value += 1;
  replyMessage.value = '';
  replyActiveTags.value = [];

  if (replyBoxRef.value) {
    replyBoxRef.value.textContent = '';
    replyBoxRef.value.innerHTML = '';
  }
};

const handleKeyDown = (event: KeyboardEvent) => {
  if (event.key === 'Enter' && !event.shiftKey && !isCreatingTag.value) {
    event.preventDefault();
    event.stopPropagation();
    handleSendMessage();
  }
};

const handleReplyKeyDown = (event: KeyboardEvent) => {
  if (event.key === 'Enter' && !event.shiftKey && !isCreatingReplyTag.value) {
    event.preventDefault();
    event.stopPropagation();
    handleSendReply();
  }
};

const handleTagInputKeyDown = (event: KeyboardEvent) => {
  if (event.key === 'Enter' && tagInput.value.trim()) {
    event.preventDefault();
    const newTag = tagInput.value.trim();
    const beforeHash = message.value.substring(0, hashPosition.value);
    const newMessage = `${beforeHash}#${newTag} `;
    message.value = newMessage;
    activeTags.value = [...activeTags.value, newTag];
    tagInput.value = '';
    isCreatingTag.value = false;

  if (messageBoxRef.value) {
    messageBoxRef.value.textContent = newMessage;
    nextTick(() => {
      highlightHashtags();
      const el = messageBoxRef.value;
      if (el) {
        el.focus();
        const range = document.createRange();
        const sel = window.getSelection();
        if (sel) {
          range.selectNodeContents(el);
          range.collapse(false);
          sel.removeAllRanges();
          sel.addRange(range);
        }
      }
    });
  }
} else if (event.key === 'Escape') {
  isCreatingTag.value = false;
    tagInput.value = '';
    const newMessage = message.value.substring(0, hashPosition.value);
    message.value = newMessage;

    if (messageBoxRef.value) {
      messageBoxRef.value.textContent = newMessage;
      highlightHashtags();
      messageBoxRef.value.focus();
    }
  }
};

const handleReplyTagInputKeyDown = (event: KeyboardEvent) => {
  if (event.key === 'Enter' && replyTagInput.value.trim()) {
    event.preventDefault();
    const newTag = replyTagInput.value.trim();
    const lastHashIndex = replyMessage.value.lastIndexOf('#');
    const beforeHash = replyMessage.value.substring(0, lastHashIndex);
    const newMessage = `${beforeHash}#${newTag} `;
    replyMessage.value = newMessage;
    replyActiveTags.value = [...replyActiveTags.value, newTag];
    replyTagInput.value = '';
    isCreatingReplyTag.value = false;

    if (replyBoxRef.value) {
      replyBoxRef.value.textContent = newMessage;
      nextTick(() => {
        highlightReplyHashtags();
        const el = replyBoxRef.value;
        if (el) {
          el.focus();
          const range = document.createRange();
          const sel = window.getSelection();
          if (sel) {
            range.selectNodeContents(el);
            range.collapse(false);
            sel.removeAllRanges();
            sel.addRange(range);
          }
        }
      });
    }
  } else if (event.key === 'Escape') {
    isCreatingReplyTag.value = false;
    replyTagInput.value = '';
    const lastHashIndex = replyMessage.value.lastIndexOf('#');
    const newMessage = replyMessage.value.substring(0, lastHashIndex);
    replyMessage.value = newMessage;

    if (replyBoxRef.value) {
      replyBoxRef.value.textContent = newMessage;
      highlightReplyHashtags();
      replyBoxRef.value.focus();
    }
  }
};

const removeTag = (tagToRemove: string) => {
  if (activeTagData.value && tagToRemove === activeTagData.value.name) {
    return;
  }
  activeTags.value = activeTags.value.filter((t) => t !== tagToRemove);
};

const removeReplyTag = (tagToRemove: string) => {
  if (replyBaseTags.value.includes(tagToRemove)) {
    return;
  }
  replyActiveTags.value = replyActiveTags.value.filter((t) => t !== tagToRemove);
};

const selectWaddle = (id: string) => {
  activeWaddle.value = id;
  activeThreadId.value = null;
  activeThreadSourceWaddle.value = null;
  const newTags = tags.value[id] ?? [];
  const remembered = lastActiveTagByWaddle.value[id];
  const fallbackTag = newTags.length > 0 ? newTags[0].id : '';
  const nextTag = remembered && newTags.some((tag) => tag.id === remembered) ? remembered : fallbackTag;
  activeTag.value = nextTag;
  if (nextTag) {
    lastActiveTagByWaddle.value[id] = nextTag;
  }
};

const selectTag = (tagId: string) => {
  activeTag.value = tagId;
};

const clearActiveThread = () => {
  if (activeTag.value) {
    delete lastActiveThreadByTag.value[activeTag.value];
  }
  activeThreadId.value = null;
  activeThreadSourceWaddle.value = null;
  replyMessage.value = '';
  replyActiveTags.value = [];
  if (replyBoxRef.value) {
    replyBoxRef.value.textContent = '';
    replyBoxRef.value.innerHTML = '';
  }
};

const setActiveThread = (thread: Message | Reply) => {
  activeThreadId.value = thread.id;
  activeThreadSourceWaddle.value = activeWaddle.value;
  if (activeTag.value) {
    lastActiveThreadByTag.value[activeTag.value] = {
      waddleId: activeWaddle.value,
      threadId: thread.id,
    };
  }
  replyMessage.value = '';
  replyActiveTags.value = thread.tags ? [...thread.tags] : [];
  nextTick(() => {
    if (replyBoxRef.value) {
      replyBoxRef.value.textContent = '';
      replyBoxRef.value.innerHTML = '';
    }
  });
};

const openWaddleSettings = () => {
  const waddle = availableWaddles.find((w) => w.id === activeWaddle.value);
  if (waddle) {
    waddleSettings.name = waddle.name;
    waddleSettings.description = `This is the ${waddle.name} waddle for team collaboration.`;
    waddleSettings.isPrivate = waddle.isPrivate;
    waddleSettings.defaultTags = ['general', 'announcements'];
    waddleSettings.inviteLink = `https://waddle.social/invite/${activeWaddle.value}-${Math.random()
      .toString(36)
      .substring(7)}`;
    isEditingWaddle.value = true;
    inviteLinkCopied.value = false;
  }
};

const copyInviteLink = async () => {
  if (!waddleSettings.inviteLink) return;
  await navigator.clipboard.writeText(waddleSettings.inviteLink);
  inviteLinkCopied.value = true;
  setTimeout(() => {
    inviteLinkCopied.value = false;
  }, 2000);
};

const addDefaultTag = () => {
  const value = newDefaultTag.value.trim();
  if (value && !waddleSettings.defaultTags.includes(value)) {
    waddleSettings.defaultTags = [...waddleSettings.defaultTags, value];
    newDefaultTag.value = '';
  }
};

const removeDefaultTag = (tag: string) => {
  waddleSettings.defaultTags = waddleSettings.defaultTags.filter((t) => t !== tag);
};

const closeWaddleSettings = () => {
  isEditingWaddle.value = false;
};

const closeWaddleSearch = () => {
  isSearchingWaddles.value = false;
  waddleSearchQuery.value = '';
};

const handleWaddleSearchInput = (event: Event) => {
  const target = event.target as HTMLInputElement;
  waddleSearchQuery.value = target.value;
};

const handleTagInputChange = (event: Event) => {
  const target = event.target as HTMLInputElement;
  tagInput.value = target.value;
};

const handleReplyTagInputChange = (event: Event) => {
  const target = event.target as HTMLInputElement;
  replyTagInput.value = target.value;
};

const handleNewDefaultTagInput = (event: Event) => {
  const target = event.target as HTMLInputElement;
  newDefaultTag.value = target.value;
};

const handleWaddleNameInput = (event: Event) => {
  const target = event.target as HTMLInputElement;
  waddleSettings.name = target.value;
};

const handleWaddleDescriptionInput = (event: Event) => {
  const target = event.target as HTMLTextAreaElement;
  waddleSettings.description = target.value;
};

const getInitials = (name: string) =>
  name
    .split(' ')
    .map((n) => n[0])
    .join('')
    .toUpperCase();

const getAvatar = (author: string) => authorAvatars[author] ?? null;

watch(
  currentTags,
  (tagList) => {
    const currentWaddle = activeWaddle.value;
    if (tagList.length === 0) {
      if (activeTag.value) {
        activeTag.value = '';
      }
      if (activeThreadId.value === null) {
        message.value = '';
        activeTags.value = [];
      }
      delete lastActiveTagByWaddle.value[currentWaddle];
      return;
    }

    const remembered = lastActiveTagByWaddle.value[currentWaddle];
    if (remembered && tagList.some((tag) => tag.id === remembered)) {
      if (activeTag.value !== remembered) {
        activeTag.value = remembered;
      } else {
        restoreActiveThreadForTag(remembered);
      }
      return;
    }

    const fallback = tagList[0].id;
    lastActiveTagByWaddle.value[currentWaddle] = fallback;
    activeTag.value = fallback;
  },
  { immediate: true },
);

watch(hasActiveTopic, (active) => {
  if (!active && activeThreadId.value === null) {
    message.value = '';
    activeTags.value = [];
    if (messageBoxRef.value) {
      messageBoxRef.value.textContent = '';
      messageBoxRef.value.innerHTML = '';
    }
  }
});

watch(activeTag, (tagId) => {
  if (tagId) {
    lastActiveTagByWaddle.value[activeWaddle.value] = tagId;
    restoreActiveThreadForTag(tagId);
  } else {
    activeThreadId.value = null;
    activeThreadSourceWaddle.value = null;
  }
});

watch(isCreatingTag, (creating) => {
  if (creating) {
    nextTick(() => tagInputRef.value?.focus());
  } else {
    nextTick(() => highlightHashtags());
  }
});

watch(message, (value) => {
  const lastHashIndex = value.lastIndexOf('#');
  if (lastHashIndex !== -1 && lastHashIndex === value.length - 1) {
    isCreatingTag.value = true;
    hashPosition.value = lastHashIndex;
    tagInput.value = '';
  } else if (isCreatingTag.value && lastHashIndex === -1) {
    isCreatingTag.value = false;
  }
});

watch(
  activeTags,
  () => {
    if (!isCreatingTag.value) {
      nextTick(() => highlightHashtags());
    }
  },
  { deep: true },
);

watch(isCreatingReplyTag, (creating) => {
  if (creating) {
    nextTick(() => replyTagInputRef.value?.focus());
  } else {
    nextTick(() => highlightReplyHashtags());
  }
});

watch(replyMessage, (value) => {
  const lastHashIndex = value.lastIndexOf('#');
  if (lastHashIndex !== -1 && lastHashIndex === value.length - 1) {
    isCreatingReplyTag.value = true;
    replyTagInput.value = '';
  } else if (isCreatingReplyTag.value && lastHashIndex === -1) {
    isCreatingReplyTag.value = false;
  }
});

watch(
  replyActiveTags,
  () => {
    if (!isCreatingReplyTag.value) {
      nextTick(() => highlightReplyHashtags());
    }
  },
  { deep: true },
);
</script>
