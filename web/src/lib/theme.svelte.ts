import { browser } from "$app/environment";

/** How the Console and the Portal choose between light and dark. */
export type ThemeMode = "system" | "light" | "dark";

// The Portal reads the same key (portal/public/theme-mode.js), so a choice
// made here carries over when both are served from one origin.
const STORAGE_KEY = "trellis.theme";
const NEXT: Record<ThemeMode, ThemeMode> = {
  system: "light",
  light: "dark",
  dark: "system",
};
const LABELS: Record<ThemeMode, string> = {
  system: "System",
  light: "Light",
  dark: "Dark",
};

class ConsoleThemeState {
  #state = $state<{ mode: ThemeMode; systemDark: boolean }>({
    mode: "system",
    systemDark: false,
  });

  /** The saved choice. */
  get mode(): ThemeMode {
    return this.#state.mode;
  }

  /** The mode the toggle switches to next. */
  get next(): ThemeMode {
    return NEXT[this.#state.mode];
  }

  /** A short name for a mode, as shown on the toggle. */
  label(mode: ThemeMode): string {
    return LABELS[mode];
  }

  /** Whether the Console is drawn dark right now. */
  get dark(): boolean {
    const { mode, systemDark } = this.#state;
    return mode === "dark" || (mode === "system" && systemDark);
  }

  /** Reads the saved choice and follows the device while it is "system". */
  init(): void {
    if (!browser) return;
    let saved: string | null = null;
    try {
      saved = localStorage.getItem(STORAGE_KEY);
    } catch {
      // Storage can be unavailable; the device setting applies.
    }
    this.#state.mode = saved === "light" || saved === "dark" ? saved : "system";
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    this.#state.systemDark = media.matches;
    media.addEventListener("change", (event) => {
      this.#state.systemDark = event.matches;
      this.#apply();
    });
    this.#apply();
  }

  /** Moves to the next mode: System, then Light, then Dark. */
  cycle(): void {
    this.#state.mode = this.next;
    try {
      localStorage.setItem(STORAGE_KEY, this.#state.mode);
    } catch {
      // The choice still applies to this page.
    }
    this.#apply();
  }

  #apply(): void {
    document.documentElement.setAttribute(
      "data-theme",
      this.dark ? "trellis-dark" : "trellis",
    );
  }
}

/** Shared theme state for the console app. */
export const consoleTheme = new ConsoleThemeState();
