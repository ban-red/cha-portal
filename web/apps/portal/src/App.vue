<script setup lang="ts">
import { useSession } from "./stores/session";

const session = useSession();
</script>

<template>
  <!-- Keyed by user, so "view as" and switching back rebuild every page instead of leaving the last user's state on screen. -->
  <RouterView :key="session.user?.id ?? 'guest'" />
</template>
