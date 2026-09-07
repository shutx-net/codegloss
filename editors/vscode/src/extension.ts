// The CodeGloss VS Code extension.
//
// Its entire job is to find `codegloss-lsp` and tell VS Code how to start it.
// No translation, no parsing and no caching happens here: all of that lives in
// the server, which is a native binary shipped alongside this extension.

import * as vscode from "vscode";
import {
  LanguageClient,
  type Executable,
  type LanguageClientOptions,
  type ServerOptions,
} from "vscode-languageclient/node";

import {
  SERVER_BINARY,
  activatedLanguageIds,
  locateServer,
  serverArguments,
  systemEnvironment,
} from "./server";

/**
 * The language server id.
 *
 * It is the section name under which the client's own settings appear, and it
 * is what shows up in the "Output" dropdown. Changing it changes what users
 * have to look for.
 */
const LANGUAGE_SERVER_ID = "codegloss";

const RESTART_COMMAND = "codegloss.restartServer";

let client: LanguageClient | undefined;

export async function activate(context: vscode.ExtensionContext): Promise<void> {
  context.subscriptions.push(
    vscode.commands.registerCommand(RESTART_COMMAND, async () => {
      await stopClient();
      await startClient(context);
    }),
  );

  // Anything under `codegloss.` either becomes a command-line argument or
  // selects the binary, and neither can be changed in a running server.
  context.subscriptions.push(
    vscode.workspace.onDidChangeConfiguration(async (event) => {
      if (!event.affectsConfiguration(LANGUAGE_SERVER_ID)) {
        return;
      }
      await stopClient();
      await startClient(context);
    }),
  );

  await startClient(context);
}

export async function deactivate(): Promise<void> {
  await stopClient();
}

async function startClient(context: vscode.ExtensionContext): Promise<void> {
  const settings = vscode.workspace.getConfiguration(LANGUAGE_SERVER_ID);

  const location = locateServer(
    settings.get<string | null>("server.path") ?? undefined,
    context.extensionPath,
    systemEnvironment(),
  );
  if (location.kind === "missing") {
    await reportMissingServer();
    return;
  }

  const executable: Executable = {
    command: location.path,
    args: serverArguments({
      modelPack: settings.get<string | null>("model.pack"),
      precision: settings.get<string | null>("model.precision"),
      beams: settings.get<number | null>("model.beams"),
      cacheDirectory: settings.get<string | null>("cache.directory"),
      cacheEnabled: settings.get<boolean>("cache.enabled") ?? true,
      downloadEnabled: settings.get<boolean>("model.download") ?? true,
      extraArguments: settings.get<string[]>("server.arguments") ?? [],
    }),
  };

  // The same command for both: there is no separate debug build to launch.
  const serverOptions: ServerOptions = { run: executable, debug: executable };

  const clientOptions: LanguageClientOptions = {
    documentSelector: activatedLanguageIds(context.extension.packageJSON).map(
      (language) => ({ language }),
    ),
    // A server that cannot start is worth one message; a server that cannot
    // start on every keystroke is not. VS Code's default already behaves this
    // way, and this only says so out loud.
    outputChannelName: "CodeGloss",
  };

  client = new LanguageClient(
    LANGUAGE_SERVER_ID,
    "CodeGloss",
    serverOptions,
    clientOptions,
  );

  try {
    await client.start();
  } catch (error) {
    client = undefined;
    void vscode.window.showErrorMessage(
      `CodeGloss could not start ${location.path}: ${describe(error)}`,
    );
  }
}

async function stopClient(): Promise<void> {
  const running = client;
  client = undefined;
  if (!running) {
    return;
  }
  try {
    await running.stop();
  } catch {
    // A server that has already died cannot be stopped, and saying so helps
    // nobody: the restart that follows is what the user asked for.
  }
}

/**
 * Says that no server was found, and offers the one setting that fixes it.
 *
 * A platform-specific VSIX always has one, so reaching here means either a
 * VSIX built without a target - the platforms the release workflow has no
 * build for - or a checkout being run as a development extension.
 */
async function reportMissingServer(): Promise<void> {
  const openSettings = "Open Settings";
  const chosen = await vscode.window.showErrorMessage(
    `CodeGloss found no ${SERVER_BINARY}: this build of the extension ships none for ` +
      "this platform, and there is none on PATH. Build one from source and name it in " +
      "settings.",
    openSettings,
  );
  if (chosen === openSettings) {
    await vscode.commands.executeCommand(
      "workbench.action.openSettings",
      `${LANGUAGE_SERVER_ID}.server.path`,
    );
  }
}

function describe(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
