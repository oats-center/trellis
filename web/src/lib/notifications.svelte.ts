import { createContext } from "svelte";
import { SvelteMap } from "svelte/reactivity";

export type ToastTone = "success" | "error" | "info";

export type ToastItem = {
  id: number;
  title: string;
  message: string;
  tone: ToastTone;
};

export class NotificationsController {
  items = $state<ToastItem[]>([]);
  #nextId = 1;
  #timers = new SvelteMap<number, ReturnType<typeof globalThis.setTimeout>>();

  push(
    title: string,
    message: string,
    tone: ToastTone = "info",
    duration = 4200,
  ): number {
    const id = this.#nextId++;
    this.items = [...this.items, { id, title, message, tone }];

    if (typeof globalThis.setTimeout !== "undefined" && duration > 0) {
      const timer = globalThis.setTimeout(() => {
        this.dismiss(id);
      }, duration);
      this.#timers.set(id, timer);
    }

    return id;
  }

  success(message: string, title = "Saved") {
    return this.push(title, message, "success");
  }

  error(message: string, title = "Action failed") {
    return this.push(title, message, "error", 5200);
  }

  info(message: string, title = "Update") {
    return this.push(title, message, "info");
  }

  dismiss(id: number) {
    const timer = this.#timers.get(id);
    if (timer !== undefined && typeof globalThis.clearTimeout !== "undefined") {
      globalThis.clearTimeout(timer);
      this.#timers.delete(id);
    }

    this.items = this.items.filter((item) => item.id !== id);
  }

  clear() {
    if (typeof globalThis.clearTimeout !== "undefined") {
      for (const timer of this.#timers.values()) {
        globalThis.clearTimeout(timer);
      }
    }

    this.#timers.clear();
    this.items = [];
  }
}

export const [getNotifications, setNotifications] = createContext<
  NotificationsController
>();
