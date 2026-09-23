import React, { useEffect, useMemo, useState } from "react";
import { toast } from "sonner";
import { useTranslation } from "react-i18next";
import {
  commands,
  type QuickSelectorPosition,
  type TranscriptionPreset,
} from "@/bindings";
import { useSettings } from "@/hooks/useSettings";
import { useModelStore } from "@/stores/modelStore";
import {
  SELECTABLE_LANGUAGES,
  supportsLanguageCode,
} from "@/lib/constants/languages";
import { normalizePresetLanguageForModel } from "./presetLanguage";
import { SettingsGroup } from "@/components/ui/SettingsGroup";
import { SettingContainer } from "@/components/ui/SettingContainer";
import { ToggleSwitch } from "@/components/ui/ToggleSwitch";
import { Input } from "@/components/ui/Input";
import { Select } from "@/components/ui/Select";
import { ShortcutInput } from "../ShortcutInput";
import { Button } from "@/components/ui/Button";

const MAX_PRESETS = 10;

interface PresetCardProps {
  preset: TranscriptionPreset;
}

const PresetCard: React.FC<PresetCardProps> = ({ preset }) => {
  const { t } = useTranslation();
  const { settings, refreshSettings } = useSettings();
  const { models, currentModel } = useModelStore();
  const [draftName, setDraftName] = useState(preset.name);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    setDraftName(preset.name);
  }, [preset.name]);

  const effectiveModelId = preset.model_id || currentModel;
  const selectedModel = models.find((model) => model.id === effectiveModelId);
  const downloadedModels = models.filter((model) => model.is_downloaded);

  const modelOptions = useMemo(
    () => [
      {
        value: "__current__",
        label: t("settings.presets.useCurrentModel"),
      },
      ...downloadedModels.map((model) => ({
        value: model.id,
        label: model.name,
      })),
    ],
    [downloadedModels, t],
  );

  const languageOptions = useMemo(() => {
    if (!selectedModel || selectedModel.supported_languages.length === 0) {
      return SELECTABLE_LANGUAGES.map((language) => ({
        value: language.value,
        label: language.label,
      }));
    }

    return SELECTABLE_LANGUAGES.filter((language) => {
      if (language.value === "auto") {
        return selectedModel.supports_language_detection;
      }
      return supportsLanguageCode(
        selectedModel.supported_languages,
        language.value,
      );
    }).map((language) => ({
      value: language.value,
      label: language.label,
    }));
  }, [selectedModel]);

  const promptOptions = useMemo(
    () =>
      (settings?.post_process_prompts ?? []).map((prompt) => ({
        value: prompt.id,
        label: prompt.name,
      })),
    [settings?.post_process_prompts],
  );

  const savePreset = async (next: TranscriptionPreset) => {
    setSaving(true);
    try {
      const result = await commands.updateTranscriptionPreset(next);
      if (result.status === "error") {
        throw new Error(result.error);
      }
      await refreshSettings();
    } catch (error) {
      toast.error(t("settings.presets.saveFailed"), {
        description: String(error),
      });
    } finally {
      setSaving(false);
    }
  };

  const saveName = async () => {
    const trimmed = draftName.trim();
    if (!trimmed || trimmed === preset.name) {
      setDraftName(preset.name);
      return;
    }
    await savePreset({ ...presetForSave, name: trimmed });
  };

  const normalizedLanguage = normalizePresetLanguageForModel(
    preset.language,
    selectedModel,
  );
  const presetForSave = { ...preset, language: normalizedLanguage };
  const translateAvailable = selectedModel?.supports_translation ?? false;
  const globalPostProcessingEnabled = settings?.post_process_enabled ?? false;
  const hasShortcut = Boolean(settings?.bindings?.[preset.id]);
  const quickSlotOptions = useMemo(
    () => [
      { value: "none", label: t("settings.presets.quickSlotNone") },
      ...Array.from({ length: 7 }, (_, index) => ({
        value: String(index + 2),
        label: t("settings.presets.quickSlotNumber", { slot: index + 2 }),
      })),
    ],
    [t],
  );

  const deletePreset = async () => {
    setSaving(true);
    try {
      const result = await commands.deleteTranscriptionPreset(preset.id);
      if (result.status === "error") {
        throw new Error(result.error);
      }
      await refreshSettings();
    } catch (error) {
      toast.error(t("settings.presets.deleteFailed"), {
        description: String(error),
      });
    } finally {
      setSaving(false);
    }
  };

  return (
    <SettingsGroup title={preset.name}>
      <ToggleSwitch
        checked={preset.enabled}
        onChange={(enabled) => savePreset({ ...presetForSave, enabled })}
        disabled={saving || !hasShortcut}
        label={t("settings.presets.enabled")}
        description={
          hasShortcut
            ? t("settings.presets.enabledDescription")
            : t("settings.presets.shortcutRequired")
        }
        grouped={true}
      />

      <SettingContainer
        title={t("settings.presets.name")}
        description={t("settings.presets.nameDescription")}
        grouped={true}
      >
        <Input
          value={draftName}
          onChange={(event) => setDraftName(event.target.value)}
          onBlur={saveName}
          onKeyDown={(event) => {
            if (event.key === "Enter") {
              event.currentTarget.blur();
            }
          }}
          disabled={saving}
          className="w-52"
        />
      </SettingContainer>

      <ShortcutInput
        shortcutId={preset.id}
        grouped={true}
        allowCreate
        title={t("settings.general.shortcut.title")}
        description={
          hasShortcut
            ? t("settings.presets.shortcutDescription", { name: preset.name })
            : t("settings.presets.shortcutRequired")
        }
      />

      <SettingContainer
        title={t("settings.presets.quickSlot")}
        description={t("settings.presets.quickSlotDescription")}
        grouped={true}
      >
        <Select
          value={preset.quick_slot ? String(preset.quick_slot) : "none"}
          options={quickSlotOptions}
          isClearable={false}
          disabled={saving}
          onChange={(value) =>
            void savePreset({
              ...presetForSave,
              quick_slot: value && value !== "none" ? Number(value) : null,
            })
          }
        />
      </SettingContainer>

      <SettingContainer
        title={t("settings.presets.model")}
        description={t("settings.presets.modelDescription")}
        grouped={true}
        layout="stacked"
      >
        <Select
          value={preset.model_id || "__current__"}
          options={modelOptions}
          isClearable={false}
          disabled={saving}
          onChange={(value) => {
            const modelId = !value || value === "__current__" ? "" : value;
            const nextModel = models.find(
              (model) => model.id === (modelId || currentModel),
            );
            void savePreset({
              ...presetForSave,
              model_id: modelId,
              language: normalizePresetLanguageForModel(
                normalizedLanguage,
                nextModel,
              ),
              translate_to_english:
                preset.translate_to_english &&
                (nextModel?.supports_translation ?? false),
            });
          }}
        />
      </SettingContainer>

      <SettingContainer
        title={t("settings.presets.language")}
        description={t("settings.presets.languageDescription")}
        grouped={true}
        layout="stacked"
      >
        <Select
          value={normalizedLanguage}
          options={languageOptions}
          isClearable={false}
          disabled={saving || languageOptions.length === 0}
          onChange={(value) =>
            savePreset({ ...presetForSave, language: value || "auto" })
          }
        />
      </SettingContainer>

      <ToggleSwitch
        checked={preset.translate_to_english}
        onChange={(translate_to_english) =>
          savePreset({ ...presetForSave, translate_to_english })
        }
        disabled={saving || !translateAvailable}
        label={t("settings.presets.translateEnglish")}
        description={
          translateAvailable
            ? t("settings.presets.translateEnglishDescription")
            : t("settings.presets.translateUnavailable")
        }
        grouped={true}
      />

      <ToggleSwitch
        checked={preset.post_process}
        onChange={(post_process) => {
          if (!post_process) {
            void savePreset({ ...presetForSave, post_process: false });
            return;
          }

          const promptId =
            preset.post_process_prompt_id &&
            promptOptions.some(
              (option) => option.value === preset.post_process_prompt_id,
            )
              ? preset.post_process_prompt_id
              : promptOptions[0]?.value;

          if (!promptId) {
            toast.error(t("settings.presets.promptRequired"));
            return;
          }

          void savePreset({
            ...presetForSave,
            post_process: true,
            post_process_prompt_id: promptId,
          });
        }}
        disabled={
          saving ||
          promptOptions.length === 0 ||
          (!globalPostProcessingEnabled && !preset.post_process)
        }
        label={t("settings.presets.postProcess")}
        description={
          globalPostProcessingEnabled
            ? t("settings.presets.postProcessDescription")
            : t("settings.presets.postProcessGlobalDisabled")
        }
        grouped={true}
      />

      {preset.post_process && (
        <SettingContainer
          title={t("settings.presets.prompt")}
          description={t("settings.presets.promptDescription")}
          grouped={true}
          layout="stacked"
        >
          <Select
            value={preset.post_process_prompt_id}
            options={promptOptions}
            placeholder={t("settings.presets.choosePrompt")}
            disabled={saving}
            onChange={(value) =>
              savePreset({ ...presetForSave, post_process_prompt_id: value })
            }
          />
        </SettingContainer>
      )}

      <SettingContainer
        title={t("settings.presets.deletePreset")}
        description={t("settings.presets.deleteDescription")}
        grouped={true}
      >
        <Button
          size="sm"
          variant="danger-ghost"
          onClick={() => void deletePreset()}
          disabled={saving}
        >
          {t("settings.presets.deletePreset")}
        </Button>
      </SettingContainer>
    </SettingsGroup>
  );
};

