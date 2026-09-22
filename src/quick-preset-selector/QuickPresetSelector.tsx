import {
  useEffect,
  useState,
  type CSSProperties,
  type KeyboardEvent,
} from "react";
import { listen } from "@tauri-apps/api/event";
import {
  commands,
  type ActivePresetSelection,
  type QuickPresetSelectorPayload,
  type QuickPresetSlot,
} from "@/bindings";
import { useTranslation } from "react-i18next";

const CENTER = 210;
const INNER_RADIUS = 69;
const OUTER_RADIUS = 174;
const LABEL_RADIUS = 122;
const DEAD_ZONE = 58;
const SLOT_ANGLE = Math.PI / 4;
const HALF_PETAL_ANGLE = SLOT_ANGLE / 2 - 0.035;

const polarPoint = (radius: number, angle: number) => ({
  x: CENTER + Math.sin(angle) * radius,
  y: CENTER - Math.cos(angle) * radius,
});

const point = ({ x, y }: { x: number; y: number }) =>
  `${x.toFixed(2)} ${y.toFixed(2)}`;

/** A softly rounded annular sector, matching the flower-petal reference. */
const petalPath = (slot: number): string => {
  const centerAngle = (slot - 1) * SLOT_ANGLE;
  const leftAngle = centerAngle - HALF_PETAL_ANGLE;
  const rightAngle = centerAngle + HALF_PETAL_ANGLE;
  const outerRoundAngle = 0.06;
  const innerRoundAngle = 0.09;
  const cornerDepth = 12;

  const outerStart = polarPoint(OUTER_RADIUS, leftAngle + outerRoundAngle);
  const outerEnd = polarPoint(OUTER_RADIUS, rightAngle - outerRoundAngle);
  const outerRight = polarPoint(OUTER_RADIUS, rightAngle);
  const outerRightInset = polarPoint(OUTER_RADIUS - cornerDepth, rightAngle);
  const innerRightOutset = polarPoint(INNER_RADIUS + cornerDepth, rightAngle);
  const innerRight = polarPoint(INNER_RADIUS, rightAngle);
  const innerArcStart = polarPoint(INNER_RADIUS, rightAngle - innerRoundAngle);
  const innerArcEnd = polarPoint(INNER_RADIUS, leftAngle + innerRoundAngle);
  const innerLeft = polarPoint(INNER_RADIUS, leftAngle);
  const innerLeftOutset = polarPoint(INNER_RADIUS + cornerDepth, leftAngle);
  const outerLeftInset = polarPoint(OUTER_RADIUS - cornerDepth, leftAngle);
  const outerLeft = polarPoint(OUTER_RADIUS, leftAngle);

  return [
    `M ${point(outerStart)}`,
    `A ${OUTER_RADIUS} ${OUTER_RADIUS} 0 0 1 ${point(outerEnd)}`,
    `Q ${point(outerRight)} ${point(outerRightInset)}`,
    `L ${point(innerRightOutset)}`,
    `Q ${point(innerRight)} ${point(innerArcStart)}`,
    `A ${INNER_RADIUS} ${INNER_RADIUS} 0 0 0 ${point(innerArcEnd)}`,
    `Q ${point(innerLeft)} ${point(innerLeftOutset)}`,
    `L ${point(outerLeftInset)}`,
    `Q ${point(outerLeft)} ${point(outerStart)}`,
    "Z",
  ].join(" ");
};

const slotFromPoint = (x: number, y: number): number | null => {
  const dx = x - CENTER;
  const dy = y - CENTER;
  if (Math.hypot(dx, dy) < DEAD_ZONE) return null;
  const angle = (Math.atan2(dx, -dy) + Math.PI * 2) % (Math.PI * 2);
  return (Math.round(angle / SLOT_ANGLE) % 8) + 1;
};

const slotLabel = (
  slot: QuickPresetSlot,
  defaultLabel: string,
  emptyLabel: string,
) => (slot.slot === 1 ? defaultLabel : slot.name || emptyLabel);

