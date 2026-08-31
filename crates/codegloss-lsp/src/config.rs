//! What the server is told at startup, and which engine that produces.
//!
//! One decision lives here: with a model pack, CodeGloss translates; without
//! one it echoes the English back. Both have to work. A server that refuses to
//! start because a pack is missing takes the editor's language support down
//! with it, and a user who installed the extension before downloading the
//! weights would see nothing at all and have nothing to read about why.

use std::path::PathBuf;
use std::sync::Arc;

use codegloss_core::{DEFAULT_CAPACITY, GlossCache, GlossStore};
use codegloss_translator::{PassthroughTranslator, Translator};

/// Command-line flag naming the model pack.
pub const MODEL_PACK_FLAG: &str = "--model-pack";
/// Environment variable naming the model pack, used when the flag is absent.
///
/// The flag is what the editor extension passes; the variable is what a person
/// debugging the server by hand reaches for.
pub const MODEL_PACK_VARIABLE: &str = "CODEGLOSS_MODEL_PACK";

/// Command-line flag choosing how the weights are held in memory.
pub const PRECISION_FLAG: &str = "--precision";
/// Environment variable for [`PRECISION_FLAG`]. See [`MODEL_PACK_VARIABLE`].
pub const PRECISION_VARIABLE: &str = "CODEGLOSS_MODEL_PRECISION";

/// Command-line flag naming the directory finished glosses are kept in.
pub const CACHE_DIR_FLAG: &str = "--cache-dir";
/// Environment variable for [`CACHE_DIR_FLAG`]. See [`MODEL_PACK_VARIABLE`].
pub const CACHE_DIR_VARIABLE: &str = "CODEGLOSS_CACHE_DIR";
/// Command-line flag turning the on-disk cache off entirely.
pub const NO_CACHE_FLAG: &str = "--no-cache";

/// How many glosses the directory keeps before the oldest are dropped.
///
/// A gloss is a sentence or two of UTF-8, so this is a few megabytes. The
/// count is what bounds it, and it is checked once per start
/// ([`GlossStore::open`]) rather than on every write.
pub const DISK_CACHE_CAPACITY: usize = 50_000;

/// Everything the server reads before it speaks LSP.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerConfig {
    /// Directory holding the weights, the tokenizers and the manifest.
    pub model_pack: Option<PathBuf>,
    /// `f32` (the default) or `f16`. Held as text rather than as
    /// `codegloss_translator::Precision` so that this type still exists in a
    /// binary built without the `candle` feature; it is parsed where the
    /// engine is built.
    ///
    /// F16 is the precision FuguMT is published in and halves what the model
    /// occupies, at some cost in time. The measured trade is in
    /// `docs/model-runtime-notes.md`.
    pub precision: Option<String>,
    /// Where finished glosses are kept between runs. `None` means the platform
    /// cache directory; [`no_cache`](ServerConfig::no_cache) means nowhere.
    pub cache_dir: Option<PathBuf>,
    /// Keep glosses in memory only, so that nothing is written outside the
    /// process. Translating is expensive enough that this is a real choice
    /// rather than a default, but a shared or read-only machine may want it.
    pub no_cache: bool,
}

impl ServerConfig {
    /// Reads the command line and the environment.
    pub fn from_environment() -> Self {
        Self::from_arguments(std::env::args().skip(1)).with_environment_fallback()
    }

    /// The command line alone. `--model-pack <dir>` and `--model-pack=<dir>`
    /// are both accepted; anything else is ignored rather than rejected, so a
    /// flag added by a future version of the extension cannot stop an older
    /// server from starting.
    pub fn from_arguments(arguments: impl IntoIterator<Item = String>) -> Self {
        let mut config = Self::default();
        let mut arguments = arguments.into_iter();

        while let Some(argument) = arguments.next() {
            if let Some(value) = argument.strip_prefix(&format!("{MODEL_PACK_FLAG}=")) {
                config.model_pack = Some(PathBuf::from(value));
            } else if argument == MODEL_PACK_FLAG {
                match arguments.next() {
                    Some(value) => config.model_pack = Some(PathBuf::from(value)),
                    None => tracing::warn!("{MODEL_PACK_FLAG} was given without a directory"),
                }
            } else if let Some(value) = argument.strip_prefix(&format!("{PRECISION_FLAG}=")) {
                config.precision = Some(value.to_owned());
            } else if argument == PRECISION_FLAG {
                match arguments.next() {
                    Some(value) => config.precision = Some(value),
                    None => tracing::warn!("{PRECISION_FLAG} was given without a value"),
                }
            } else if let Some(value) = argument.strip_prefix(&format!("{CACHE_DIR_FLAG}=")) {
                config.cache_dir = Some(PathBuf::from(value));
            } else if argument == CACHE_DIR_FLAG {
                match arguments.next() {
                    Some(value) => config.cache_dir = Some(PathBuf::from(value)),
                    None => tracing::warn!("{CACHE_DIR_FLAG} was given without a directory"),
                }
            } else if argument == NO_CACHE_FLAG {
                config.no_cache = true;
            } else {
                tracing::warn!(argument, "ignoring an unknown argument");
            }
        }

        config
    }

