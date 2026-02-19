<script setup lang="ts">
import { ref, onMounted, computed } from 'vue';
import { useWaddle } from '../composables/useWaddle';
import { useVCardStore } from '../stores/vcard';
import { useAuthStore } from '../stores/auth';
import AvatarImage from '../components/AvatarImage.vue';

const { getVCard, setVCard } = useWaddle();
const vcardStore = useVCardStore();
const authStore = useAuthStore();

const displayName = ref('');
const avatarPreview = ref<string | null>(null);
const avatarFile = ref<File | null>(null);
const avatarRemoved = ref(false);
const saving = ref(false);
const error = ref<string | null>(null);
const success = ref(false);

const MAX_FILE_SIZE = 5 * 1024 * 1024; // 5MB (NFR-5)
const MAX_OUTPUT_SIZE = 100 * 1024; // 100KB (NFR-5)
const MAX_DIMENSION = 256; // 256x256 (FR-3.6)

const ownJid = computed(() => vcardStore.ownJid || authStore.jid || '');

onMounted(async () => {
  if (!vcardStore.initialized) {
    vcardStore.init(getVCard, setVCard, ownJid.value);
  }

  const vcard = await vcardStore.fetchOwnVCard();
  if (vcard) {
    displayName.value = vcard.fullName || '';
    avatarPreview.value = vcard.photoUrl || null;
  }
});

function onAvatarSelect(event: Event) {
  const input = event.target as HTMLInputElement;
  const file = input.files?.[0];
  if (!file) return;

  error.value = null;

  // Validate file type
  if (!file.type.startsWith('image/') || file.type === 'image/gif') {
    error.value = 'Please select a JPEG or PNG image (animated images are not supported).';
    return;
  }

  // Validate file size (NFR-5)
  if (file.size > MAX_FILE_SIZE) {
    error.value = `Image must be under ${MAX_FILE_SIZE / 1024 / 1024}MB.`;
    return;
  }

  avatarFile.value = file;
  avatarRemoved.value = false;

  // Preview
  const reader = new FileReader();
  reader.onload = (e) => {
    avatarPreview.value = e.target?.result as string;
  };
  reader.onerror = () => {
    error.value = 'Failed to read image file. Please try another image.';
    avatarPreview.value = null;
    avatarFile.value = null;
  };
  reader.readAsDataURL(file);
}

/**
 * Resize an image to max 256x256 and convert to JPEG/PNG.
 * Returns { base64, mimeType } or null on failure.
 */
async function resizeImage(file: File): Promise<{ base64: string; mimeType: string } | null> {
  return new Promise((resolve) => {
    const img = new Image();
    img.onload = () => {
      const canvas = document.createElement('canvas');
      let { width, height } = img;

      // Scale down if larger than MAX_DIMENSION
      if (width > MAX_DIMENSION || height > MAX_DIMENSION) {
        const ratio = Math.min(MAX_DIMENSION / width, MAX_DIMENSION / height);
        width = Math.round(width * ratio);
        height = Math.round(height * ratio);
      }

      canvas.width = width;
      canvas.height = height;
      const ctx = canvas.getContext('2d');
      if (!ctx) { resolve(null); return; }

      ctx.drawImage(img, 0, 0, width, height);

      // Try JPEG first (smaller), fall back to PNG
      let mimeType = 'image/jpeg';
      let dataUrl = canvas.toDataURL('image/jpeg', 0.85);

      // Check output size; try lower quality or PNG if too large
      let base64 = dataUrl.split(',')[1] || '';
      if (base64.length > MAX_OUTPUT_SIZE * 1.37) { // base64 is ~37% larger
        dataUrl = canvas.toDataURL('image/jpeg', 0.6);
        base64 = dataUrl.split(',')[1] || '';
      }

      if (base64.length > MAX_OUTPUT_SIZE * 1.37) {
        mimeType = 'image/png';
        dataUrl = canvas.toDataURL('image/png');
        base64 = dataUrl.split(',')[1] || '';
      }

      // Final hard limit: reject if still too large after all attempts
      if (base64.length > MAX_OUTPUT_SIZE * 1.37) {
        resolve(null);
        return;
      }

      resolve({ base64, mimeType });
    };
    img.onerror = () => resolve(null);

    const reader = new FileReader();
    reader.onload = (e) => { img.src = e.target?.result as string; };
    reader.onerror = () => resolve(null);
    reader.readAsDataURL(file);
  });
}

