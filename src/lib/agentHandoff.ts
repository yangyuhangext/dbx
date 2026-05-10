export type AgentHandoffStatus = "queued" | "shown" | "approved" | "rejected" | "executed" | "failed";

export interface AgentHandoffItem {
  id: string;
  createdAt: string;
  createdBy: string;
  connectionId: string;
  connectionName: string;
  database?: string;
  title: string;
  description?: string;
  sql: string;
  operationClass: string;
  riskLevel: string;
  isProduction: boolean;
  status: AgentHandoffStatus;
  resultSummary?: string;
  error?: string;
}

export function isPendingHandoff(item: AgentHandoffItem): boolean {
  return item.status === "queued" || item.status === "shown";
}

export function mergeLoadedHandoffs(items: AgentHandoffItem[]): AgentHandoffItem[] {
  return items.filter(isPendingHandoff).sort((a, b) => Date.parse(a.createdAt) - Date.parse(b.createdAt));
}

export function updateHandoffStatus(
  items: AgentHandoffItem[],
  id: string,
  status: AgentHandoffStatus,
): AgentHandoffItem[] {
  return mergeLoadedHandoffs(items.map((item) => (item.id === id ? { ...item, status } : item)));
}

export function deriveHandoffDialogState(items: AgentHandoffItem[], activeId: string | null) {
  const active = (activeId ? items.find((item) => item.id === activeId) : undefined) ?? items[0] ?? null;
  return {
    open: items.length > 0,
    active,
  };
}