    fn with_environment_fallback(mut self) -> Self {
        if self.model_pack.is_none()
            && let Ok(path) = std::env::var(MODEL_PACK_VARIABLE)
            && !path.is_empty()
        {
            self.model_pack = Some(PathBuf::from(path));
        }
        if self.precision.is_none()
            && let Ok(precision) = std::env::var(PRECISION_VARIABLE)
            && !precision.is_empty()
        {
            self.precision = Some(precision);
        }
        if self.cache_dir.is_none()
            && let Ok(directory) = std::env::var(CACHE_DIR_VARIABLE)
            && !directory.is_empty()
        {
            self.cache_dir = Some(PathBuf::from(directory));
        }
        self
    }
}

/// The engine to run: candle when a pack is configured and loadable, the
/// passthrough otherwise.
///
/// IMPORTANT: this never fails and never panics. A corrupt pack, a path that
/// does not exist, a binary built without the `candle` feature - each of them
/// logs what happened and falls back. Nothing here can take the server down,
/// because taking the server down is worse than showing English.
pub fn engine(config: &ServerConfig) -> Arc<dyn Translator> {
    let Some(pack) = config.model_pack.as_deref() else {
        tracing::info!(
            "no model pack configured ({MODEL_PACK_FLAG} / {MODEL_PACK_VARIABLE}); \
             comments will be shown in English"
        );
        return Arc::new(PassthroughTranslator);
    };

    #[cfg(feature = "candle")]
    match codegloss_translator::CandleTranslator::load_with(pack, precision(config)) {
        Ok(translator) => return Arc::new(translator),
        Err(error) => tracing::error!(
            pack = %pack.display(),
            "the model pack could not be loaded, falling back to English: {error:#}"
        ),
    }

    #[cfg(not(feature = "candle"))]
    tracing::error!(
        pack = %pack.display(),
        "a model pack was configured but this binary was built without the \
         `candle` feature, so it has no engine to load it into"
    );

    Arc::new(PassthroughTranslator)
}

/// The cache the server answers requests from.
///
/// Backed by a directory whenever one can be found and opened, because that is
/// what makes reopening a file free: a gloss costs a third of a second to
/// produce and a few microseconds to read back. Everything that can go wrong -
/// no home directory, no permission, a full disk - falls back to memory only
/// and logs, on the same principle as [`engine`]: a cache is an optimisation,
/// and losing one must not cost the user their editor.
pub fn cache(config: &ServerConfig) -> GlossCache {
    if config.no_cache {
        tracing::info!("{NO_CACHE_FLAG}: glosses will be kept in memory only");
        return GlossCache::default();
    }

    let Some(directory) = cache_directory(config) else {
        tracing::info!(
            "no cache directory could be found ({CACHE_DIR_FLAG} / {CACHE_DIR_VARIABLE}); \
             glosses will be kept in memory only"
        );
        return GlossCache::default();
    };

    match GlossStore::open(&directory, DISK_CACHE_CAPACITY) {
        Ok(store) => {
            tracing::info!(
                directory = %store.directory().display(),
                glosses = store.len(),
                "glosses are kept between runs"
            );
            GlossCache::with_store(DEFAULT_CAPACITY, store)
        }
        Err(error) => {
            tracing::warn!(
                directory = %directory.display(),
                "the cache directory is unusable, keeping glosses in memory only: {error}"
            );
            GlossCache::default()
        }
    }
}

/// Where glosses go: what was configured, or the platform's cache directory.
///
/// The subdirectory is named after the crate rather than shared with anything
/// else, because [`GlossStore`] prunes what it finds there.
fn cache_directory(config: &ServerConfig) -> Option<PathBuf> {
    if let Some(directory) = &config.cache_dir {
        return Some(directory.clone());
    }
    Some(
        platform_cache_directory()?
            .join("codegloss")
            .join("glosses"),
    )
}

/// The directory this platform keeps caches in.
#[cfg(target_os = "windows")]
fn platform_cache_directory() -> Option<PathBuf> {
    non_empty("LOCALAPPDATA").map(PathBuf::from)
}

#[cfg(target_os = "macos")]
fn platform_cache_directory() -> Option<PathBuf> {
    Some(PathBuf::from(non_empty("HOME")?).join("Library/Caches"))
}

/// XDG: `XDG_CACHE_HOME` when it is set, `~/.cache` otherwise.
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn platform_cache_directory() -> Option<PathBuf> {
    match non_empty("XDG_CACHE_HOME") {
        Some(directory) => Some(PathBuf::from(directory)),
        None => Some(PathBuf::from(non_empty("HOME")?).join(".cache")),
    }
}

fn non_empty(variable: &str) -> Option<String> {
    std::env::var(variable)
        .ok()
        .filter(|value| !value.is_empty())
}

