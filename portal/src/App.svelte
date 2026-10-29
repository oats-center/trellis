<script lang="ts">
  import { onMount } from "svelte";
  import Mark from "./Mark.svelte";

  interface Capability {
    capabilityId: string;
    title: string;
    description: string;
    grants: string;
    /** A group stands for the capabilities it lists. */
    kind: "capability" | "group";
    members: { capabilityId: string; title: string }[] | null;
    /** Manages this Trellis installation. */
    administrative: boolean;
    required: boolean;
    available: boolean;
    selected: boolean;
  }

  interface Consent {
    transaction: string;
    presentationId: string;
    principalId: string;
    username: string | null;
    clientId: string;
    clientName: string | null;
    registered: boolean;
    redirectUri: string | null;
    deviceUserCode: string | null;
    capabilities: Capability[];
    /** Administration needs a separate confirmation step. */
    confirmAdministration: boolean;
  }

  interface Provider {
    providerId: string;
    displayName: string;
  }

  /** Installation or client branding; consent itself is never themed. */
  interface Theme {
    name: string | null;
    logo: string | null;
    colors: Record<string, string>;
    darkColors: Record<string, string>;
    copy: Record<string, string>;
  }

  /** The sign-in methods the server accepts. */
  interface SignIn {
    localIdentity: boolean;
    providers: Provider[];
    /** Whether this sign-in's app lets people create their own account. */
    signUp: { password: boolean; providers: boolean };
  }

  /** One consent row. */
  interface Choice {
    id: string;
    title: string;
    description: string;
    grants: string;
    members: string[];
    administrative: boolean;
    required: boolean;
    available: boolean;
  }

  interface Group {
    key: string;
    label: string;
    description: string;
    admin: boolean;
    choices: Choice[];
  }

  const query = new URL(window.location.href).searchParams;
  // A development installation serves sign-in under a path on the Console's
  // origin. The server names that path here, and every path below is under it.
  const base = document.documentElement.dataset.base ?? "";
  const path = window.location.pathname.slice(base.length);

  /** Uppercases a typed device code and groups it as `XXXX-XXXX`. */
  function formatDeviceCode(value: string): string {
    const code = value
      .toUpperCase()
      .replace(/[^A-Z0-9]/g, "")
      .slice(0, 8);
    return code.length > 4 ? `${code.slice(0, 4)}-${code.slice(4)}` : code;
  }
  let deviceCode = $state(formatDeviceCode(query.get("user_code") ?? ""));
  const transaction = query.get("transaction") ?? "";
  const login = path === "/login";
  const device = path === "/device";
  const deviceComplete = path === "/device/complete";
  const enrollment = path === "/device-enrollment";
  const enrollmentComplete = path === "/device-enrollment/complete";
  const reviewId = query.get("reviewId") ?? "";
  // The server validated this return address before serving the page.
  const signingOut = path === "/logout";
  const returnTo = query.get("post_logout_redirect_uri") ?? "";
  let signOutFailed = $state(false);

  interface EnrollmentReview {
    reviewId: string;
    state: string;
    revision: string;
    expiresAt: string;
    participantId: string;
    deviceType: string;
    deviceName: string;
    instanceId: string;
    canDecide: boolean;
  }

  // The code survives the sign-in round trip so people type it once.
  const codeKey = `trellis.device-review.${reviewId}`;
  let enrollmentCode = $state(sessionStorage.getItem(codeKey) ?? "");
  let enrollmentReview = $state.raw<EnrollmentReview | null>(null);
  let enrollmentStep = $state<"code" | "login" | "review">("code");
  let enrollmentError = $state("");
  let enrollmentBusy = $state(false);
  const decisionKey = crypto.randomUUID().replaceAll("-", "").slice(0, 26);

  function readableName(name: string): string {
    if (!/^[A-Za-z][a-zA-Z0-9]*(\.[A-Za-z][a-zA-Z0-9]*)*$/.test(name))
      return name;
    const words = (name.split(".").at(-1) ?? name)
      .replace(/([a-z0-9])([A-Z])/g, "$1 $2")
      .toLowerCase();
    return words.charAt(0).toUpperCase() + words.slice(1);
  }

  async function inspectEnrollment(): Promise<void> {
    if (enrollmentBusy) return;
    enrollmentBusy = true;
    enrollmentError = "";
    sessionStorage.setItem(codeKey, enrollmentCode);
    try {
      const response = await fetch(`${base}/portal/device-review/inspect`, {
        method: "POST",
        credentials: "same-origin",
        cache: "no-store",
        body: new URLSearchParams({
          csrf,
          review_id: reviewId,
          code: enrollmentCode,
        }),
      });
      if (response.status === 401) {
        enrollmentStep = "login";
        return;
      }
      if (response.status === 403) {
        enrollmentError =
          "That code doesn't match this request. Check the device and try again.";
        return;
      }
      if (!response.ok) throw new Error("unavailable");
      enrollmentReview = await response.json();
      enrollmentStep = "review";
    } catch {
      enrollmentError = "This request isn't available right now. Try again.";
    } finally {
      enrollmentBusy = false;
    }
  }
  const csrf =
    document.cookie
      .split(";")
      .map((cookie) => cookie.trim())
      .find((cookie) => cookie.startsWith("__Host-trellis-portal-csrf="))
      ?.slice("__Host-trellis-portal-csrf=".length) ?? "";
  let view = $state.raw<Consent | null>(null);
  let selected = $state<string[]>([]);
  let expanded = $state<string[]>([]);
  let confirming = $state(false);
  let error = $state(false);
  let providers = $state.raw<Provider[]>([]);
  let localIdentity = $state(true);
  let signUp = $state.raw<SignIn["signUp"]>({
    password: false,
    providers: false,
  });
  let creating = $state(query.get("sign_up") === "1");
  let providerError = $state(false);
  // The server marks pages it serves with Trellis-generated certificates.
  const generatedTls =
    document.documentElement.hasAttribute("data-generated-tls");
  let theme = $state.raw<Theme>({
    name: null,
    logo: null,
    colors: {},
    darkColors: {},
    copy: {},
  });
  const brand = $derived(theme.name ?? "Trellis");

  // The server picks the theme from its own record of this transaction.
  onMount(() => {
    const controller = new AbortController();
    const dark = window.matchMedia("(prefers-color-scheme: dark)");
    const paint = () => {
      const root = document.documentElement.style;
      const palette = dark.matches
        ? { ...theme.colors, ...theme.darkColors }
        : theme.colors;
      for (const name of new Set([
        ...Object.keys(theme.colors),
        ...Object.keys(theme.darkColors),
      ])) {
        const value = palette[name];
        if (value) root.setProperty(`--${name}`, value);
        else root.removeProperty(`--${name}`);
      }
    };
    void fetch(
      `${base}/portal/theme${transaction ? `?transaction=${encodeURIComponent(transaction)}` : ""}`,
      { credentials: "same-origin", signal: controller.signal },
    )
      .then((response) => (response.ok ? response.json() : null))
      .then((loaded: Theme | null) => {
        if (!loaded) return;
        theme = loaded;
        paint();
      })
      .catch(() => {});
    dark.addEventListener("change", paint);
    return () => {
      controller.abort();
      dark.removeEventListener("change", paint);
    };
  });

  // "trellis.auth.v1:users.edit" → "users": capabilities are named area.action,
  // so people choose by area. A capability group is its own row.
  function areaOf(capabilityId: string): string {
    const local = capabilityId.slice(capabilityId.indexOf(":") + 1);
    return local.split(".")[0] ?? local;
  }

  function sentence(name: string): string {
    const words = name.replace(/([a-z0-9])([A-Z])/g, "$1 $2").toLowerCase();
    return words.charAt(0).toUpperCase() + words.slice(1);
  }

  function toChoice(capability: Capability): Choice {
    return {
      id: capability.capabilityId,
      title: capability.title,
      description: capability.description,
      grants: capability.grants,
      members: capability.members?.map((member) => member.title) ?? [],
      administrative: capability.administrative,
      required: capability.required,
      available: capability.available,
    };
  }

  let groups = $derived.by((): Group[] => {
    if (!view) return [];
    const byKey = new Map<string, Group>();
    for (const capability of view.capabilities) {
      if (capability.administrative) continue;
      const api = capability.capabilityId.split(":")[0] ?? "";
      const key =
        capability.kind === "group"
          ? capability.capabilityId
          : `${api}:${areaOf(capability.capabilityId)}`;
      let group = byKey.get(key);
      if (!group) {
        group = {
          key,
          label:
            capability.kind === "group"
              ? capability.title
              : sentence(areaOf(capability.capabilityId)),
          description:
            capability.kind === "group" ? capability.description : "",
          admin: false,
          choices: [],
        };
        byKey.set(key, group);
      }
      group.choices.push(toChoice(capability));
    }
    const result = [...byKey.values()].sort((left, right) =>
      left.label.localeCompare(right.label),
    );
    const administration = view.capabilities.filter(
      (capability) => capability.administrative,
    );
    if (administration.length) {
      result.push({
        key: "administration",
        label: "Manage this Trellis",
        description: "Change how this Trellis installation itself is run.",
        admin: true,
        choices: administration.map(toChoice),
      });
    }
    return result;
  });
  let selectedAdministration = $derived(
    groups
      .flatMap((group) => group.choices)
      .filter(
        (choice) =>
          choice.administrative &&
          choice.available &&
          selected.includes(choice.id),
      ),
  );
  let needsConfirmation = $derived(
    (view?.confirmAdministration ?? false) && selectedAdministration.length > 0,
  );
  // Administration is never selected in bulk.
  let optional = $derived(
    groups
      .flatMap((group) => group.choices)
      .filter(
        (choice) =>
          !choice.required && choice.available && !choice.administrative,
      ),
  );
  let blocked = $derived(
    groups.some((group) =>
      group.choices.some((choice) => choice.required && !choice.available),
    ),
  );
  let allOptionalSelected = $derived(
    optional.every((choice) => selected.includes(choice.id)),
  );
  let appName = $derived(view ? (view.clientName ?? view.clientId) : "");

  function setGroup(group: Group, on: boolean): void {
    const ids = group.choices
      .filter((choice) => !choice.required && choice.available)
      .map((choice) => choice.id);
    selected = on
      ? [...new Set([...selected, ...ids])]
      : selected.filter((id) => !ids.includes(id));
  }

  function groupState(group: Group): "all" | "some" | "none" {
    const ids = group.choices
      .filter((choice) => !choice.required && choice.available)
      .map((choice) => choice.id);
    const count = ids.filter((id) => selected.includes(id)).length;
    if (count === ids.length) return "all";
    return count ? "some" : "none";
  }

  function indeterminate(node: HTMLInputElement, value: boolean) {
    node.indeterminate = value;
    return {
      update(next: boolean) {
        node.indeterminate = next;
      },
    };
  }

  /** Ends the Portal login on this browser, then returns to the app. */
  async function endPortalLogin(): Promise<void> {
    signOutFailed = false;
    try {
      if (csrf) {
        const response = await fetch(`${base}/portal/logout`, {
          method: "POST",
          credentials: "same-origin",
          body: new URLSearchParams({ csrf, request_id: decisionKey }),
        });
        // access_denied: no Portal login is left to end.
        if (
          !response.ok &&
          (await response.json().catch(() => null))?.error !== "access_denied"
        )
          throw new Error("logout_failed");
      }
      window.location.replace(returnTo);
    } catch {
      signOutFailed = true;
    }
  }

  onMount(() => {
    if (signingOut) {
      void endPortalLogin();
      return;
    }
    if (enrollmentComplete) {
      sessionStorage.removeItem(codeKey);
      return;
    }
    if (enrollment) {
      if (!reviewId || !csrf) {
        error = true;
        return;
      }
      if (
        query.get("error") === "login_failed" ||
        query.get("error") === "upstream_login_failed"
      )
        enrollmentStep = "login";
      else if (enrollmentCode) void inspectEnrollment();
      void fetch(`${base}/portal/providers`, {
        credentials: "same-origin",
        cache: "no-store",
      })
        .then(async (response) => {
          if (!response.ok) return;
          const result: SignIn = await response.json();
          providers = result.providers;
          localIdentity = result.localIdentity;
        })
        .catch(() => {});
      return;
    }
    if (deviceComplete || (device && csrf)) return;
    if (!transaction || !csrf) {
      error = true;
      return;
    }
    const controller = new AbortController();
    if (login) {
      void fetch(
        `${base}/portal/providers?transaction=${encodeURIComponent(transaction)}`,
        {
          signal: controller.signal,
          credentials: "same-origin",
          cache: "no-store",
        },
      )
        .then(async (response) => {
          if (!response.ok) throw new Error("providers_unavailable");
          const result: SignIn = await response.json();
          if (controller.signal.aborted) return;
          providers = result.providers;
          localIdentity = result.localIdentity;
          signUp = result.signUp;
        })
        .catch(() => {
          if (!controller.signal.aborted) providerError = true;
        });
      return () => controller.abort();
    }
    void fetch(
      `${base}/portal/authorization/${encodeURIComponent(transaction)}`,
      {
        signal: controller.signal,
        credentials: "same-origin",
        cache: "no-store",
      },
    )
      .then(async (response) => {
        if (!response.ok) throw new Error("authorization_unavailable");
        const result: Consent = await response.json();
        if (!controller.signal.aborted) {
          selected = result.capabilities
            .filter((item) => item.selected)
            .map((item) => item.capabilityId);
          view = result;
          // A short request is easier to review fully open.
          if (result.capabilities.length <= 6)
            expanded = groups.map((group) => group.key);
        }
      })
      .catch(() => {
        if (!controller.signal.aborted) error = true;
      });
    return () => controller.abort();
  });