export const PresetsSettings: React.FC = () => {
  const { t } = useTranslation();
  const { settings, refreshSettings, updateSetting, isUpdating } =
    useSettings();
  const [creating, setCreating] = useState(false);
  const [selectingActive, setSelectingActive] = useState(false);
  const presets = settings?.transcription_presets ?? [];
  const atLimit = presets.length >= MAX_PRESETS;
  const activePresetOptions = useMemo(
    () => [
      { value: "default", label: t("settings.presets.defaultPreset") },
      ...presets.map((preset) => ({ value: preset.id, label: preset.name })),
    ],
    [presets, t],
  );

  const selectActivePreset = async (value: string | null) => {
    setSelectingActive(true);
    try {
      const result = await commands.setActiveTranscriptionPreset(
        !value || value === "default" ? null : value,
      );
      if (result.status === "error") throw new Error(result.error);
      await refreshSettings();
    } catch (error) {
      toast.error(t("settings.presets.activePresetFailed"), {
        description: String(error),
      });
    } finally {
      setSelectingActive(false);
    }
  };

  const createPreset = async () => {
    setCreating(true);
    try {
      const result = await commands.createTranscriptionPreset();
      if (result.status === "error") {
        throw new Error(result.error);
      }
      await refreshSettings();
    } catch (error) {
      toast.error(t("settings.presets.createFailed"), {
        description: String(error),
      });
    } finally {
      setCreating(false);
    }
  };

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      <div className="px-4">
        <h1 className="text-lg font-semibold">{t("settings.presets.title")}</h1>
        <p className="text-sm text-mid-gray mt-1">
          {t("settings.presets.description")}
        </p>
      </div>

      {presets.length === 0 ? (
        <div className="mx-4 rounded-lg border border-dashed border-mid-gray/40 px-6 py-10 text-center">
          <div className="font-medium">{t("settings.presets.emptyTitle")}</div>
          <p className="text-sm text-mid-gray mt-1">
            {t("settings.presets.emptyDescription")}
          </p>
          <Button
            size="md"
            className="mt-4"
            onClick={() => void createPreset()}
            disabled={creating || atLimit}
          >
            {t("settings.presets.addPreset")}
          </Button>
        </div>
      ) : (
        <>
          <SettingsGroup title={t("settings.presets.quickSelector")}>
            <SettingContainer
              title={t("settings.presets.activePreset")}
              description={t("settings.presets.activePresetDescription")}
              grouped={true}
              layout="stacked"
            >
              <Select
                value={settings?.active_transcription_preset_id ?? "default"}
                options={activePresetOptions}
                isClearable={false}
                disabled={selectingActive}
                onChange={(value) => void selectActivePreset(value)}
              />
            </SettingContainer>
            <ShortcutInput
              shortcutId="quick_preset_selector"
              grouped={true}
              title={t("settings.presets.quickSelectorShortcut")}
              description={t(
                "settings.presets.quickSelectorShortcutDescription",
              )}
            />
            <SettingContainer
              title={t("settings.presets.quickSelectorPosition")}
              description={t(
                "settings.presets.quickSelectorPositionDescription",
              )}
              grouped={true}
              layout="stacked"
            >
              <Select
                value={settings?.quick_selector_position ?? "auto"}
                options={[
                  {
                    value: "mouse",
                    label: t("settings.presets.quickSelectorAtMouse"),
                  },
                  {
                    value: "bottom",
                    label: t("settings.presets.quickSelectorAtBottom"),
                  },
                  {
                    value: "auto",
                    label: t("settings.presets.quickSelectorAuto"),
                  },
                ]}
                isClearable={false}
                disabled={isUpdating("quick_selector_position")}
                onChange={(value) =>
                  value &&
                  void updateSetting(
                    "quick_selector_position",
                    value as QuickSelectorPosition,
                  )
                }
              />
            </SettingContainer>
          </SettingsGroup>
          {presets.map((preset) => (
            <PresetCard key={preset.id} preset={preset} />
          ))}
          <div className="px-4">
            <Button
              size="md"
              onClick={() => void createPreset()}
              disabled={creating || atLimit}
            >
              {t("settings.presets.addPreset")}
            </Button>
          </div>
        </>
      )}

      {atLimit && (
        <p className="px-4 text-xs text-mid-gray">
          {t("settings.presets.limitReached", { max: MAX_PRESETS })}
        </p>
      )}
    </div>
  );
};
