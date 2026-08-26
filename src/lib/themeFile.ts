import { invoke } from "@tauri-apps/api/core";
import {
  PRESET_THEMES,
  THEME_STORAGE_KEY,
  resolveThemeFile,
  themeToFile,
  themesEqual,
  type Theme,
  type ThemeFile,
} from "./theme";

let writeWarned = false;
function warnWrite(err: unknown) {
  if (!writeWarned) {
    writeWarned = true;
    console.warn(
      "[theme] theme.json unavailable — continuing in localStorage-only mode:",
      err,
    );
  }
}

/** Last successfully persisted serialized file (no-op write guard) */
let lastPersisted = "";

function readStoredTheme(): Theme {
  try {
    const raw = localStorage.getItem(THEME_STORAGE_KEY);
    if (raw) {
      const t = JSON.parse(raw) as Theme | null;
      if (t && typeof t.accent === "string" && typeof t.bgBase === "string") {
        return t;
      }
    }
  } catch {
    // fall through to default
  }
  return PRESET_THEMES[0];
}

function writeStoredTheme(theme: Theme) {
  try {
    localStorage.setItem(THEME_STORAGE_KEY, JSON.stringify(theme));
  } catch {}
}

/** Pre-render bootstrap */
export async function bootstrapThemeFile(): Promise<void> {
  const current = readStoredTheme();
  let file: ThemeFile | null;
  try {
    file = await invoke<ThemeFile | null>("theme_file_get");
  } catch {
    return; // Invalid file or backend unavailable
  }
  if (file === null) {
    // Create if not exists
    const newFile = themeToFile(current);
    try {
      await invoke("theme_file_set", { file: newFile });
      lastPersisted = JSON.stringify(newFile);
    } catch (err) {
      warnWrite(err);
    }
    return;
  }
  const resolved = resolveThemeFile(file);
  if (resolved && !themesEqual(resolved, current)) {
    // If theme in file != from localStorage, update
    writeStoredTheme(resolved);
  }
}

export async function syncThemeToFile(theme: Theme): Promise<void> {
  const file = themeToFile(theme);
  const json = JSON.stringify(file);
  if (json === lastPersisted) return;
  try {
    await invoke("theme_file_set", { file });
    lastPersisted = json;
  } catch (err) {
    warnWrite(err);
  }
}

export async function handleThemeFocusChange(
  focused: boolean,
  getCurrent: () => Theme,
  setCurrent: (theme: Theme) => void,
): Promise<void> {
  if (!focused) return;

  let file: ThemeFile | null;
  try {
    file = await invoke<ThemeFile | null>("theme_file_get");
  } catch {
    return; // invalid file, keep in-app theme
  }
  if (file === null) {
    const current = getCurrent();
    try {
      await invoke("theme_file_set", { file: themeToFile(current) });
      lastPersisted = JSON.stringify(themeToFile(current));
    } catch (err) {
      warnWrite(err);
    }
    return;
  }
  const resolved = resolveThemeFile(file);
  if (resolved && !themesEqual(resolved, getCurrent())) {
    setCurrent(resolved);
  }
}
