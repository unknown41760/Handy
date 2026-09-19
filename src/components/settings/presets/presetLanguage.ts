import type { ModelInfo } from "@/bindings";
import {
  SELECTABLE_LANGUAGES,
  supportsLanguageCode,
} from "@/lib/constants/languages";

type LanguageModel = Pick<
  ModelInfo,
  "supported_languages" | "supports_language_detection"
>;

/**
 * Keep a preset's persisted language valid for the selected model.
 * Preserve the user's explicit language when possible; otherwise prefer
 * auto-detection, then English, then the first language Handy can expose in
 * the picker for that model.
 */
export const normalizePresetLanguageForModel = (
  language: string,
  model?: LanguageModel,
): string => {
  const requested = language.trim() || "auto";
  if (!model || model.supported_languages.length === 0) {
    return requested;
  }

  if (requested === "auto") {
    if (model.supports_language_detection) return "auto";
  } else if (supportsLanguageCode(model.supported_languages, requested)) {
    return requested;
  }

  if (model.supports_language_detection) return "auto";

  if (supportsLanguageCode(model.supported_languages, "en")) {
    return "en";
  }

  return (
    SELECTABLE_LANGUAGES.find(
      (candidate) =>
        candidate.value !== "auto" &&
        supportsLanguageCode(model.supported_languages, candidate.value),
    )?.value ?? requested
  );
};
