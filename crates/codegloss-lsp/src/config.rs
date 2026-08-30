//! What the server is told at startup, and which engine that produces.
//!
//! One decision lives here: with a model pack, CodeGloss translates; without
//! one it echoes the English back. Both have to work. A server that refuses to
//! start because a pack is missing takes the editor's language support down
//! with it, and a user who installed the extension before downloading the
//! weights would see nothing at all and have nothing to read about why.

use std::path::PathBuf;
use std::sync::Arc;

use codegloss_translator::{PassthroughTranslator, Translator};

/// Command-line flag naming the model pack.
pub const MODEL_PACK_FLAG: &str = "--model-pack";
/// Environment variable naming the model pack, used when the flag is absent.
///
/// The flag is what the editor extension passes; the variable is what a person
/// debugging the server by hand reaches for.
pub const MODEL_PACK_VARIABLE: &str = "CODEGLOSS_MODEL_PACK";

/// Everything the server reads before it speaks LSP.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerConfig {
    /// Directory holding the weights, the tokenizers and the manifest.
    pub model_pack: Option<PathBuf>,
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
    match codegloss_translator::CandleTranslator::load(pack) {
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
    #[test]
    fn a_missing_pack_falls_back_to_the_passthrough_engine() {
        let config = ServerConfig {
            model_pack: Some(PathBuf::from("/nonexistent/codegloss-model-pack")),
        };
        assert_eq!(
            engine(&config).model_version(),
            codegloss_translator::PASSTHROUGH_MODEL_VERSION
        );
    }
}
