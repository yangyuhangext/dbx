import type { DatabaseType } from "@/types/database";
import { supportsDriverManagement } from "./databaseCapabilities";

export interface AgentDriverInstallState {
  db_type: string;
  installed: boolean;
}

export function showAgentDriverInstallHint(
  dbType: DatabaseType | undefined,
  drivers: readonly AgentDriverInstallState[],
): boolean {
  if (!supportsDriverManagement(dbType)) return false;
  return drivers.find((driver) => driver.db_type === dbType)?.installed !== true;
}
