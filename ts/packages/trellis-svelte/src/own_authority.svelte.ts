import type {
  OwnAuthoritySnapshot,
  OwnAuthorityView,
} from "@oatscenter/trellis";

/** Reactive advisory view of the SDK's verified promoted authority; the runtime owns its lifecycle. */
export class OwnAuthorityController {
  #view = $state.raw<OwnAuthorityView>();
  #snapshot = $state.raw<OwnAuthoritySnapshot>();
  #unsubscribe?: () => void;

  /** Current authority, absent whenever the runtime cannot prove its usability. */
  get current(): OwnAuthoritySnapshot | undefined {
    return this.#snapshot === this.#view?.current ? this.#snapshot : undefined;
  }

  /** Observe a connected client without creating an authorization owner or refresh loop. */
  bind(view: OwnAuthorityView | undefined): void {
    this.#unsubscribe?.();
    this.#unsubscribe = undefined;
    this.#snapshot = undefined;
    this.#view = view;
    if (view)
      this.#unsubscribe = view.subscribe((snapshot) => {
        this.#snapshot = snapshot;
      });
  }

  /** Release the UI subscription, leaving transport and authorization ownership with the SDK. */
  dispose(): void {
    this.bind(undefined);
  }
}