export default function QuickPresetSelector() {
  const { t } = useTranslation();
  const [payload, setPayload] = useState<QuickPresetSelectorPayload | null>(
    null,
  );
  const [highlightedSlot, setHighlightedSlot] = useState<number | null>(null);
  const [confirmation, setConfirmation] =
    useState<ActivePresetSelection | null>(null);
  const [appearanceKey, setAppearanceKey] = useState(0);

  useEffect(() => {
    void commands.getQuickPresetSelectorPayload().then((initialPayload) => {
      setPayload(initialPayload);
      setAppearanceKey((current) => current + 1);
    });
    const show = listen<QuickPresetSelectorPayload>(
      "show-quick-preset-selector",
      (event) => {
        setPayload(event.payload);
        setHighlightedSlot(null);
        setConfirmation(null);
        setAppearanceKey((current) => current + 1);
      },
    );
    const confirmed = listen<ActivePresetSelection>(
      "quick-preset-confirmed",
      (event) => {
        setConfirmation(event.payload);
        setHighlightedSlot(null);
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
    const highlighted = listen<number | null>(
      "quick-preset-highlighted",
      (event) => setHighlightedSlot(event.payload),
    );
    return () => {
      show.then((unlisten) => unlisten());
      confirmed.then((unlisten) => unlisten());
      highlighted.then((unlisten) => unlisten());
    };
  }, []);

  useEffect(() => {
    const handleKeyDown = (event: globalThis.KeyboardEvent) => {
      if (event.key === "Escape") {
        void commands.closeQuickPresetSelector();
        return;
      }
      const slot = Number(event.key);
      if (Number.isInteger(slot) && slot >= 1 && slot <= 8) {
        const target = payload?.slots.find((item) => item.slot === slot);
        if (target && (slot === 1 || target.preset_id)) {
          setHighlightedSlot(slot);
          void commands.selectQuickPresetSlot(slot);
        }
      }
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [payload]);

  const selectFromKeyboard = (
    event: KeyboardEvent<SVGGElement>,
    slot: QuickPresetSlot,
  ) => {
    if (event.key !== "Enter" && event.key !== " ") return;
    event.preventDefault();
    if (slot.slot === 1 || slot.preset_id) {
      void commands.selectQuickPresetSlot(slot.slot);
    }
  };

  const defaultLabel = t("settings.presets.defaultPreset");
  const emptyLabel = t("settings.presets.emptyQuickSlot");

  return (
    <main
      key={appearanceKey}
      className={`quick-selector ${confirmation ? "is-confirming" : ""}`}
      onMouseMove={(event) =>
        setHighlightedSlot(slotFromPoint(event.clientX, event.clientY))
      }
      onMouseLeave={() => setHighlightedSlot(null)}
    >
      <div className="flower-ambient" aria-hidden="true" />
      <svg
        className="quick-selector-flower"
        viewBox="0 0 420 420"
        role="group"
        aria-label={t("settings.presets.quickSelector")}
      >
        <defs>
          <radialGradient id="petal-surface" cx="50%" cy="30%" r="80%">
            <stop offset="0%" stopColor="rgba(78, 76, 86, 0.72)" />
            <stop offset="100%" stopColor="rgba(24, 23, 29, 0.88)" />
          </radialGradient>
          <radialGradient id="petal-active" cx="48%" cy="50%" r="75%">
            <stop offset="0%" stopColor="rgba(210, 84, 177, 0.62)" />
            <stop offset="100%" stopColor="rgba(94, 39, 83, 0.82)" />
          </radialGradient>
          <filter id="pink-glow" x="-60%" y="-60%" width="220%" height="220%">
            <feGaussianBlur stdDeviation="7" result="blur" />
            <feFlood floodColor="#ff63da" floodOpacity="0.72" />
            <feComposite in2="blur" operator="in" />
            <feMerge>
              <feMergeNode />
              <feMergeNode in="SourceGraphic" />
            </feMerge>
          </filter>
          <filter id="soft-shadow" x="-30%" y="-30%" width="160%" height="160%">
            <feDropShadow dx="0" dy="8" stdDeviation="9" floodOpacity="0.42" />
          </filter>
        </defs>

        {(payload?.slots ?? []).map((slot) => {
          const angle = (slot.slot - 1) * SLOT_ANGLE;
          const labelPosition = polarPoint(LABEL_RADIUS, angle);
          const populated = slot.slot === 1 || Boolean(slot.preset_id);
          const highlighted = highlightedSlot === slot.slot;
          const label = slotLabel(slot, defaultLabel, emptyLabel);
          const pushDistance = highlighted && populated ? 4 : 0;
          const style = {
            "--slot-index": slot.slot - 1,
            "--push-x": `${Math.sin(angle) * pushDistance}px`,
            "--push-y": `${-Math.cos(angle) * pushDistance}px`,
            transformOrigin: `${labelPosition.x}px ${labelPosition.y}px`,
          } as CSSProperties;

          return (
            <g
              key={slot.slot}
              className={`quick-petal ${slot.active ? "active" : ""} ${
                highlighted ? "highlighted" : ""
              } ${populated ? "populated" : "empty"}`}
              style={style}
              role="button"
              aria-label={`${slot.slot}. ${label}`}
              aria-disabled={!populated}
              tabIndex={populated ? 0 : -1}
              onClick={() =>
                populated && void commands.selectQuickPresetSlot(slot.slot)
              }
              onKeyDown={(event) => selectFromKeyboard(event, slot)}
            >
              <g className="quick-petal-motion">
                <path
                  className="quick-petal-surface"
                  d={petalPath(slot.slot)}
                />
                <foreignObject
                  className="quick-petal-copy"
                  x={labelPosition.x - 49}
                  y={labelPosition.y - 29}
                  width="98"
                  height="58"
                  aria-hidden="true"
                >
                  <div className="quick-petal-label">
                    <span className="quick-slot-number">{slot.slot}</span>
                    <span className="quick-slot-name">{label}</span>
                  </div>
                </foreignObject>
              </g>
            </g>
          );
        })}

        <g className="quick-selector-center" aria-hidden="true">
          <circle
            className="quick-selector-center-glow"
            cx={CENTER}
            cy={CENTER}
            r="58"
          />
          <circle
            className="quick-selector-center-surface"
            cx={CENTER}
            cy={CENTER}
            r="55"
          />
          {confirmation ? (
            <g className="quick-confirmation-mark">
              <path d="M190 208.5 203.5 222 231 194.5" />
              <foreignObject x="165" y="226" width="90" height="28">
                <div className="quick-confirmation-name">
                  {confirmation.preset_id
                    ? confirmation.preset_name
                    : defaultLabel}
                </div>
              </foreignObject>
            </g>
          ) : (
            <g className="quick-cursor-mark">
              <path
                className="quick-cursor-fill"
                d="M196 184v48l11-10 8 18 9-4-8-18h15z"
              />
              <path
                className="quick-cursor-edge"
                d="M196 184v48l11-10 8 18 9-4-8-18h15z"
              />
            </g>
          )}
        </g>
      </svg>
    </main>
  );
}
