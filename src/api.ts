import { invoke } from "@tauri-apps/api/core";
import { mockInvoke } from "./mock";

export const isDesktop = () => "__TAURI_INTERNALS__" in window;

export async function call<T>(command: string, args: Record<string, unknown> = {}): Promise<T> {
  if (isDesktop()) return invoke<T>(command, args);
  return mockInvoke<T>(command, args);
}
