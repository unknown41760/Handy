import { describe, expect, test } from "bun:test";
import { getEffectiveDisplayModelId } from "../src/components/model-selector/effectiveModel";

describe("effective preset model display", () => {
  test("uses the backend-resolved preset model instead of the global model", () => {
    expect(
      getEffectiveDisplayModelId(
        {
          preset_id: "preset_russian",
          preset_name: "Russian",
          model_id: "whisper-large-v3-turbo",
          recording: true,
        },
        "whisper-small",
      ),
    ).toBe("whisper-large-v3-turbo");
  });

  test("uses the resolved global model for Default or Use Current Model", () => {
    expect(
      getEffectiveDisplayModelId(
        {
          preset_id: "preset_current",
          preset_name: "Current",
          model_id: "whisper-medium",
          recording: false,
        },
        "whisper-medium",
      ),
    ).toBe("whisper-medium");
  });

  test("falls back to the global model before backend target initialization", () => {
    expect(getEffectiveDisplayModelId(null, "whisper-small")).toBe(
      "whisper-small",
    );
  });
});
