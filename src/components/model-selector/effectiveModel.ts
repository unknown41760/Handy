import type { EffectiveTranscriptionTarget } from "@/bindings";

/** The footer communicates the backend-resolved processing target. The global
 * selection is only a startup fallback before that target has loaded. */
export const getEffectiveDisplayModelId = (
  target: EffectiveTranscriptionTarget | null,
  globalModelId: string,
): string => target?.model_id ?? globalModelId;
