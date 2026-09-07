// Bundles the extension into the single CommonJS file `package.json` names as
// `main`. VS Code loads that file directly, so nothing may be left for npm to
// resolve at run time - which is also why the VSIX is packaged with
// `--no-dependencies`.
import { copyFileSync } from "node:fs";
import { context, build } from "esbuild";

const options = {
  entryPoints: ["src/extension.ts"],
  outfile: "dist/extension.js",
  bundle: true,
  format: "cjs",
  platform: "node",
  // VS Code hands the extension host's own version to the extension; bundling
  // a copy would give a second, non-functioning API object.
  external: ["vscode"],
  // The floor `package.json` declares through `engines.vscode`. VS Code 1.91
  // ships Electron 30, which is Node 20.
  target: "node20",
  sourcemap: true,
  minify: !process.argv.includes("--watch"),
};

if (process.argv.includes("--watch")) {
  await (await context(options)).watch();
} else {
  await build(options);
}

// `vsce` looks for a licence beside the manifest, and the repository keeps one
// at its root. Copying rather than committing a second one means there is
// still only one licence text to keep correct; `.gitignore` covers the copy.
copyFileSync("../../LICENSE", "LICENSE");
