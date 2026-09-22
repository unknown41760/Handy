import { useEffect, useMemo, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { commands } from "@/bindings";
import { useTranslation } from "react-i18next";

interface QuickPresetSlot {
  slot: number;
  preset_id: string | null;
  name: string;
  active: boolean;
}

interface SelectorPayload {
  slots: QuickPresetSlot[];
  active_preset_id: string | null;
}

interface ActivePresetSelection {
  preset_id: string | null;
  preset_name: string;
  model_id: string;
}

const CENTER = 210;
const RADIUS = 142;
const DEAD_ZONE = 42;

const slotFromPoint = (x: number, y: number): number | null => {
  const dx = x - CENTER;
  const dy = y - CENTER;
  if (Math.hypot(dx, dy) < DEAD_ZONE) return null;
  const angle = (Math.atan2(dx, -dy) + Math.PI * 2) % (Math.PI * 2);
  return (Math.round(angle / (Math.PI / 4)) % 8) + 1;
};

export default function QuickPresetSelector() {
  const { t } = useTranslation();
  const [payload, setPayload] = useState<SelectorPayload | null>(null);
  const [highlightedSlot, setHighlightedSlot] = useState<number | null>(null);
  const [confirmation, setConfirmation] =
    useState<ActivePresetSelection | null>(null);

  useEffect(() => {
    void commands.getQuickPresetSelectorPayload().then(setPayload);
    const show = listen<SelectorPayload>(
      "show-quick-preset-selector",
      (event) => {
        setPayload(event.payload);
        setHighlightedSlot(null);
        setConfirmation(null);
      },
    );
    const confirmed = listen<ActivePresetSelection>(
      "quick-preset-confirmed",
      (event) => {
        setConfirmation(event.payload);
        setPayload((current) =>
          current
            ? {
                ...current,
                active_preset_id: event.payload.preset_id,
                slots: current.slots.map((slot) => ({
                  ...slot,
                  active:
                    slot.preset_id === event.payload.preset_id ||
                    (slot.slot === 1 && event.payload.preset_id === null),
                })),
              }
            : current,
        );
      },
    );
    return () => {
      show.then((unlisten) => unlisten());
      confirmed.then((unlisten) => unlisten());
    };
  }, []);

  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        void commands.closeQuickPresetSelector();
        return;
      }
      const slot = Number(event.key);
      if (Number.isInteger(slot) && slot >= 1 && slot <= 8) {
        const target = payload?.slots.find((item) => item.slot === slot);
        if (target && (slot === 1 || target.preset_id)) {
          void commands.selectQuickPresetSlot(slot);
        }
      }
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [payload]);

  const slots = useMemo(() => payload?.slots ?? [], [payload]);

  return (
    <main
      className="quick-selector"
      onMouseMove={(event) =>
        setHighlightedSlot(slotFromPoint(event.clientX, event.clientY))
      }
      onMouseLeave={() => setHighlightedSlot(null)}
    >
      <div className="quick-selector-ring" aria-hidden="true" />
      {slots.map((slot) => {
        const angle = ((slot.slot - 1) * Math.PI) / 4;
        const x = CENTER + Math.sin(angle) * RADIUS;
        const y = CENTER - Math.cos(angle) * RADIUS;
        const populated = slot.slot === 1 || Boolean(slot.preset_id);
        return (
          <button
            key={slot.slot}
            type="button"
            className={`quick-slot ${slot.active ? "active" : ""} ${
              highlightedSlot === slot.slot ? "highlighted" : ""
            } ${populated ? "populated" : "empty"}`}
            style={{ left: x, top: y }}
            disabled={!populated}
            onClick={() => void commands.selectQuickPresetSlot(slot.slot)}
          >
            <span className="quick-slot-number">{slot.slot}</span>
            <span className="quick-slot-name">
              {slot.slot === 1
                ? t("settings.presets.defaultPreset")
                : slot.name || t("settings.presets.emptyQuickSlot")}
            </span>
          </button>
        );
      })}
      <div className="quick-selector-center">
        {confirmation ? (
          <>
            <span className="quick-selector-caption">
              {t("settings.presets.activePreset")}
            </span>
            <strong>
              {confirmation.preset_id
                ? confirmation.preset_name
                : t("settings.presets.defaultPreset")}
            </strong>
          </>
        ) : (
          <span>{t("settings.presets.moveToSelect")}</span>
        )}
      </div>
    </main>
  );
}