/// The configured precision, or the default when nothing was configured.
///
/// A value that is not a precision is a typo, and a typo must not stop the
/// server: it is logged and the default is used, exactly as a missing pack is.
#[cfg(feature = "candle")]
fn precision(config: &ServerConfig) -> codegloss_translator::Precision {
    use codegloss_translator::Precision;

    let Some(text) = config.precision.as_deref() else {
        return Precision::default();
    };
    Precision::parse(text).unwrap_or_else(|| {
        tracing::error!(
            "{PRECISION_FLAG} {text:?} is not f32, f16 or bf16; using {}",
            Precision::default()
        );
        Precision::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(arguments: &[&str]) -> ServerConfig {
        ServerConfig::from_arguments(arguments.iter().map(|argument| (*argument).to_owned()))
    }

    #[test]
    fn no_arguments_means_no_model_pack() {
        assert_eq!(parse(&[]).model_pack, None);
    }

    #[test]
    fn the_model_pack_can_be_given_in_either_spelling() {
        let separate = parse(&["--model-pack", "/models/fugumt"]);
        let joined = parse(&["--model-pack=/models/fugumt"]);

        assert_eq!(
            separate.model_pack.as_deref(),
            Some("/models/fugumt".as_ref())
        );
        assert_eq!(separate, joined);
    }

    /// A path with a space in it is one argument, not two: the editor passes
    /// the arguments as a list and never as a command line to re-split.
    #[test]
    fn a_path_with_a_space_survives() {
        let config = parse(&["--model-pack", "/Application Support/codegloss"]);
        assert_eq!(
            config.model_pack.as_deref(),
            Some("/Application Support/codegloss".as_ref())
        );
    }

    /// An argument the server does not know cannot stop it from starting.
    #[test]
    fn unknown_arguments_are_ignored() {
        let config = parse(&["--future-flag", "--model-pack", "/models", "--another"]);
        assert_eq!(config.model_pack.as_deref(), Some("/models".as_ref()));
    }

    /// A flag with nothing after it leaves the server without a pack rather
    /// than reading the next flag as a directory.
    #[test]
    fn a_dangling_flag_yields_no_pack() {
        assert_eq!(parse(&["--model-pack"]).model_pack, None);
    }

    /// Without a pack the engine is the passthrough, whether or not this
    /// binary has candle in it.
    #[test]
    fn an_unconfigured_server_runs_on_the_passthrough_engine() {
        let translator = engine(&ServerConfig::default());
        assert_eq!(
            translator.model_version(),
            codegloss_translator::PASSTHROUGH_MODEL_VERSION
        );
    }

    /// The fallback that keeps the editor alive: a pack that is not there is
    /// logged, not fatal.
    /// The precision is optional and has both spellings, like the pack.
    #[test]
    fn the_precision_can_be_given_in_either_spelling() {
        assert_eq!(parse(&[]).precision, None);
        assert_eq!(
            parse(&["--precision", "f16"]).precision.as_deref(),
            Some("f16")
        );
        assert_eq!(
            parse(&["--precision=f16"]).precision.as_deref(),
            Some("f16")
        );
        assert_eq!(parse(&["--precision"]).precision, None);
    }

    /// The cache directory has both spellings too, and can be refused outright.
    #[test]
    fn the_cache_directory_can_be_given_or_refused() {
        assert_eq!(parse(&[]).cache_dir, None);
        assert!(!parse(&[]).no_cache);
        assert_eq!(
            parse(&["--cache-dir", "/var/glosses"]).cache_dir.as_deref(),
            Some("/var/glosses".as_ref())
        );
        assert_eq!(
            parse(&["--cache-dir=/var/glosses"]).cache_dir.as_deref(),
            Some("/var/glosses".as_ref())
        );
        assert!(parse(&["--no-cache"]).no_cache);
    }

    /// A configured directory wins over the platform's, and `--no-cache`
    /// produces a cache with nothing behind it.
    #[test]
    fn the_configured_directory_is_the_one_used() {
        let directory =
            std::env::temp_dir().join(format!("codegloss-config-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);

        let config = ServerConfig {
            cache_dir: Some(directory.clone()),
            ..Default::default()
        };
        assert_eq!(
            cache_directory(&config).as_deref(),
            Some(directory.as_path())
        );

        let opened = cache(&config);
        assert_eq!(
            opened.store().map(|store| store.directory().to_owned()),
            Some(directory.clone())
        );

        let refused = ServerConfig {
            no_cache: true,
            ..config
        };
        assert!(cache(&refused).store().is_none());

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_missing_pack_falls_back_to_the_passthrough_engine() {
        let config = ServerConfig {
            model_pack: Some(PathBuf::from("/nonexistent/codegloss-model-pack")),
            ..Default::default()
        };
        assert_eq!(
            engine(&config).model_version(),
            codegloss_translator::PASSTHROUGH_MODEL_VERSION
        );
    }
}
