import { MockApiClient } from "./mock";
import {
  createTauriBridge,
  isTauriRuntime,
  TauriApiClient,
  type TypedCommandBridge,
} from "./tauri";
import type { ApiClient } from "../types";

export type { TypedCommandBridge } from "./tauri";

/**
 * Browser development intentionally uses an in-memory client. Packaged Tauri
 * builds are detected through the private runtime marker and use only the
 * fixed handlers declared by `TauriApiClient`.
 */
export function createApiClient(
  bridge: TypedCommandBridge | undefined = window.__BROWS_BRIDGE__,
): ApiClient {
  if (bridge) return new TauriApiClient(bridge);
  return isTauriRuntime() ? new TauriApiClient(createTauriBridge()) : new MockApiClient();
}
