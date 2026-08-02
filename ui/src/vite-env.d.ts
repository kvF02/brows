/// <reference types="vite/client" />

interface Window {
  /** Optional, narrowly scoped bridge installed by the Tauri shell. */
  __BROWS_BRIDGE__?: {
    invoke(handler: string, args?: Record<string, unknown>): Promise<unknown>;
  };

  /** Private marker installed by the Tauri runtime, never by browser previews. */
  __TAURI_INTERNALS__?: unknown;
}
