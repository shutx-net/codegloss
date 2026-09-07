// Finding the language server, and building the command line to start it.
//
// Everything in this file is a pure function over an injected [`Environment`],
// so the decisions can be tested without a running VS Code. `extension.ts`
// supplies the real filesystem and the real settings.

import { constants, accessSync, statSync } from "node:fs";
import { delimiter, join } from "node:path";

/** File name of the language server this extension launches. */
export const SERVER_BINARY = "codegloss-lsp";

/**
 * Where a platform-specific VSIX keeps the server, relative to the extension's
 * own directory.
 *
 * The release workflow unpacks the release asset into this directory before
 * running `vsce package --target`, and `.vscodeignore` is written to keep it.
 * A VSIX built without a target has nothing here, which is why the search
 * continues onto `PATH` rather than stopping.
 */
export const BUNDLED_SERVER_DIRECTORY = "server";

/** The parts of the outside world the search depends on. */
export interface Environment {
  /** Whether the path names a file this process may execute. */
  isExecutable(path: string): boolean;
  /** `PATH`, already split into directories. */
  searchPath: readonly string[];
  /** `.exe` on Windows, `""` everywhere else. */
  executableSuffix: string;
}

/** Where the server was found, and which rule found it. */
export type ServerLocation =
  | { readonly kind: "configured" | "bundled" | "path"; readonly path: string }
  | { readonly kind: "missing" };

/**
 * Resolves the server binary.
 *
 * The order is the same one the Zed extension uses, minus its download step:
 * here the binary that goes with this extension is already inside it.
 *
 * 1. An explicit path from settings always wins, and is returned **without
 *    being checked**. Someone who names a path wants that path, and a spawn
 *    failure naming their own setting is a better message than this file
 *    silently moving on to a different binary.
 * 2. The bundled server. It is the build this extension version was published
 *    with, so it is preferred over whatever happens to be on `PATH` - the same
 *    reasoning that makes the Zed extension ask for its own release tag rather
 *    than the newest one.
 * 3. `PATH`. This is what a developer with a local build has, and it is also
 *    the only thing a VSIX built without a target can find.
 */
export function locateServer(
  configured: string | undefined,
  extensionPath: string,
  environment: Environment,
): ServerLocation {
  const configuredPath = configured?.trim();
  if (configuredPath) {
    return { kind: "configured", path: configuredPath };
  }

  const fileName = SERVER_BINARY + environment.executableSuffix;

  const bundled = join(extensionPath, BUNDLED_SERVER_DIRECTORY, fileName);
  if (environment.isExecutable(bundled)) {
    return { kind: "bundled", path: bundled };
  }

  for (const directory of environment.searchPath) {
    if (!directory) {
      continue;
    }
    const candidate = join(directory, fileName);
    if (environment.isExecutable(candidate)) {
      return { kind: "path", path: candidate };
    }
  }

  return { kind: "missing" };
}

/** The settings that become command-line arguments. */
export interface ServerSettings {
  readonly modelPack?: string | null;
  readonly precision?: string | null;
  readonly beams?: number | null;
  readonly cacheDirectory?: string | null;
  readonly cacheEnabled: boolean;
  readonly downloadEnabled: boolean;
  readonly extraArguments: readonly string[];
}

/**
 * Builds the server's command line.
 *
 * The flags are the server's own, spelled as `crates/codegloss-lsp/src/
 * config.rs` declares them. A setting left at its default contributes nothing:
 * the server already has that default, and passing it anyway would make the
 * command line say things the user did not ask for.
 *
 * `extraArguments` goes last so that it can override anything above it - the
 * server takes the last occurrence of a flag.
 */
export function serverArguments(settings: ServerSettings): string[] {
  const argv: string[] = [];

  const modelPack = settings.modelPack?.trim();
  if (modelPack) {
    argv.push("--model-pack", modelPack);
  }

  const precision = settings.precision?.trim();
  if (precision) {
    argv.push("--precision", precision);
  }

  // `0` is not a beam width, and neither is a fraction; either would have the
  // server fall back to its default anyway, with a warning nobody reads.
  const beams = settings.beams;
  if (typeof beams === "number" && Number.isInteger(beams) && beams >= 1) {
    argv.push("--beams", String(beams));
  }

  const cacheDirectory = settings.cacheDirectory?.trim();
  if (cacheDirectory) {
    argv.push("--cache-dir", cacheDirectory);
  }

  // These two are flags rather than values, so only the non-default shows up.
  if (!settings.cacheEnabled) {
    argv.push("--no-cache");
  }
  if (!settings.downloadEnabled) {
    argv.push("--no-download");
  }

  argv.push(...settings.extraArguments);
  return argv;
}

/**
 * The language ids this extension attaches the server to, read off its own
 * manifest.
 *
 * The list exists once, as `activationEvents` in `package.json`, and this
 * derives the document selector from it. Writing it out a second time here
 * would let activation and selection disagree: the extension would wake up for
 * a language it then ignored, or claim one it never woke up for, and neither
 * failure says anything on screen.
 *
 * Whether those ids are ones the server actually parses is a question this
 * file cannot answer - the registry that can is in another workspace. CI
 * compares the two (`.github/workflows/ci.yml`).
 */
export function activatedLanguageIds(manifest: unknown): string[] {
  const events = (manifest as { activationEvents?: unknown })?.activationEvents;
  if (!Array.isArray(events)) {
    return [];
  }

  const prefix = "onLanguage:";
  return events
    .filter((event): event is string => typeof event === "string")
    .filter((event) => event.startsWith(prefix))
    .map((event) => event.slice(prefix.length));
}

/** The real filesystem and process environment. */
export function systemEnvironment(): Environment {
  return {
    isExecutable(path: string): boolean {
      try {
        // Both checks matter: a directory named `codegloss-lsp` passes the
        // access check on some systems, and on Windows `X_OK` is not a real
        // permission bit, so `statSync` is what rules the directory out.
        if (!statSync(path).isFile()) {
          return false;
        }
        accessSync(path, constants.X_OK);
        return true;
      } catch {
        return false;
      }
    },
    searchPath: (process.env.PATH ?? "").split(delimiter),
    executableSuffix: process.platform === "win32" ? ".exe" : "",
  };
}
