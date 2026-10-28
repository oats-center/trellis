import { createContext } from "svelte";

/** Reopens consent so the user can change what the Console may do. */
export type ChangeAccess = () => Promise<void>;

export const [getChangeAccess, setChangeAccess] = createContext<ChangeAccess>();
