/** One way to sign in on the Account page: a linked identity, or a provider to connect. */
export type SignInRow<Identity> = {
  key: string;
  /** The offered provider, or null for a provider that is no longer offered. */
  providerId: string | null;
  title: string;
  identity?: Identity;
  /** A row to connect another account through a provider already connected. */
  another?: true;
};

/**
 * Rows for the Account page's sign-in list, grouped by provider in the order
 * providers are offered. Every linked identity gets its own row, including
 * several linked through the same provider, so each can be disconnected on
 * its own. Each offered provider ends its group with one row to connect an
 * account: the first, or another one. Identities from providers no longer
 * offered follow, titled by their issuer.
 */
export function signInRows<
  Identity extends { identityId: string; providerId: string; issuer: string },
>(
  providers: readonly { providerId: string; title: string }[],
  identities: readonly Identity[],
): SignInRow<Identity>[] {
  const offered = new Set(providers.map((provider) => provider.providerId));
  return [
    ...providers.flatMap((provider): SignInRow<Identity>[] => {
      const linked = identities.filter(
        (identity) => identity.providerId === provider.providerId,
      );
      return [
        ...linked.map((identity) => ({
          key: identity.identityId,
          providerId: provider.providerId,
          title: provider.title,
          identity,
        })),
        {
          key: provider.providerId,
          providerId: provider.providerId,
          title: provider.title,
          ...(linked.length > 0 ? { another: true as const } : {}),
        },
      ];
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
