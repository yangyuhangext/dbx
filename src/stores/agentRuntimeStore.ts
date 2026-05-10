import { defineStore } from "pinia";
import { computed, ref } from "vue";
import * as api from "@/lib/api";
import {
  deriveHandoffDialogState,
  mergeLoadedHandoffs,
  updateHandoffStatus,
  type AgentHandoffItem,
} from "@/lib/agentHandoff";
import { buildAgentRuntimeSnapshot, DEFAULT_RESULT_SAMPLE_LIMIT } from "@/lib/agentRuntimeSnapshot";
import { useConnectionStore } from "@/stores/connectionStore";
import { useQueryStore } from "@/stores/queryStore";

export const useAgentRuntimeStore = defineStore("agentRuntime", () => {
  const selection = ref<unknown>({ type: "none" });
  const selectedSql = ref("");
  const handoffs = ref<AgentHandoffItem[]>([]);
  const activeHandoffId = ref<string | null>(null);
  const handoffDialogOpen = ref(false);
  let timer: ReturnType<typeof setTimeout> | null = null;

  const activeHandoff = computed(() => handoffs.value.find((item) => item.id === activeHandoffId.value) ?? null);

  function setSelection(value: unknown) {
    selection.value = value;
    scheduleSync();
  }

  function setSelectedSql(value: string) {
    selectedSql.value = value;
    scheduleSync();
  }

  function scheduleSync() {
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => {
      timer = null;
      void syncNow();
    }, 100);
  }

  async function syncNow() {
    const connectionStore = useConnectionStore();
    const queryStore = useQueryStore();
    const snapshot = buildAgentRuntimeSnapshot({
      tabs: queryStore.tabs,
      activeTabId: queryStore.activeTabId,
      getConnection: (connectionId) => connectionStore.getConfig(connectionId),
      selectedSql: selectedSql.value,
      selection: selection.value,
      resultSampleLimit: DEFAULT_RESULT_SAMPLE_LIMIT,
    });

    try {
      await api.agentRuntimeUpdateSnapshot(snapshot);
    } catch (err) {
      console.debug("[DBX] Agent runtime snapshot sync skipped:", err);
    }
  }

  async function loadHandoffs() {
    const loaded = mergeLoadedHandoffs(await api.agentRuntimeLoadHandoffs());
    handoffs.value = loaded;
    const state = deriveHandoffDialogState(loaded, activeHandoffId.value);
    activeHandoffId.value = state.active?.id ?? null;
    handoffDialogOpen.value = state.open;
    if (state.active?.status === "queued") {
      await markHandoffShown(state.active.id);
    }
  }

  async function markHandoffShown(id: string) {
    handoffs.value = updateHandoffStatus(handoffs.value, id, "shown");
    await api.agentRuntimeMarkHandoffShown(id);
  }

  async function rejectHandoff(id: string) {
    await api.agentRuntimeRejectHandoff(id);
    handoffs.value = updateHandoffStatus(handoffs.value, id, "rejected");
    const state = deriveHandoffDialogState(handoffs.value, activeHandoffId.value === id ? null : activeHandoffId.value);
    activeHandoffId.value = state.active?.id ?? null;
    handoffDialogOpen.value = state.open;
  }

  function setActiveHandoff(id: string) {
    activeHandoffId.value = id;
    const item = handoffs.value.find((handoff) => handoff.id === id);
    if (item?.status === "queued") void markHandoffShown(id);
  }

  return {
    selection,
    selectedSql,
    handoffs,
    activeHandoff,
    handoffDialogOpen,
    setSelection,
    setSelectedSql,
    scheduleSync,
    syncNow,
    loadHandoffs,
    markHandoffShown,
    rejectHandoff,
    setActiveHandoff,
  };
});
