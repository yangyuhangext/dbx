<script setup lang="ts">
import { computed, onMounted, onUnmounted } from "vue";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { useAgentRuntimeStore } from "@/stores/agentRuntimeStore";

const agentRuntimeStore = useAgentRuntimeStore();
let refreshTimer: number | null = null;

const active = computed(() => agentRuntimeStore.activeHandoff);
const riskTone = computed(() => {
  if (!active.value) return "secondary";
  return active.value.riskLevel === "critical" || active.value.isProduction ? "destructive" : "secondary";
});

async function refresh() {
  try {
    await agentRuntimeStore.loadHandoffs();
  } catch (err) {
    console.debug("[DBX] Agent handoff refresh skipped:", err);
  }
}

async function rejectActive() {
  if (!active.value) return;
  await agentRuntimeStore.rejectHandoff(active.value.id);
}

onMounted(() => {
  void refresh();
  refreshTimer = window.setInterval(() => void refresh(), 5000);
});

onUnmounted(() => {
  if (refreshTimer) window.clearInterval(refreshTimer);
});
</script>

<template>
  <Dialog v-model:open="agentRuntimeStore.handoffDialogOpen">
    <DialogContent class="max-w-3xl">
      <DialogHeader>
        <DialogTitle>DBX Agent Handoff</DialogTitle>
      </DialogHeader>

      <div v-if="active" class="space-y-4">
        <div class="flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
          <Badge :variant="riskTone">{{ active.riskLevel }}</Badge>
          <span>{{ active.connectionName }}</span>
          <span v-if="active.database">/ {{ active.database }}</span>
          <span>/ {{ active.operationClass }}</span>
          <span v-if="active.status === 'shown'">/ shown</span>
        </div>

        <div class="space-y-1">
          <h3 class="text-sm font-semibold">{{ active.title }}</h3>
          <p v-if="active.description" class="text-sm text-muted-foreground">{{ active.description }}</p>
        </div>

        <pre class="max-h-96 overflow-auto rounded-md border bg-muted/60 p-3 text-xs leading-relaxed">{{
          active.sql
        }}</pre>

        <p class="text-xs text-muted-foreground">
          Review only: DBX does not execute agent handoff SQL from this dialog.
        </p>
      </div>

      <div v-else class="text-sm text-muted-foreground">No pending agent handoffs.</div>

      <DialogFooter>
        <Button variant="outline" @click="agentRuntimeStore.handoffDialogOpen = false">Close</Button>
        <Button v-if="active" variant="destructive" @click="rejectActive">Reject</Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>
</template>
