import assert from "node:assert/strict";
import { join } from "node:path";
import { test } from "node:test";

import {
  BUNDLED_SERVER_DIRECTORY,
  SERVER_BINARY,
  activatedLanguageIds,
  locateServer,
  serverArguments,
  type Environment,
  type ServerSettings,
} from "../src/server";

const EXTENSION = "/ext";

/** An environment where exactly the named paths are executable. */
function environment(
  executables: readonly string[],
  overrides: Partial<Environment> = {},
): Environment {
  return {
    isExecutable: (path) => executables.includes(path),
    searchPath: ["/usr/local/bin", "/usr/bin"],
    executableSuffix: "",
    ...overrides,
  };
}

const bundled = join(EXTENSION, BUNDLED_SERVER_DIRECTORY, SERVER_BINARY);

test("a configured path wins, and is taken as given", () => {
  // Not checked for existence on purpose: a spawn failure that names the
  // user's own setting beats silently running some other binary.
  const found = locateServer("/home/me/target/debug/codegloss-lsp", EXTENSION, environment([bundled]));
  assert.deepEqual(found, {
    kind: "configured",
    path: "/home/me/target/debug/codegloss-lsp",
  });
});

test("a blank configured path is not a path", () => {
  const found = locateServer("   ", EXTENSION, environment([bundled]));
  assert.equal(found.kind, "bundled");
});

test("the bundled server is preferred over one on PATH", () => {
  // The bundled binary is the build this extension version was published with.
  // Whatever is on PATH is some other build.
  const onPath = "/usr/bin/codegloss-lsp";
  const found = locateServer(undefined, EXTENSION, environment([bundled, onPath]));
  assert.deepEqual(found, { kind: "bundled", path: bundled });
});

test("PATH is searched when the VSIX bundles no server", () => {
  const onPath = "/usr/bin/codegloss-lsp";
  const found = locateServer(undefined, EXTENSION, environment([onPath]));
  assert.deepEqual(found, { kind: "path", path: onPath });
});

test("PATH is searched in order", () => {
  const first = "/usr/local/bin/codegloss-lsp";
  const second = "/usr/bin/codegloss-lsp";
  const found = locateServer(undefined, EXTENSION, environment([first, second]));
  assert.deepEqual(found, { kind: "path", path: first });
});

test("an empty PATH entry is skipped rather than read as the root", () => {
  const found = locateServer(
    undefined,
    EXTENSION,
    environment(["/codegloss-lsp"], { searchPath: ["", "/usr/bin"] }),
  );
  assert.equal(found.kind, "missing");
});

test("the executable suffix is part of the name that is looked for", () => {
  const windows = environment([join(EXTENSION, BUNDLED_SERVER_DIRECTORY, "codegloss-lsp.exe")], {
    executableSuffix: ".exe",
  });
  assert.equal(locateServer(undefined, EXTENSION, windows).kind, "bundled");
  // Without the suffix the same tree holds nothing this could run.
  assert.equal(locateServer(undefined, EXTENSION, environment([], {})).kind, "missing");
});

test("nothing anywhere is reported rather than guessed at", () => {
  assert.deepEqual(locateServer(undefined, EXTENSION, environment([])), { kind: "missing" });
});

const DEFAULTS: ServerSettings = {
  modelPack: null,
  precision: null,
  beams: null,
  cacheDirectory: null,
  cacheEnabled: true,
  downloadEnabled: true,
  extraArguments: [],
};

test("settings left at their defaults add no arguments", () => {
  // The server already has these defaults. Passing them anyway would put
  // choices on the command line that the user never made.
  assert.deepEqual(serverArguments(DEFAULTS), []);
});

test("each setting becomes the flag the server declares", () => {
  assert.deepEqual(
    serverArguments({
      ...DEFAULTS,
      modelPack: "/models/fugumt",
      precision: "f16",
      beams: 1,
      cacheDirectory: "/cache",
      cacheEnabled: false,
      downloadEnabled: false,
    }),
    [
      "--model-pack",
      "/models/fugumt",
      "--precision",
      "f16",
      "--beams",
      "1",
      "--cache-dir",
      "/cache",
      "--no-cache",
      "--no-download",
    ],
  );
});

test("a beam width that is not one is dropped rather than passed on", () => {
  for (const beams of [0, -1, 2.5, Number.NaN]) {
    assert.deepEqual(serverArguments({ ...DEFAULTS, beams }), [], `beams ${beams}`);
  }
});

test("a path of only spaces is not a path", () => {
  assert.deepEqual(
    serverArguments({ ...DEFAULTS, modelPack: "  ", cacheDirectory: "\t" }),
    [],
  );
});

test("extra arguments come last, so that they can override what is above", () => {
  assert.deepEqual(
    serverArguments({ ...DEFAULTS, beams: 4, extraArguments: ["--beams", "8"] }),
    ["--beams", "4", "--beams", "8"],
  );
});

test("the document selector is the manifest's own activation events", () => {
  // One list, in package.json. Writing it out again in the client options is
  // what would let activation and selection disagree.
  assert.deepEqual(
    activatedLanguageIds({
      activationEvents: ["onLanguage:rust", "onStartupFinished", "onLanguage:typescriptreact"],
    }),
    ["rust", "typescriptreact"],
  );
});

test("a manifest with nothing to say yields no languages", () => {
  for (const manifest of [{}, { activationEvents: "onLanguage:rust" }, null, undefined]) {
    assert.deepEqual(activatedLanguageIds(manifest), []);
  }
});
