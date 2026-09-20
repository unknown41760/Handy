import { commands } from "@/bindings";

export async function initializeInputRuntime(): Promise<void> {
  const [enigoResult, shortcutsResult] = await Promise.all([
    commands.initializeEnigo(),
    commands.initializeShortcuts(),
  ]);

  const errors: string[] = [];
  if (enigoResult.status === "error") {
    errors.push(String(enigoResult.error));
  }
  if (shortcutsResult.status === "error") {
    errors.push(String(shortcutsResult.error));
  }

  if (errors.length > 0) {
    throw new Error(errors.join("; "));
  }
}
