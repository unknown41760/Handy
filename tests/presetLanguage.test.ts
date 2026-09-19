import { describe, expect, test } from "bun:test";
import { normalizePresetLanguageForModel } from "../src/components/settings/presets/presetLanguage";

const model = (
  supported_languages: string[],
  supports_language_detection: boolean,
) => ({ supported_languages, supports_language_detection });

describe("normalizePresetLanguageForModel", () => {
  test("preserves a supported explicit language", () => {
    expect(
      normalizePresetLanguageForModel("nl", model(["en", "nl"], false)),
    ).toBe("nl");
  });

  test("uses auto only when the model supports detection", () => {
    expect(
      normalizePresetLanguageForModel("auto", model(["en", "nl"], true)),
    ).toBe("auto");
    expect(
      normalizePresetLanguageForModel("auto", model(["en", "nl"], false)),
    ).toBe("en");
  });

  test("falls back to a supported selectable language when English is unavailable", () => {
    expect(normalizePresetLanguageForModel("auto", model(["de"], false))).toBe(
      "de",
    );
  });

  test("maps a Chinese recognition capability to a selectable script intent", () => {
    expect(normalizePresetLanguageForModel("auto", model(["zh"], false))).toBe(
      "zh-Hans",
    );
  });
});
