/** One way to sign in on the Account page: a linked identity, or a provider to connect. */
export type SignInRow<Identity> = {
  key: string;
  /** The offered provider, or null for a provider that is no longer offered. */
  providerId: string | null;
  title: string;
  identity?: Identity;
};

/**
 * Rows for the Account page's sign-in list, grouped by provider in the order
 * providers are offered. Every linked identity gets its own row, including
 * several linked through the same provider, so each can be disconnected on
 * its own. An offered provider without an identity gets one row to connect it.
 * Identities from providers no longer offered follow, titled by their issuer.
 */
export function signInRows<
  Identity extends { identityId: string; providerId: string; issuer: string },
>(
  providers: readonly { providerId: string; title: string }[],
  identities: readonly Identity[],
): SignInRow<Identity>[] {
  const offered = new Set(providers.map((provider) => provider.providerId));
  return [
    ...providers.flatMap((provider) => {
      const linked = identities.filter(
        (identity) => identity.providerId === provider.providerId,
      );
      if (linked.length === 0)
        return [
          {
            key: provider.providerId,
            providerId: provider.providerId,
            title: provider.title,
          },
        ];
      return linked.map((identity) => ({
        key: identity.identityId,
        providerId: provider.providerId,
        title: provider.title,
        identity,
      }));
    }),
    ...identities
      .filter((identity) => !offered.has(identity.providerId))
      .map((identity) => ({
        key: identity.identityId,
        providerId: null,
        title: identity.issuer,
        identity,
      })),
  ];
}
