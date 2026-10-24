import { connect } from "@nats-io/transport-node";
import { connectNativeSession } from "@oatscenter/trellis/native";

/** Admit a real native client, then retain an ordinary non-renewing NATS attachment for wire/security checks. */
export async function admitRawNativeClient(
  login: Parameters<typeof connectNativeSession>[0],
) {
  const admitted = await connectNativeSession(login).orThrow();
  const own = admitted.owner.own();
  try {
    const authority = own.authority;
    const contextDigest = own.digest;
    const options = await login.session.auth.natsConnectOptions({
      inboxPrefix: authority.inboxPrefix,
      admission: login.session.admission,
    });
    const nc = await connect({
      servers: [...login.servers],
      ...options,
      reconnect: false,
    });
    try {
      await admitted.manager.close();
      return {
        nc,
        auth: login.session.auth,
        authority,
        contextDigest,
        ...options,
      };
    } catch (error) {
      await nc.close();
      throw error;
    }
  } finally {
    own.release();
    await admitted.manager.close();
  }
}

/** Real native proof material and its exact uncooperative physical attachment. */
export type RawNativeConnection = Awaited<
  ReturnType<typeof admitRawNativeClient>
>;
