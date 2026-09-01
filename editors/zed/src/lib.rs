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
    self as zed, Architecture, Command, DownloadedFileType, EnvVars, LanguageServerId,
    LanguageServerInstallationStatus, Os, Result, Worktree, github_release_by_tag_name,
    settings::LspSettings,
};

/// File name of the language server binary this extension launches.
const SERVER_BINARY: &str = "codegloss-lsp";

/// Where the server's release assets come from.
const SERVER_REPOSITORY: &str = "shutx-net/codegloss";

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
    fn server_binary(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<(String, Vec<String>)> {
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

        // 3. Anything on PATH. A developer with a local build usually has one.
        if let Some(path) = worktree.which(SERVER_BINARY) {
            self.cached_binary_path = Some(path.clone());
            return Ok((path, arguments));
        }

        // 4. The release. This is the path an ordinary install takes, and the
        //    reason installing the extension is enough.
        //
        // A failure is reported to Zed as well as returned: without it the
        // status is left saying "downloading" forever, and the reason the
        // server never came up is only in the log.
        let path = self.download(language_server_id).inspect_err(|error| {
            zed::set_language_server_installation_status(
                language_server_id,
                &LanguageServerInstallationStatus::Failed(error.clone()),
            );
        })?;
        self.cached_binary_path = Some(path.clone());
        Ok((path, arguments))
    }

    /// Downloads the server that goes with this extension, and returns where
    /// it landed.
    ///
    /// The release is looked up **by this extension's own version**, not by
    /// whichever is newest. The two are built and tested as a pair, and the
    /// release workflow refuses a tag whose version disagrees with the
    /// extension's, so asking for the matching tag is asking for the server
    /// this extension was built against. Taking the newest instead would
    /// silently pair an old extension with a new server.
    fn download(&self, language_server_id: &LanguageServerId) -> Result<String> {
        let asset = asset_name()?;
        // Scoped by version, so upgrading the extension fetches the server
        // that goes with it rather than reusing what is already on disk.
        let tag = format!("v{}", env!("CARGO_PKG_VERSION"));
        let directory = format!("{SERVER_BINARY}-{tag}");

        if let Some(path) = installed(&directory, &asset.stem) {
            return Ok(path);
        }

        zed::set_language_server_installation_status(
            language_server_id,
            &LanguageServerInstallationStatus::CheckingForUpdate,
        );
        let release = github_release_by_tag_name(SERVER_REPOSITORY, &tag).map_err(|error| {
            format!("{SERVER_REPOSITORY} のリリース {tag} が見つかりません: {error}")
        })?;
        let found = release
            .assets
            .iter()
            .find(|candidate| candidate.name == asset.file)
            .ok_or_else(|| {
                format!(
                    "リリース {tag} に {} がありません。\
                     この構成向けのバイナリが配られていません。",
                    asset.file
                )
            })?;

        zed::set_language_server_installation_status(
            language_server_id,
            &LanguageServerInstallationStatus::Downloading,
        );
        zed::download_file(&found.download_url, &directory, asset.kind)
            .map_err(|error| format!("{} をダウンロードできませんでした: {error}", asset.file))?;

        let path = installed(&directory, &asset.stem).ok_or_else(|| {
            format!(
                "{} を展開しましたが {SERVER_BINARY} が見つかりません。",
                asset.file
            )
        })?;
        zed::make_file_executable(&path)?;

        zed::set_language_server_installation_status(
            language_server_id,
            &LanguageServerInstallationStatus::None,
        );
        Ok(path)
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
        language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Command> {
        let (command, args) = self.server_binary(language_server_id, worktree)?;
        Ok(Command {
            command,
            args,
            env: Self::server_env(worktree),
        })
    }
}

zed::register_extension!(CodeglossExtension);

/// The release asset for the machine this is running on.
struct Asset {
    /// The asset's file name, as the release workflow spells it.
    file: String,
    /// The same without its extension, which is also the directory the archive
    /// unpacks into.
    stem: String,
    kind: DownloadedFileType,
}

/// Which asset this platform needs.
///
/// The names come from the release workflow's build matrix, so a target added
/// there has to be added here too - and a target missing here is a clear
/// message rather than a download that 404s.
fn asset_name() -> Result<Asset> {
    let (os, architecture) = zed::current_platform();
    let target = match (os, architecture) {
        (Os::Linux, Architecture::X8664) => "x86_64-unknown-linux-gnu",
        (Os::Linux, Architecture::Aarch64) => "aarch64-unknown-linux-gnu",
        (Os::Mac, Architecture::Aarch64) => "aarch64-apple-darwin",
        (Os::Mac, Architecture::X8664) => "x86_64-apple-darwin",
        (Os::Windows, Architecture::X8664) => "x86_64-pc-windows-msvc",
        (os, architecture) => {
            return Err(format!(
                "{os:?} の {architecture:?} 向けの {SERVER_BINARY} は配布していません。\
                 ソースからビルドして settings.json の \
                 lsp.{LANGUAGE_SERVER_ID}.binary.path に指定してください。"
            ));
        }
    };

    let stem = format!("{SERVER_BINARY}-{target}");
    let (extension, kind) = match os {
        Os::Windows => (".zip", DownloadedFileType::Zip),
        Os::Mac | Os::Linux => (".tar.gz", DownloadedFileType::GzipTar),
    };
    Ok(Asset {
        file: format!("{stem}{extension}"),
        stem,
        kind,
    })
}

/// The server inside `directory`, if it has already been unpacked there.
///
/// The archive holds a directory named after itself, so the binary normally
/// sits one level below what was asked for. The flat form is checked too, so
/// that a change in how Zed unpacks an archive shows up as a slower start
/// rather than as a download that never gets used.
///
/// The same resolution runs before and after the download, on purpose: a cache
/// check that looks somewhere else than the download writes never hits, and
/// nothing about that is visible except the network traffic.
fn installed(directory: &str, stem: &str) -> Option<String> {
    let suffix = executable_suffix();
    [
        format!("{directory}/{stem}/{SERVER_BINARY}{suffix}"),
        format!("{directory}/{SERVER_BINARY}{suffix}"),
    ]
    .into_iter()
    .find(|candidate| std::fs::metadata(candidate).is_ok_and(|found| found.is_file()))
}

fn executable_suffix() -> &'static str {
    match zed::current_platform().0 {
        Os::Windows => ".exe",
        Os::Mac | Os::Linux => "",
    }
}