</script>

<svelte:head
  ><title
    >{device || deviceComplete || enrollment || enrollmentComplete
      ? "Connect a device"
      : signingOut
        ? "Sign out"
        : login
          ? "Sign in"
          : "Allow access"} · {brand}</title
  ></svelte:head
>

{#if generatedTls}
  <p class="tls-notice" role="status">
    This Trellis is using self-generated certificates and may not be suitable
    for production.
  </p>
{/if}

<header class="brand">
  {#if theme.logo}<img class="brand-logo" src={theme.logo} alt="" />{:else}<Mark
      size={22}
    />{/if}<span>{brand}</span>
</header>

{#if signingOut}
  <main class="card narrow center">
    {#if signOutFailed}
      <h1>We couldn't sign you out</h1>
      <p class="muted" role="alert">Check your connection and try again.</p>
      <button class="button block" type="button" onclick={endPortalLogin}
        >Try again</button
      >
    {:else}
      <h1>Signing you out…</h1>
    {/if}
  </main>
{:else if error}
  <main class="card narrow center">
    <h1>This sign-in link has expired</h1>
    <p class="muted" role="alert">Go back to the app and sign in again.</p>
  </main>
{:else if enrollmentComplete}
  {@const approved = query.get("decision") === "approved"}
  <main class="card narrow center">
    <span class={["status-icon", approved ? "ok" : "denied"]} aria-hidden="true"
    ></span>
    <h1>{approved ? "Device approved" : "Request rejected"}</h1>
    <p class="muted">
      {approved
        ? "The device can connect now. You can close this window."
        : "The device won't be able to connect. You can close this window."}
    </p>
  </main>
{:else if enrollment}
  <main class="card narrow">
    <h1>Approve a device</h1>
    {#if query.get("error") === "not_allowed"}
      <p class="alert error" role="alert">
        Your account can't approve devices. Ask an administrator.
      </p>
    {:else if query.get("error") === "changed"}
      <p class="alert" role="alert">
        This request changed or was already decided. Check it again.
      </p>
    {:else if query.get("error") === "unavailable" || query.get("error") === "invalid"}
      <p class="alert error" role="alert">Something went wrong. Try again.</p>
    {/if}
    {#if enrollmentStep === "review" && enrollmentReview}
      {@const pending = enrollmentReview.state === "pending"}
      <p class="muted">Check that this is the device in front of you.</p>
      <dl class="device-details">
        <dt>Device</dt>
        <dd>{enrollmentReview.deviceName}</dd>
        <dt>Type</dt>
        <dd>{readableName(enrollmentReview.deviceType)}</dd>
        <dt>Software</dt>
        <dd>{readableName(enrollmentReview.participantId)}</dd>
        <dt>Code</dt>
        <dd><code>{enrollmentCode.toUpperCase()}</code></dd>
      </dl>
      {#if !pending}
        <p class="alert" role="status">
          {enrollmentReview.state === "approved"
            ? "This device is already approved."
            : enrollmentReview.state === "rejected"
              ? "This request was rejected."
              : "This request has expired. Start again on the device."}
        </p>
      {:else if !enrollmentReview.canDecide}
        <p class="alert" role="status">
          Your account can't approve devices. Ask an administrator to open this
          link.
        </p>
      {:else}
        <form method="post" action={`${base}/portal/device-review/decide`}>
          <input type="hidden" name="csrf" value={csrf} />
          <input type="hidden" name="review_id" value={reviewId} />
          <input type="hidden" name="code" value={enrollmentCode} />
          <input
            type="hidden"
            name="revision"
            value={enrollmentReview.revision}
          />
          <input type="hidden" name="idempotency_key" value={decisionKey} />
          <div class="actions">
            <button name="decision" value="declined" type="submit"
              >Reject</button
            >
            <button
              name="decision"
              value="approved"
              type="submit"
              class="primary">Approve</button
            >
          </div>
        </form>
      {/if}
    {:else if enrollmentStep === "login"}
      <p class="muted">Sign in to approve this device.</p>
      {#if query.get("error") === "login_failed"}
        <p class="alert error" role="alert">
          That username and password didn't match. Try again.
        </p>
      {/if}
      {#if query.get("error") === "upstream_login_failed"}
        <p class="alert error" role="alert">
          Company sign-in wasn't completed. Try again.
        </p>
      {/if}
      {#if providers.length}
        <div class="providers">
          {#each providers as provider (provider.providerId)}
            <a
              class="button block"
              href={`${base}/portal/oidc/${encodeURIComponent(provider.providerId)}/begin?review=${encodeURIComponent(reviewId)}`}
            >
              Continue with {provider.displayName}
            </a>
          {/each}
        </div>
        {#if localIdentity}<p class="divider"><span>or</span></p>{/if}
      {/if}
      {#if localIdentity}<form
          method="post"
          action={`${base}/portal/device-review/login`}
        >
          <input type="hidden" name="csrf" value={csrf} />
          <input type="hidden" name="review_id" value={reviewId} />
          <label class="field" for="enrollment-username">Username</label>
          <input
            id="enrollment-username"
            name="username"
            type="text"
            autocomplete="username"
            autocapitalize="none"
            spellcheck="false"
            required
          />
          <label class="field" for="enrollment-password">Password</label>
          <input
            id="enrollment-password"
            name="password"
            type="password"
            autocomplete="current-password"
            required
          />
          <button class="primary block" type="submit">Sign in</button>
        </form>{/if}
    {:else}
      <p class="muted">Enter the code shown on the device.</p>
      <form
        onsubmit={(event) => {
          event.preventDefault();
          void inspectEnrollment();
        }}
      >
        <label class="field" for="enrollment-code">Code</label>
        <input
          id="enrollment-code"
          class="code-input"
          type="text"
          bind:value={enrollmentCode}
          maxlength="8"
          autocomplete="off"
          autocapitalize="characters"
          spellcheck="false"
          required
        />
        {#if enrollmentError}<p class="alert error" role="alert">
            {enrollmentError}
          </p>{/if}
        <button
          class="primary block"
          type="submit"
          disabled={enrollmentBusy || enrollmentCode.trim().length !== 8}
          >Continue</button
        >
      </form>
    {/if}
  </main>
{:else if deviceComplete}
  {@const approved = query.get("decision") === "approved"}
  <main class="card narrow center">
    <span class={["status-icon", approved ? "ok" : "denied"]} aria-hidden="true"
    ></span>
    <h1>{approved ? "Your device is connected" : "Request denied"}</h1>
    <p class="muted">You can close this window and return to your device.</p>
  </main>
{:else if device}
  <main class="card narrow">
    <h1>Connect a device</h1>
    <p class="muted">Enter the code shown on your device.</p>
    <form method="post" action={`${base}/portal/device`}>
      <input type="hidden" name="csrf" value={csrf} />
      <label class="field" for="user-code">Code</label>
      <input
        id="user-code"
        class="code-input"
        name="user_code"
        type="text"
        bind:value={
          () => deviceCode, (value) => (deviceCode = formatDeviceCode(value))
        }
        placeholder="XXXX-XXXX"
        maxlength="9"
        autocomplete="off"
        autocapitalize="characters"
        spellcheck="false"
        required
      />
      <button class="primary block" type="submit" disabled={!csrf}
        >Continue</button
      >
    </form>
    <p class="hint">Only enter a code from a device you are setting up.</p>
  </main>
{:else if login && creating && signUp.password}
  <main class="card narrow">
    <h1>Create your account</h1>
    <p class="muted">You'll use it to sign in to {brand}.</p>
    {#if query.get("error") === "sign_up_taken"}
      <p class="alert error" role="alert">
        That username is taken. Choose another.
      </p>
    {:else if query.get("error") === "sign_up_invalid"}
      <p class="alert error" role="alert">
        Check your name and username, and use a longer password.
      </p>
    {:else if query.get("error") === "sign_up_failed"}
      <p class="alert error" role="alert">
        Your account wasn't created. Try again.
      </p>
    {/if}
    <form method="post" action={`${base}/portal/sign-up`}>
      <input type="hidden" name="csrf" value={csrf} />
      <input type="hidden" name="request_id" value={transaction} />
      <label class="field" for="name">Name</label>
      <input id="name" name="name" type="text" autocomplete="name" required />
      <label class="field" for="email">Email (optional)</label>
      <input id="email" name="email" type="email" autocomplete="email" />
      <label class="field" for="username">Username</label>
      <input
        id="username"
        name="username"
        type="text"
        autocomplete="username"
        autocapitalize="none"
        spellcheck="false"
        required
      />
      <label class="field" for="password">Password</label>
      <input
        id="password"
        name="password"
        type="password"
        autocomplete="new-password"
        required
      />
      <button class="primary block" type="submit" disabled={!csrf}
        >Create account</button
      >
    </form>
    <p class="hint">
      Already have an account?
      <button class="link" type="button" onclick={() => (creating = false)}
        >Sign in</button
      >
    </p>
  </main>
{:else if login}
  <main class="card narrow">
    <h1>{theme.copy.sign_in_title ?? "Sign in"}</h1>
    <p class="muted">
      {theme.copy.sign_in_subtitle ?? `Use your ${brand} account to continue.`}
    </p>
    {#if query.get("error") === "login_failed"}
      <p class="alert error" role="alert">
        That username and password didn't match. Try again.
      </p>
    {/if}
    {#if query.get("error") === "upstream_login_failed"}
      <p class="alert error" role="alert">
        Company sign-in wasn't completed. Try again.
      </p>
    {/if}
    {#if providers.length}
      <div class="providers">
        {#each providers as provider (provider.providerId)}
          <a
            class="button block"
            href={`${base}/portal/oidc/${encodeURIComponent(provider.providerId)}/begin?transaction=${encodeURIComponent(transaction)}`}
          >
            Continue with {provider.displayName}
          </a>
        {/each}
      </div>
      {#if localIdentity}<p class="divider"><span>or</span></p>{/if}
    {/if}
    {#if providerError}<p class="alert" role="status">
        Company sign-in is unavailable right now.
      </p>{/if}
    {#if localIdentity}<form method="post" action={`${base}/portal/login`}>
        <input type="hidden" name="csrf" value={csrf} />
        <input type="hidden" name="request_id" value={transaction} />
        <label class="field" for="username">Username</label>
        <input
          id="username"
          name="username"
          type="text"
          autocomplete="username"
          autocapitalize="none"
          spellcheck="false"
          required
        />
        <label class="field" for="password">Password</label>
        <input
          id="password"
          name="password"
          type="password"
          autocomplete="current-password"
          required
        />
        <button
          class="primary block"
          type="submit"
          disabled={!csrf || !transaction}>Sign in</button
        >
      </form>{/if}
    {#if signUp.password}
      <p class="hint">
        New here?
        <button class="link" type="button" onclick={() => (creating = true)}
          >Create an account</button
        >
      </p>
    {:else if signUp.providers}
      <p class="hint">
        New here? Continue with your company account to create one.
      </p>
    {/if}
  </main>
{:else if view}
  <main class="card consent">
    <div class="app">
      <span class="app-icon" aria-hidden="true"
        >{appName.charAt(0).toUpperCase()}</span
      >
      <div>
        <h1>{appName} wants to use your account</h1>
        <p class="muted">
          Signed in as <strong>{view.username ?? view.principalId}</strong>
        </p>
      </div>
    </div>

    {#if query.get("error") === "consent_changed"}
      <p class="alert" role="alert">
        This request changed while you were reviewing it. Check it again.
      </p>
    {/if}
    {#if !view.registered}
      <p class="alert warn">
        <strong>Unverified app.</strong> Your administrator hasn't approved this app.
        Only continue if you trust it.
      </p>
    {/if}
    {#if blocked}
      <p class="alert error" role="status">
        Your account is missing access this app needs. Ask an administrator.
      </p>
    {/if}

    <form method="post" action={`${base}/portal/approve`}>
      <input type="hidden" name="csrf" value={csrf} />
      <input type="hidden" name="transaction" value={transaction} />
      <input type="hidden" name="presentation" value={view.presentationId} />
      <!-- Submitted from state so collapsed groups still send their choices. -->
      {#each view.capabilities as capability (capability.capabilityId)}
        {#if capability.available && selected.includes(capability.capabilityId)}
          <input
            type="hidden"
            name="approved"
            value={capability.capabilityId}
          />
        {/if}
      {/each}

      {#if view.deviceUserCode}
        <div class="device-code">
          <span class="muted">Code</span>
          <code>{view.deviceUserCode}</code>
        </div>
        <p class="alert warn">
          Make sure this code matches the one on your device. If someone sent
          you this code or link, stop here: approving gives them access as you.
        </p>
      {/if}

      <div hidden={confirming}>
        <div class="section-head">
          <h2>Allow it to</h2>
          {#if optional.length}
            <button
              type="button"
              class="link"
              onclick={() => {
                const select = !allOptionalSelected;
                for (const group of groups)
                  if (!group.admin) setGroup(group, select);
              }}>{allOptionalSelected ? "Clear all" : "Select all"}</button
            >
          {/if}
        </div>

        <ul class="groups">
          {#each groups as group (group.key)}
            {@const state = groupState(group)}
            {@const open = expanded.includes(group.key)}
            {@const choosable = group.choices.some(
              (choice) => !choice.required && choice.available,
            )}
            {@const granted = group.choices.filter(
              (choice) => choice.available && selected.includes(choice.id),
            ).length}
            <li
              class={["group", group.admin && "admin"]}
              role="group"
              aria-label={group.label}
            >
              <div class="group-head">
                <input
                  type="checkbox"
                  aria-label={`Allow all: ${group.label}`}
                  checked={granted > 0 && state === "all"}
                  use:indeterminate={state === "some" ||
                    (!choosable &&
                      granted > 0 &&
                      granted < group.choices.length)}
                  disabled={!choosable}
                  onchange={(event) =>
                    setGroup(group, event.currentTarget.checked)}
                />
                <button
                  type="button"
                  class="group-toggle"
                  aria-expanded={open}
                  onclick={() =>
                    (expanded = open
                      ? expanded.filter((key) => key !== group.key)
                      : [...expanded, group.key])}
                >
                  <span class="group-name"
                    >{group.label}{#if group.description}<span
                        class="group-desc">{group.description}</span
                      >{/if}</span
                  >
                  <span class="count">{granted} of {group.choices.length}</span>
                  <span class="chevron" aria-hidden="true"></span>
                </button>
              </div>
              {#if open}
                <ul class="choices">
                  {#each group.choices as choice (choice.id)}
                    <li class={["choice", !choice.available && "unavailable"]}>
                      <input
                        id={`choice-${choice.id}`}
                        type="checkbox"
                        checked={choice.available &&
                          selected.includes(choice.id)}
                        disabled={choice.required || !choice.available}
                        onchange={(event) => {
                          selected = event.currentTarget.checked
                            ? [...selected, choice.id]
                            : selected.filter((id) => id !== choice.id);
                        }}
                      />
                      <label for={`choice-${choice.id}`}>
                        <span class="choice-title"
                          >{choice.title}
                          {#if !choice.available}<span class="tag"
                              >Not available to you</span
                            >{:else if choice.required}<span class="tag"
                              >Required</span
                            >{/if}</span
                        >
                        <span class="choice-desc">{choice.description}</span>
                        {#if choice.members.length}<span class="choice-desc"
                            >Includes {choice.members.join(", ")}</span
                          >{/if}
                        {#if !choice.available && choice.administrative && !view.registered}<span
                            class="choice-desc"
                            >Only apps your administrator registered can manage
                            Trellis.</span
                          >{/if}
                      </label>
                    </li>
                  {/each}
                </ul>
              {/if}
            </li>
          {/each}
        </ul>

        <label class="check remember"
          ><input type="checkbox" name="remember" value="true" checked /><span
            >Remember my choices for this app</span
          ></label
        >
      </div>

      {#if confirming}
        <section
          class="alert administration"
          aria-labelledby="administration-title"
        >
          <h2 id="administration-title">
            Allow {appName} to manage this Trellis?
          </h2>
          <p>Only allow this if you expected it. {appName} will be able to:</p>
          <ul>
            {#each selectedAdministration as choice (choice.id)}
              <li>
                <strong>{choice.title}</strong>{#if choice.grants}: {choice.grants}{/if}
              </li>
            {/each}
          </ul>
          <input type="hidden" name="confirm_administration" value="true" />
        </section>
      {/if}

      <div class="actions">
        {#if confirming}<button
            type="button"
            onclick={() => (confirming = false)}>Back</button
          >{/if}
        <button name="decision" value="deny" type="submit" formnovalidate
          >Cancel</button
        >
        <button
          name="decision"
          value="approve"
          type="submit"
          class="primary"
          disabled={blocked}
          onclick={(event) => {
            // Administration takes a separate, explicit confirmation step.
            if (needsConfirmation && !confirming) {
              event.preventDefault();
              confirming = true;
            }
          }}>{confirming ? "Allow management" : "Allow"}</button
        >
      </div>
    </form>

    <details class="app-details">
      <summary>App details</summary>
      <dl>
        <dt>App ID</dt>
        <dd><code>{view.clientId}</code></dd>
        {#if view.redirectUri}<dt>Returns to</dt>
          <dd><code>{view.redirectUri}</code></dd>{/if}
        <dt>Status</dt>
        <dd>
          {view.registered ? "Registered by your administrator" : "Unverified"}
        </dd>
      </dl>
    </details>
  </main>
{:else}
  <main class="card narrow center" aria-busy="true">
    <span class="spinner" aria-hidden="true"></span>
    <p class="muted" role="status">Loading…</p>
  </main>
{/if}

{#if theme.copy.footer}<footer class="portal-footer">
    {theme.copy.footer}
  </footer>{/if}
