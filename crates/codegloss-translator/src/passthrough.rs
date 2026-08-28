//! The engine that does not translate.

use codegloss_core::Segment;

use crate::Translator;

/// Cache-key identity of [`PassthroughTranslator`].
///
/// The trailing number is the version of *this* engine's behaviour. Bumping it
/// invalidates everything it has cached, which is what a real model swap will
/// rely on as well.
pub const PASSTHROUGH_MODEL_VERSION: &str = "passthrough-1";

/// Returns every segment unchanged.
///
/// It exists so that the pipeline around it - the queue, the cache, the
/// background worker, the refresh requests - can be built and tested before a
/// real model is in the tree. Being instantaneous is exactly what makes it
/// dangerous: a handler that translates synchronously looks perfectly fine
/// against this engine and falls over against candle. Nothing may call
/// [`Translator::translate`] from a request handler, and the pipeline is built
/// as if this engine took a second per segment.
#[derive(Debug, Clone, Copy, Default)]
pub struct PassthroughTranslator;

impl Translator for PassthroughTranslator {
    fn translate(&self, segments: &[Segment]) -> anyhow::Result<Vec<String>> {
        Ok(segments
            .iter()
            .map(|segment| segment.text().to_owned())
            .collect())
    }

    fn model_version(&self) -> &str {
        PASSTHROUGH_MODEL_VERSION
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn every_segment_comes_back_unchanged_and_in_order() {
        let segments = [
            Segment::new("Return the cached user."),
            Segment::new("Fails when the id is unknown."),
        ];

        let translations = PassthroughTranslator
            .translate(&segments)
            .expect("passthrough cannot fail");

        assert_eq!(
            translations,
            vec![
                "Return the cached user.".to_owned(),
                "Fails when the id is unknown.".to_owned(),
            ]
        );
    }

    #[test]
    fn an_empty_batch_yields_an_empty_result() {
        let translations = PassthroughTranslator
            .translate(&[])
            .expect("passthrough cannot fail");
        assert!(translations.is_empty());
    }

    #[test]
    fn the_model_version_is_stable() {
        assert_eq!(PassthroughTranslator.model_version(), "passthrough-1");
    }

    /// The pipeline stores engines as `Arc<dyn Translator>`, so the trait has
    /// to stay dyn-compatible. A generic method or an `async fn` would break
    /// this and nothing else would notice until the server failed to compile.
    #[test]
    fn the_trait_is_usable_as_a_trait_object() {
        let translator: Arc<dyn Translator> = Arc::new(PassthroughTranslator);
        assert_eq!(translator.model_version(), PASSTHROUGH_MODEL_VERSION);
        assert_eq!(
            translator
                .translate(&[Segment::new("x")])
                .expect("passthrough cannot fail"),
            vec!["x".to_owned()]
        );
    }
}
