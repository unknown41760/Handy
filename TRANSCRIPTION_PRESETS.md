# Transcription Presets

This source tree adds optional hotkey-driven transcription presets to Handy.

## What a preset stores

Each preset has its own:

- stable generated ID
- name
- optional global keyboard shortcut chosen explicitly by the user
- speech-to-text model (or **Use current model**)
- input language
- built-in **Translate to English** choice when the model supports it
- optional AI post-processing
- optional saved post-processing prompt

Using a preset does **not** overwrite Handy's normal selected model, language, translation, or post-processing prompt. At recording start the preset is resolved into an immutable `AppSettings` snapshot. That snapshot is passed explicitly through model loading, streaming/batch transcription, and output post-processing. History retry explicitly pins the normal persistent model/settings snapshot, CLI transcription keeps its explicit CLI-loaded model behavior, and no non-preset manager path reads preset runtime state.

If a preset uses a different speech model, Handy loads that exact model for the preset operation. A later preset or normal transcription ensures its own requested model is resident before it transcribes.

## Defaults and lifecycle

Fresh installs have zero extra transcription presets. Open **Settings → Presets** and choose **Add Preset** to create one. Up to 10 presets can be created through the UI/backend; loading settings never truncates an existing collection.

A new preset starts disabled and has no shortcut. The user must choose **Add Shortcut** and record a key combination before the preset can be enabled. The first chosen shortcut becomes that binding's reset/default value; Handy never guesses a preset hotkey.

Deleting a preset removes its stored shortcut binding. Enabled shortcuts are unregistered before deletion is persisted. Delete, disable, and shortcut-replacement operations are rejected while that preset owns the active recording so the stop/release event source cannot disappear mid-recording. Generic settings normalization also removes orphan `preset_*` bindings that no longer refer to a stored preset.

There is intentionally no special migration for the earlier private build that manufactured exactly three preset slots. Those persisted presets remain ordinary presets until manually deleted; no version-specific compatibility path is carried forward.

For translation targets other than English, create a post-processing prompt such as “Translate the transcription to Spanish and output only the translation”, then select that prompt in the preset and enable AI post-processing.

## Validation and recovery

- An enabled preset must have a shortcut and resolve to an existing downloaded model.
- Shortcut assignment is checked against Handy's other stored bindings even while a preset is disabled, so conflicts are rejected before enable-time registration.
- Switching keyboard implementations is transactional: if target registration fails, Handy restores the previous implementation and bindings.
- An explicit model selected for a preset is validated whenever the preset is saved. Presets using **Use current model** are reconciled when the current model changes so unsupported auto-detection/translation state is not left persisted.
- AI post-processing cannot be enabled without a valid non-empty saved prompt and a model configured for the selected AI provider.
- Deleting a model disables presets that depended on it; explicit references to that deleted model are reset to **Use current model**.
- Deleting a post-processing prompt disables post-processing for presets that referenced it.
- Settings loading normalizes existing preset names/languages, clears stale prompt references, and removes orphan preset bindings without manufacturing new presets.
- Older Handy settings files that never contained presets deserialize to an empty preset list.

## Architecture notes

Preset IDs use the `preset_<uuid>` namespace and remain stable across settings writes and restarts. The shortcut binding uses the same ID, while runtime validity is checked against the actual persisted preset collection. Load normalization repairs malformed or duplicate preset identities by assigning a fresh ID and disabling ambiguous entries rather than allowing them to alias static shortcuts or another preset.

The global **Post Processing** toggle remains a master/privacy switch. Presets remember their configured prompt while it is off, but no preset sends text to an AI provider until the global toggle is enabled.

History **Re-transcribe** intentionally uses the current normal model/settings, not the preset configuration that may have created the original entry. History does not currently persist a full transcription-operation snapshot.

## Validation

Run the normal project checks after changing preset behavior:

```bash
bun run format:check
bun run lint
bun run check:translations
bun run check:model-languages
bun run test:keyboard
bun run test:presets:frontend
cd src-tauri
cargo fmt -- --check
cargo clippy
cargo test
```