async function saveProfile() {
  saving.value = true;
  error.value = null;
  success.value = false;

  try {
    let photoBase64: string | undefined;
    let photoMimeType: string | undefined;

    if (avatarFile.value) {
      const resized = await resizeImage(avatarFile.value);
      if (!resized) {
        error.value = 'Failed to process image. Please try a different file.';
        saving.value = false;
        return;
      }
      photoBase64 = resized.base64;
      photoMimeType = resized.mimeType;
    }

    // If avatar was explicitly removed and no new file uploaded, send update without photo
    // This clears the avatar on the server
    await vcardStore.updateOwnVCard({
      fullName: displayName.value.trim() || undefined,
      photoBase64: avatarRemoved.value && !avatarFile.value ? undefined : photoBase64,
      photoMimeType: avatarRemoved.value && !avatarFile.value ? undefined : photoMimeType,
    });

    avatarFile.value = null;
    avatarRemoved.value = false;
    success.value = true;
    setTimeout(() => { success.value = false; }, 3000);
  } catch (e) {
    error.value = e instanceof Error ? e.message : String(e);
  } finally {
    saving.value = false;
  }
}

function removeAvatar() {
  avatarPreview.value = null;
  avatarFile.value = null;
  avatarRemoved.value = true;
}
</script>

<template>
  <div class="flex h-full flex-col">
    <!-- Header -->
    <header class="flex h-12 flex-shrink-0 items-center border-b border-border px-4 shadow-sm">
      <svg class="mr-2 h-5 w-5 text-muted" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="2">
        <path stroke-linecap="round" stroke-linejoin="round" d="M16 7a4 4 0 11-8 0 4 4 0 018 0zM12 14a7 7 0 00-7 7h14a7 7 0 00-7-7z" />
      </svg>
      <h2 class="text-base font-semibold text-foreground">Profile</h2>
    </header>

    <!-- Content -->
    <div class="flex-1 overflow-y-auto p-6">
      <div class="mx-auto max-w-md space-y-6">
        <!-- JID (read-only) -->
        <div>
          <label class="mb-1 block text-xs font-semibold uppercase tracking-wide text-muted">JID</label>
          <div class="rounded-lg bg-surface-raised px-3 py-2 text-sm text-muted">{{ ownJid }}</div>
        </div>

        <!-- Avatar -->
        <div>
          <label class="mb-2 block text-xs font-semibold uppercase tracking-wide text-muted">Avatar</label>
          <div class="flex items-center gap-4">
            <AvatarImage
              :jid="ownJid"
              :name="displayName || ownJid"
              :photo-url="avatarPreview"
              :size="80"
            />
            <div class="flex flex-col gap-2">
              <label
                class="cursor-pointer rounded-lg bg-accent px-4 py-2 text-center text-sm font-medium text-white transition-colors hover:bg-accent/80"
              >
                Upload Photo
                <input
                  type="file"
                  accept="image/jpeg,image/png,image/webp"
                  class="hidden"
                  @change="onAvatarSelect"
                />
              </label>
              <button
                v-if="avatarPreview"
                class="rounded-lg bg-surface-raised px-4 py-2 text-sm text-muted transition-colors hover:bg-hover"
                @click="removeAvatar"
              >
                Remove
              </button>
              <p class="text-xs text-muted">Max 5MB. Will be resized to 256×256.</p>
            </div>
          </div>
        </div>

        <!-- Display Name -->
        <div>
          <label class="mb-1 block text-xs font-semibold uppercase tracking-wide text-muted">Display Name</label>
          <input
            v-model="displayName"
            type="text"
            placeholder="Your display name"
            class="w-full rounded-lg bg-surface-raised px-3 py-2 text-sm text-foreground placeholder-muted outline-none focus:ring-1 focus:ring-accent"
            maxlength="100"
          />
        </div>

        <!-- Error / Success -->
        <div v-if="error" class="rounded bg-danger/20 px-3 py-2 text-sm text-danger">{{ error }}</div>
        <div v-if="success" class="rounded bg-success/20 px-3 py-2 text-sm text-success">Profile saved!</div>

        <!-- Save button -->
        <button
          class="w-full rounded-lg bg-accent px-4 py-2.5 text-sm font-semibold text-white transition-colors hover:bg-accent/80 disabled:opacity-50"
          :disabled="saving"
          @click="saveProfile"
        >
          {{ saving ? 'Saving…' : 'Save Profile' }}
        </button>
      </div>
    </div>
  </div>
</template>
