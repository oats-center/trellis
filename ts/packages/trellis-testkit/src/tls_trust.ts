import { getCACertificates, setDefaultCACertificates } from "node:tls";
import { execFileSync } from "node:child_process";
import {
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

/** PEM contents by CA path; contents outlive runtimes whose files were removed. */
const trusted = new Map<string, string>();

/**
 * Adds a test runtime's generated installation CA to this process's TLS trust.
 *
 * Each runtime has its own CA. Node's `fetch` and NATS TLS verify every
 * runtime certificate against the explicitly trusted CAs; verification is
 * never disabled and system trust is never modified.
 */
export function trustInstallationCa(path: string): void {
  if (trusted.has(path)) return;
  const pem = readFileSync(path, "utf8");
  setDefaultCACertificates([...getCACertificates("default"), pem]);
  trusted.set(path, pem);
}

/**
 * Returns the installation CA files this process trusts, so spawned clients
 * such as test browsers can be given the same explicit trust.
 */
export function trustedInstallationCas(): readonly string[] {
  return [...trusted.keys()];
}

/**
 * Returns a process environment whose private NSS trust store trusts the
 * explicitly trusted installation CAs, for launching a test browser.
 *
 * Chromium on Linux reads user trust from `$HOME/.pki/nssdb`; this creates a
 * fresh temporary home holding only those CAs, exactly as a user who imported
 * the installation CA would have. It also includes the CA named by
 * `TRELLIS_TEST_CA_CERTIFICATE` for drivers started by another test process.
 * Requires NSS `certutil`. Never combine it with `ignoreHTTPSErrors`.
 */
export function browserTrustEnvironment(): NodeJS.ProcessEnv {
  const cas = [...trusted.values()];
  const inherited = process.env["TRELLIS_TEST_CA_CERTIFICATE"];
  if (inherited) cas.push(readFileSync(inherited, "utf8"));
  if (cas.length === 0) return { ...process.env };
  removeAbandonedHomes();
  const home = mkdtempSync(join(tmpdir(), `${HOME_PREFIX}${process.pid}-`));
  homes.add(home);
  const database = `sql:${join(home, ".pki", "nssdb")}`;
  mkdirSync(join(home, ".pki", "nssdb"), { recursive: true });
  execFileSync("certutil", ["-N", "-d", database, "--empty-password"]);
  cas.forEach((pem, index) => {
    const file = join(home, `installation-${index}.crt`);
    writeFileSync(file, pem);
    execFileSync("certutil", [
      "-A",
      "-d",
      database,
      "-n",
      `trellis-installation-${index}`,
      "-t",
      "C,,",
      "-i",
      file,
    ]);
  });
  return { ...process.env, HOME: home };
}

const HOME_PREFIX = "trellis-browser-trust-";
const homes = new Set<string>();
let swept = false;

/**
 * Removes trust homes whose creating process no longer exists. Homes are
 * removed at normal exit; this reclaims those stranded by a killed process.
 */
function removeAbandonedHomes(): void {
  if (swept) return;
  swept = true;
  for (const entry of readdirSync(tmpdir())) {
    const owner = Number(
      new RegExp(`^${HOME_PREFIX}(\\d+)-`).exec(entry)?.[1] ?? Number.NaN,
    );
    if (!Number.isInteger(owner) || owner === process.pid) continue;
    try {
      process.kill(owner, 0);
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === "ESRCH")
        rmSync(join(tmpdir(), entry), { recursive: true, force: true });
    }
  }
}
process.once("exit", () => {
  for (const home of homes) rmSync(home, { recursive: true, force: true });
});
