# Transcription Presets

This source tree adds three optional hotkey-driven transcription presets to Handy 0.9.6. Custom builds identify as `0.9.6+presets.3`.

## What a preset stores

Each preset has its own:

- name
- global keyboard shortcut
- speech-to-text model (or **Use current model**)
- input language
- built-in **Translate to English** choice when the model supports it
- optional AI post-processing
- optional saved post-processing prompt

Using a preset does **not** overwrite Handy's normal selected model, language, translation, or post-processing prompt. At recording start the preset is resolved into an immutable `AppSettings` snapshot. That snapshot is passed explicitly through model loading, streaming/batch transcription, and output post-processing. History retry explicitly pins the normal persistent model/settings snapshot, CLI transcription keeps its explicit CLI-loaded model behavior, and no non-preset manager path reads preset runtime state.

If a preset uses a different speech model, Handy loads that exact model for the preset operation. A later preset or normal transcription ensures its own requested model is resident before it transcribes.

## Defaults

Three preset slots are created but disabled so existing behavior is unchanged:

- Preset 1 — `Ctrl+Alt+1`
- Preset 2 — `Ctrl+Alt+2`
- Preset 3 — `Ctrl+Alt+3`

Open **Settings → Presets** to configure and enable them. Disabled preset shortcuts are stored but are not registered with Tauri shortcuts, HandyKeys, or the macOS Secure Input fallback.

For translation targets other than English, create a post-processing prompt such as “Translate the transcription to Spanish and output only the translation”, then select that prompt in the preset and enable AI post-processing.

## Validation and recovery

- An enabled preset must resolve to an existing downloaded model.
- An explicit model selected for a preset is validated whenever the preset is saved.
- AI post-processing cannot be enabled without a valid non-empty saved prompt and a model configured for the selected AI provider.
- Deleting a model disables presets that depended on it; explicit references to that deleted model are reset to **Use current model**.
- Deleting a post-processing prompt disables post-processing for presets that referenced it.
- Settings loading normalizes the fixed three preset slots, drops duplicate/unknown slots, restores missing slots, and clears stale prompt references.
- Older settings stores automatically receive three disabled preset slots.

## Automated coverage added

Unit/state-machine tests cover preset defaults and normalization, shortcut registration eligibility, immutable preset-vs-normal snapshots, operation handoff clearing, queued preset dispatch, deleted-model reconciliation, and deleted-prompt reconciliation.

## Build

Use Handy's existing build instructions in `BUILD.md`. The normal development commands are:

```bash
bun install
bun tauri dev
```

and for a production package:

```bash
bun run tauri build
```

## Validation performed in the editing environment

The repository translation checker was run with Node and reports all 25 non-English locales complete. The new strings in those locales currently use English fallback text so the existing CI key-completeness check passes without pretending they were professionally translated.

`git diff --check` passes, and the changed TypeScript files were parsed with the available global TypeScript compiler with no parse-level diagnostics. A complete Tauri/Rust build and `cargo test` could not be run because Bun, Rust, Cargo, and the project dependencies are not installed in this environment. Run the repository's normal Bun/Rust CI/build before treating this as a production binary.

## Behavioral notes

- Changing a preset model preserves its language when supported; otherwise the preset is normalized to a valid model language. The backend repeats this normalization before saving.
- The global **Post Processing** toggle is a master/privacy switch. Presets remember their configured prompt while it is off, but no preset sends text to an AI provider until the global toggle is enabled.
- History **Re-transcribe** intentionally uses the current normal model/settings, not the preset configuration that may have created the original entry. History does not currently persist a full transcription-operation snapshot.
- Bulk shortcut cleanup attempts to unregister all known preset bindings, including presets currently marked disabled, so stale OS registrations can be recovered after partial failures.
- Non-English preset strings use `null` untranslated sentinels and i18next's English fallback instead of duplicating English text as if it were localized.
