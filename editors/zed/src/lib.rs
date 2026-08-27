//! The CodeGloss Zed extension.
//!
//! Its entire job is to find `codegloss-lsp` and tell Zed how to start it.
//! No translation, no parsing, no caching happens here: this crate is compiled
//! to `wasm32-wasip2` and runs inside Zed's extension host, which is the wrong
//! place for a translation engine.
//!
//! Note: `zed::register_extension!` expands to code containing `unsafe`, so
//! `#![forbid(unsafe_code)]` cannot be used in this crate.

use zed_extension_api::{
    self as zed, Command, EnvVars, LanguageServerId, Os, Result, Worktree, settings::LspSettings,
};

/// File name of the language server binary this extension launches.
const SERVER_BINARY: &str = "codegloss-lsp";

/// The language server id.
///
/// This is the `[language_servers.<id>]` table key in `extension.toml`, and it
/// is also the key users write under `"lsp"` in their Zed `settings.json`. It
/// is *not* the human-readable `name` field of that table.
const LANGUAGE_SERVER_ID: &str = "codegloss";

struct CodeglossExtension {
    /// Path found on `PATH`, remembered so the lookup is not repeated on every
    /// language server start. Explicit settings still take precedence over it.
    cached_binary_path: Option<String>,
}

impl CodeglossExtension {
    /// Resolves the server binary and the arguments to start it with.
    fn server_binary(&mut self, worktree: &Worktree) -> Result<(String, Vec<String>)> {
        // Settings may be absent or malformed; that is not a reason to fail,
        // it just means falling through to the PATH lookup.
        let binary = LspSettings::for_worktree(LANGUAGE_SERVER_ID, worktree)
            .ok()
            .and_then(|settings| settings.binary);

        let arguments = binary
            .as_ref()
            .and_then(|binary| binary.arguments.clone())
            .unwrap_or_default();

        // 1. An explicit path from settings.json always wins. This is how a
        //    developer points Zed at a locally built debug binary.
        if let Some(path) = binary.as_ref().and_then(|binary| binary.path.clone()) {
            return Ok((path, arguments));
        }

        // 2. A path already resolved during this session.
        if let Some(path) = &self.cached_binary_path {
            return Ok((path.clone(), arguments));
        }

        // 3. Anything on PATH.
        if let Some(path) = worktree.which(SERVER_BINARY) {
            self.cached_binary_path = Some(path.clone());
            return Ok((path, arguments));
        }

        Err(format!(
            "{SERVER_BINARY} が見つかりません。\
             settings.json の lsp.{LANGUAGE_SERVER_ID}.binary.path に絶対パスを設定するか、\
             {SERVER_BINARY} を PATH の通った場所に置いてください。"
        ))
    }

    /// Environment for the server process.
    ///
    /// On Unix, Zed itself may be launched from a GUI without the user's shell
    /// environment, so the worktree's shell environment is passed through.
    /// Windows has no equivalent notion, hence the empty list there.
    fn server_env(worktree: &Worktree) -> EnvVars {
        let mut env = match zed::current_platform().0 {
            Os::Mac | Os::Linux => worktree.shell_env(),
            Os::Windows => Vec::new(),
        };

        if let Ok(settings) = LspSettings::for_worktree(LANGUAGE_SERVER_ID, worktree)
            && let Some(configured) = settings.binary.and_then(|binary| binary.env)
        {
            // Appended last so that explicit settings override the shell.
            env.extend(configured);
        }

        env
    }
}

impl zed::Extension for CodeglossExtension {
    fn new() -> Self {
        Self {
            cached_binary_path: None,
        }
    }

    fn language_server_command(
        &mut self,
        _language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Command> {
        let (command, args) = self.server_binary(worktree)?;
        Ok(Command {
            command,
            args,
            env: Self::server_env(worktree),
        })
    }
}

zed::register_extension!(CodeglossExtension);
