<script lang="ts">
  import { onMount } from "svelte";

  interface Capability {
    capabilityId: string;
    title: string;
    description: string;
    consequence: string;
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
    /** Another administrator approves administration before the app holds it. */
    peerApproval: boolean;
  }

  interface Provider {
    providerId: string;
    displayName: string;
  }

  const query = new URL(window.location.href).searchParams;
  const transaction = query.get("transaction") ?? "";
  const login = window.location.pathname === "/login";
  const device = window.location.pathname === "/device";
  const deviceComplete = window.location.pathname === "/device/complete";
  const csrf =
    document.cookie
      .split(";")
      .map((cookie) => cookie.trim())
      .find((cookie) => cookie.startsWith("__Host-trellis-portal-csrf="))
      ?.slice("__Host-trellis-portal-csrf=".length) ?? "";
  let view = $state.raw<Consent | null>(null);
  let selectedCapabilities = $state<string[]>([]);
  let confirming = $state(false);
  let error = $state("");
  let providers = $state.raw<Provider[]>([]);
  let providerError = $state("");
  // The server marks pages it serves with Trellis-generated certificates.
  const generatedTls =
    document.documentElement.hasAttribute("data-generated-tls");
  let blocked = $derived(
    view?.capabilities.some((item) => item.required && !item.available),
  );
  let application = $derived(
    view?.capabilities.filter((item) => !item.administrative) ?? [],
  );
  let administration = $derived(
    view?.capabilities.filter((item) => item.administrative) ?? [],
  );
  let selectedAdministration = $derived(
    administration.filter(
      (item) =>
        item.available && selectedCapabilities.includes(item.capabilityId),
    ),
  );
  let needsConfirmation = $derived(
    (view?.confirmAdministration ?? false) && selectedAdministration.length > 0,
  );

  onMount(() => {
    if (deviceComplete || (device && csrf)) return;
    if (!transaction || !csrf) {
      error =
        "This authorization request is unavailable. Return to the application and start sign-in again.";
      return;
    }
    const controller = new AbortController();
    if (login) {
      void fetch("/portal/providers", {
        signal: controller.signal,
        credentials: "same-origin",
        cache: "no-store",
      })
        .then(async (response) => {
          if (!response.ok) throw new Error("providers_unavailable");
          const result: Provider[] = await response.json();
          if (!controller.signal.aborted) providers = result;
        })
        .catch(() => {
          if (!controller.signal.aborted)
            providerError =
              "Company sign-in is temporarily unavailable. You can still use a local Trellis account.";
        });
      return () => controller.abort();
    }
    void fetch(`/portal/authorization/${encodeURIComponent(transaction)}`, {
      signal: controller.signal,
      credentials: "same-origin",
      cache: "no-store",
    })
      .then(async (response) => {
        if (!response.ok) throw new Error("authorization_unavailable");
        const result: Consent = await response.json();
        if (!controller.signal.aborted) {
          selectedCapabilities = result.capabilities
            .filter((item) => item.selected)
            .map((item) => item.capabilityId);
          view = result;
        }
      })
      .catch(() => {
        if (!controller.signal.aborted) {
          error =
            "This authorization request is unavailable or has expired. Return to the application and start sign-in again.";
        }
      });
    return () => controller.abort();
  });
</script>

<svelte:head
  ><title
    >{device || deviceComplete
      ? "Device verification"
      : login
        ? "Sign in"
        : "Review access"} · Trellis</title
  ></svelte:head
>

<header><strong>Trellis</strong><span>Authorization</span></header>

