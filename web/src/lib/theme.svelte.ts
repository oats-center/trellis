import { browser } from "$app/environment";

export type ConsoleTheme = "trellis" | "trellis-dark";

const STORAGE_KEY = "trellis.console.theme";

function isConsoleTheme(value: string | null): value is ConsoleTheme {
  return value === "trellis" || value === "trellis-dark";
}

class ConsoleThemeState {
  #state = $state<{ theme: ConsoleTheme }>({ theme: "trellis" });

  get theme(): ConsoleTheme {
    return this.#state.theme;
  }

  get darkMode(): boolean {
    return this.#state.theme === "trellis-dark";
  }

  /** Applies the saved console theme to the document root. */
  init(): void {
    if (!browser) return;

    const savedTheme = localStorage.getItem(STORAGE_KEY);
    this.setTheme(isConsoleTheme(savedTheme) ? savedTheme : "trellis", {
      persist: false,
    });
  }

  /** Switches between the light and dark Trellis console themes. */
  toggle(): void {
    this.setTheme(this.darkMode ? "trellis" : "trellis-dark");
  }

  /** Sets the active Trellis console theme. */
  setTheme(theme: ConsoleTheme, options: { persist?: boolean } = {}): void {
    this.#state.theme = theme;
    if (!browser) return;

    document.documentElement.setAttribute("data-theme", theme);
    if (options.persist !== false) {
      localStorage.setItem(STORAGE_KEY, theme);
    }
  }
}

/** Shared theme state for the console app. */
export const consoleTheme = new ConsoleThemeState();
