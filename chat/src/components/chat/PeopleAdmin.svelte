<script lang="ts">
  import { Field } from "@ark-ui/svelte/field";
  import type { EditableRole } from "../../lib/chat-ui";
  import type { MemberSummary, UserSearchResult } from "../../lib/waddle-api";

  export let sortedMembers: MemberSummary[] = [];
  export let memberQuery = "";
  export let newMemberRole: EditableRole = "member";
  export let memberSearchResults: UserSearchResult[] = [];
  export let canManageMembers = false;
  export let isSearchingUsers = false;
  export let isSubmitting = false;
  export let onAddMember: (userId: string) => void | Promise<void>;
  export let onUpdateMemberRole: (member: MemberSummary, role: EditableRole) => void | Promise<void>;
  export let onRemoveMember: (member: MemberSummary) => void | Promise<void>;
</script>

<section class="section">
  <div class="stack">
    <header class="section__header">
      <p class="muted">Add member</p>
    </header>

    <div class="form">
      <Field.Root class="form-block">
        <Field.Label>Search username</Field.Label>
        <Field.Input bind:value={memberQuery} type="search" maxlength="80" disabled={!canManageMembers} />
      </Field.Root>

      <Field.Root class="form-block">
        <Field.Label>Role</Field.Label>
        <Field.Select bind:value={newMemberRole} disabled={!canManageMembers}>
          <option value="member">member</option>
          <option value="moderator">moderator</option>
          <option value="admin">admin</option>
        </Field.Select>
      </Field.Root>
    </div>

    {#if isSearchingUsers}
      <p>Searching…</p>
    {:else if memberSearchResults.length}
      <div class="list">
        {#each memberSearchResults as user (user.id)}
          <div class="inline-row">
            <div>
              <div>{user.display_name || user.username}</div>
              <div class="muted">{user.jid}</div>
            </div>
            <button type="button" disabled={isSubmitting || !canManageMembers} on:click={() => onAddMember(user.id)}>
              Add
            </button>
          </div>
        {/each}
      </div>
    {/if}
  </div>

  <div class="stack">
    <header class="section__header">
      <p class="muted">Members</p>
    </header>

    {#if sortedMembers.length}
      <div class="list">
        {#each sortedMembers as member (member.user_id)}
          <div class="inline-row">
            <div>
              <div>{member.username}</div>
              <div class="muted">{member.role}</div>
            </div>

            <div class="member-actions">
              <Field.Root class="form-block">
                <Field.Label>Role</Field.Label>
                <Field.Select
                  value={member.role}
                  disabled={member.role === "owner" || !canManageMembers}
                  on:change={(event) =>
                    onUpdateMemberRole(
                      member,
                      (event.currentTarget as HTMLSelectElement).value as EditableRole,
                    )}
                >
                  <option value="member">member</option>
                  <option value="moderator">moderator</option>
                  <option value="admin">admin</option>
                  {#if member.role === "owner"}
                    <option value="owner">owner</option>
                  {/if}
                </Field.Select>
              </Field.Root>

              <button
                type="button"
                disabled={member.role === "owner" || !canManageMembers}
                on:click={() => onRemoveMember(member)}
              >
                Remove
              </button>
            </div>
          </div>
        {/each}
      </div>
    {:else}
      <p>No members.</p>
    {/if}
  </div>
</section>