{#if generatedTls}
  <p class="tls-notice" role="status">
    This Trellis is using self-generated certificates and may not be suitable
    for production.
  </p>
{/if}

{#if error}
  <main>
    <h1>Authorization unavailable</h1>
    <p role="alert">{error}</p>
  </main>
{:else if deviceComplete}
  <main>
    <h1>
      {query.get("decision") === "approved"
        ? "Device access approved"
        : "Device access denied"}
    </h1>
    <p>
      Return to your device or application to continue. You can close this page.
    </p>
  </main>
{:else if device}
  <main class="login">
    <h1>Verify your device</h1>
    <p>
      Enter the code displayed by the device or application you are signing in
      to.
    </p>
    <p class="unavailable">
      Do not enter a code sent by someone else. You will review the actual
      application and its requested access before approving anything.
    </p>
    <form method="post" action="/portal/device">
      <input type="hidden" name="csrf" value={csrf} />
      <label class="field" for="user-code">Device code</label>
      <input
        id="user-code"
        name="user_code"
        type="text"
        value={query.get("user_code") ?? ""}
        maxlength="32"
        autocomplete="off"
        autocapitalize="characters"
        spellcheck="false"
        required
      />
      <button class="primary" type="submit" disabled={!csrf}
        >Review device request</button
      >
    </form>
  </main>
{:else if login}
  <main class="login">
    <h1>Sign in to Trellis</h1>
    <p class="muted">
      Your account determines which access you can approve. The application
      never receives your password.
    </p>
    {#if query.get("error") === "login_failed"}
      <p class="unavailable" role="alert">
        Sign-in failed. Check your username and password, or contact your
        Trellis administrator.
      </p>
    {/if}
    {#if query.get("error") === "upstream_login_failed"}
      <p class="unavailable" role="alert">
        Company sign-in was not completed. Try again, or use a local Trellis
        account.
      </p>
    {/if}
    {#if providers.length}
      <section aria-label="Company sign-in">
        <h2>Company account</h2>
        {#each providers as provider (provider.providerId)}
          <a
            href={`/portal/oidc/${encodeURIComponent(provider.providerId)}/begin?transaction=${encodeURIComponent(transaction)}`}
          >
            Continue with {provider.displayName}
          </a>
        {/each}
      </section>
      <h2>Local Trellis account</h2>
    {/if}
    {#if providerError}<p role="status">{providerError}</p>{/if}
    <form method="post" action="/portal/login">
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
      <button class="primary" type="submit" disabled={!csrf || !transaction}
        >Sign in</button
      >
    </form>
    <footer>
      You'll review the application's identity and requested access before
      approving anything.
    </footer>
  </main>
{:else if view}
  <main>
    {#if query.get("error") === "consent_changed"}
      <p class="unavailable" role="alert">
        The requested access changed while you were reviewing it. Review the
        current information below before making a new decision.
      </p>
    {/if}
    <p class="muted">Signed in as {view.username ?? view.principalId}</p>
    <h1>Allow {view.clientName ?? view.clientId} to access Trellis?</h1>
    <section class="identity" aria-label="Application identity">
      <strong
        >{view.registered ? "Registered client" : "Unregistered client"}</strong
      >
      <p>
        {view.registered
          ? "A Trellis administrator registered this client. Review the requested access before approving."
          : "The publisher claims this name and metadata. A Trellis administrator has not reviewed this application."}
      </p>
      <dl>
        <dt>Client ID</dt>
        <dd><code>{view.clientId}</code></dd>
        {#if view.redirectUri}<dt>Actual callback</dt>
          <dd><code>{view.redirectUri}</code></dd>{/if}
        {#if view.deviceUserCode}<dt>Device code</dt>
          <dd><code>{view.deviceUserCode}</code></dd>{/if}
      </dl>
    </section>
    <form method="post" action="/portal/approve">
      <input type="hidden" name="csrf" value={csrf} />
      <input type="hidden" name="transaction" value={transaction} />
      <input type="hidden" name="presentation" value={view.presentationId} />
      {#if view.deviceUserCode}
        <label class="remember"
          ><input
            type="checkbox"
            name="confirm_device"
            value={view.deviceUserCode}
            required
          /><span
            >This code matches the device or application I am signing in to.
            Nobody sent me this code.</span
          ></label
        >
      {/if}
      <div class="actions" aria-label="Access selection">
        <button
          type="button"
          onclick={() => {
            if (!view) return;
            // Administration is never selected in bulk.
            selectedCapabilities = view.capabilities
              .filter(
                (item) =>
                  item.available && (item.required || !item.administrative),
              )
              .map((item) => item.capabilityId);
          }}>Select all</button
        >
        <button
          type="button"
          onclick={() => {
            if (!view) return;
            selectedCapabilities = view.capabilities
              .filter((item) => item.required && item.available)
              .map((item) => item.capabilityId);
          }}>Unselect all</button
        >
      </div>
      {#snippet choice(capability: Capability)}
        <div class="choice">
          <input
            id={capability.capabilityId}
            type="checkbox"
            name="approved"
            value={capability.capabilityId}
            checked={selectedCapabilities.includes(capability.capabilityId)}
            onchange={(event) => {
              selectedCapabilities = event.currentTarget.checked
                ? [...selectedCapabilities, capability.capabilityId]
                : selectedCapabilities.filter(
                    (id) => id !== capability.capabilityId,
                  );
            }}
            disabled={capability.required || !capability.available}
          />
          <div>
            <div class="choice-title">
              <label for={capability.capabilityId}
                ><strong>{capability.title}</strong></label
              ><span class="requirement"
                >{capability.required ? "Required" : "Optional"}</span
              >
            </div>
            <code>{capability.capabilityId}</code>
            <p>{capability.description}</p>
            {#if capability.members?.length}<p>
                <strong>Includes:</strong>
                {capability.members.map((member) => member.title).join(", ")}
              </p>{/if}
            {#if capability.consequence}<p>
                <strong>Effect:</strong>
                {capability.consequence}
              </p>{/if}
            {#if !capability.available}<p class="unavailable">
                {capability.administrative && !view?.registered
                  ? "Only applications an administrator registered can manage Trellis."
                  : "Your account does not currently have this capability."}
              </p>{/if}
            {#if capability.required && capability.available}<input
                type="hidden"
                name="approved"
                value={capability.capabilityId}
              />{/if}
          </div>
        </div>
      {/snippet}
      <div hidden={confirming}>
        {#if application.length}
          <fieldset>
            <legend>Application capabilities</legend>
            <p class="muted">
              Required capabilities are necessary to continue. Choose optional
              capabilities independently.
            </p>
            {#each application as capability (capability.capabilityId)}
              {@render choice(capability)}
            {/each}
          </fieldset>
        {/if}
        {#if administration.length}
          <fieldset>
            <legend>Manage this Trellis</legend>
            <p class="muted">
              These capabilities administer this Trellis installation itself,
              not just the application's own data.
            </p>
            {#each administration as capability (capability.capabilityId)}
              {@render choice(capability)}
            {/each}
          </fieldset>
        {/if}
        <label class="remember"
          ><input type="checkbox" name="remember" value="true" checked /><span
            >Remember these choices for this account and application.<br
            /><small
              >Future capability additions require new approval, and permission
              to manage Trellis is never remembered. You can revoke access
              later.</small
            ></span
          ></label
        >
      </div>
      {#if confirming}
        <section class="administration" aria-labelledby="administration-title">
          <h2 id="administration-title">
            Allow {view.clientName ?? view.clientId} to manage this Trellis?
          </h2>
          <p>
            This application is asking to manage this Trellis installation
            itself. That is a rare and exceptional request: most applications
            never need it. Approve it only if you expected it and trust this
            application.
          </p>
          <ul>
            {#each selectedAdministration as capability (capability.capabilityId)}
              <li>
                <strong>{capability.title}</strong>
                {#if capability.consequence}— {capability.consequence}{/if}
              </li>
            {/each}
          </ul>
          {#if view.peerApproval}<p>
              Another administrator must also approve this before the
              application receives it.
            </p>{/if}
          <label class="remember"
            ><input
              type="checkbox"
              name="confirm_administration"
              value="true"
              required
            /><span
              >I understand this application will be able to manage this Trellis
              installation.</span
            ></label
          >
        </section>
      {/if}
      {#if blocked}<p class="unavailable" role="status">
          Required access is unavailable. You cannot authorize this application
          with this account.
        </p>{/if}
      <div class="actions">
        {#if confirming}<button
            type="button"
            onclick={() => (confirming = false)}>Back</button
          >{/if}
        <button name="decision" value="deny" type="submit" formnovalidate
          >Deny access</button
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
          }}
          >{confirming
            ? "Allow management of Trellis"
            : "Authorize application"}</button
        >
      </div>
    </form>
    <footer>
      Trellis grants only the access approved here, while your account still
      holds that access.
    </footer>
  </main>
{:else}
  <main aria-busy="true">
    <h1>Review application access</h1>
    <p class="muted" role="status">
      Loading the client's identity, callback, and your current capabilities…
    </p>
  </main>
{/if}
