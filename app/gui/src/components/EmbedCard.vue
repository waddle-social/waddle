<script setup lang="ts">
/**
 * EmbedCard renders a plugin-generated embed attached to a chat message.
 *
 * For known namespaces (e.g. urn:waddle:github:0) it renders a built-in card.
 * Unknown namespaces get a generic fallback.
 */

interface Embed {
  namespace: string;
  data: Record<string, unknown>;
}

const props = defineProps<{ embed: Embed }>();
const NS_WADDLE_GITHUB = 'urn:waddle:github:0';

function str(value: unknown): string {
  return typeof value === 'string' ? value : '';
}

function isGitHubRepo(embed: Embed): boolean {
  const type = str(embed.data.type).toLowerCase();
  const owner = str(embed.data.owner);
  const name = str(embed.data.name);
  const url = str(embed.data.url);
  return (
    embed.namespace === NS_WADDLE_GITHUB &&
    (type === 'repo' || (!!owner && !!name && !!url))
  );
}

function isGitHubIssueOrPr(embed: Embed): boolean {
  const type = str(embed.data.type).toLowerCase();
  const repo = str(embed.data.repo);
  const number = str(embed.data.number);
  const url = str(embed.data.url);
  return (
    embed.namespace === NS_WADDLE_GITHUB &&
    !!repo &&
    !!number &&
    !!url &&
    (type === 'issue' || type === 'pr' || type === '')
  );
}

function num(value: unknown): number | null {
  if (typeof value === 'number' && Number.isFinite(value)) return value;
  if (typeof value === 'string' && value.trim().length > 0) {
    const parsed = Number(value);
    if (Number.isFinite(parsed)) return parsed;
  }
  return null;
}

function formatStars(n: number): string {
  if (n >= 1000) return `${(n / 1000).toFixed(1)}k`;
  return String(n);
}

function githubIssueKind(embed: Embed): string {
  const type = str(embed.data.type).toLowerCase();
  if (type === 'pr') return 'PR';
  if (type === 'issue') return 'Issue';
  return str(embed.data.url).includes('/pull/') ? 'PR' : 'Issue';
}
</script>

<template>
  <!-- GitHub repository card -->
  <div
    v-if="isGitHubRepo(props.embed)"
    class="mt-2 max-w-md rounded-lg border border-border bg-surface p-3"
  >
    <div class="flex items-center gap-2">
      <svg class="h-4 w-4 flex-shrink-0 text-muted" viewBox="0 0 16 16" fill="currentColor">
        <path fill-rule="evenodd" d="M2 2.5A2.5 2.5 0 014.5 0h8.75a.75.75 0 01.75.75v12.5a.75.75 0 01-.75.75h-2.5a.75.75 0 110-1.5h1.75v-2h-8a1 1 0 00-.714 1.7.75.75 0 01-1.072 1.05A2.495 2.495 0 012 11.5v-9z" />
      </svg>
      <a
        :href="str(props.embed.data.url) || `https://github.com/${str(props.embed.data.owner)}/${str(props.embed.data.name)}`"
        target="_blank"
        rel="noopener"
        class="text-sm font-semibold text-accent hover:underline"
      >
        {{ str(props.embed.data.owner) }}/{{ str(props.embed.data.name) }}
      </a>
    </div>
    <p v-if="str(props.embed.data.description)" class="mt-1 text-xs leading-snug text-muted line-clamp-2">
      {{ str(props.embed.data.description) }}
    </p>
    <div class="mt-2 flex flex-wrap items-center gap-3 text-xs text-muted">
      <span v-if="str(props.embed.data.language)" class="flex items-center gap-1">
        <span class="inline-block h-2.5 w-2.5 rounded-full bg-accent"></span>
        {{ str(props.embed.data.language) }}
      </span>
      <span v-if="num(props.embed.data.stars) !== null" class="flex items-center gap-1">
        ⭐ {{ formatStars(num(props.embed.data.stars)!) }}
      </span>
      <span v-if="str(props.embed.data.license)">
        📄 {{ str(props.embed.data.license) }}
      </span>
    </div>
  </div>

  <!-- GitHub issue / PR card -->
  <div
    v-else-if="isGitHubIssueOrPr(props.embed)"
    class="mt-2 max-w-md rounded-lg border border-border bg-surface p-3"
  >
    <div class="flex items-center gap-2">
      <span class="text-sm">{{ str(props.embed.data.state) === 'closed' ? '🟣' : '🟢' }}</span>
      <a
        :href="str(props.embed.data.url)"
        target="_blank"
        rel="noopener"
        class="text-sm font-semibold text-accent hover:underline"
      >
        {{ githubIssueKind(props.embed) }} · {{ str(props.embed.data.repo) }}#{{ str(props.embed.data.number) }}
      </a>
    </div>
    <p class="mt-1 text-xs leading-snug text-foreground">
      {{ str(props.embed.data.title) || `${githubIssueKind(props.embed)} #${str(props.embed.data.number)}` }}
    </p>
    <div class="mt-1 flex items-center gap-2 text-xs text-muted">
      <span v-if="str(props.embed.data.author)">by {{ str(props.embed.data.author) }}</span>
      <span v-if="str(props.embed.data.state)">· {{ str(props.embed.data.state) }}</span>
    </div>
  </div>

  <!-- Generic fallback -->
  <div v-else class="mt-2 max-w-md rounded border border-border bg-surface-raised px-3 py-2 text-xs text-muted">
    📎 embed: {{ props.embed.namespace }}
  </div>
</template>
