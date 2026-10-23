import { humanizeName } from "$lib/format";
import { traverseAll } from "$lib/console/paging.ts";
import { getTrellis } from "$lib/trellis";

/**
 * Service names for pages that only hold IDs. Events name a service by its
 * deployment ID; jobs name it by its participant. A participant shared by
 * several services has no single name, so it falls back to its own.
 */
export function createServiceNames() {
  let byDeployment = $state.raw(new Map<string, string>());
  let byParticipant = $state.raw(
    new Map<string, { name: string; deploymentId: string }>(),
  );

  void traverseAll(async (page) => {
    const response = await getTrellis().deploymentsList({ page }).orThrow();
    return { items: response.items, cursor: response.page.nextCursor };
  })
    .then((result) => {
      byDeployment = new Map(
        result.items.map((item) => [
          item.deploymentId,
          humanizeName(item.title),
        ]),
      );
      const counts = new Map<string, number>();
      for (const item of result.items) {
        counts.set(
          item.participantId,
          (counts.get(item.participantId) ?? 0) + 1,
        );
      }
      byParticipant = new Map(
        result.items
          .filter((item) => counts.get(item.participantId) === 1)
          .map((item) => [
            item.participantId,
            { name: humanizeName(item.title), deploymentId: item.deploymentId },
          ]),
      );
    })
    .catch(() => {
      // Without the list, pages fall back to the names they already hold.
    });

  return {
    /** A service's name from its deployment ID, if known. */
    deployment(id: string | undefined): string | undefined {
      return id ? byDeployment.get(id) : undefined;
    },
    /** A service's name from its participant, else the participant's own name. */
    participant(id: string): string {
      return byParticipant.get(id)?.name ?? humanizeName(id);
    },
    /** The deployment ID of the one service running a participant, if unique. */
    deploymentFor(participantId: string): string | undefined {
      return byParticipant.get(participantId)?.deploymentId;
    },
  };
}
