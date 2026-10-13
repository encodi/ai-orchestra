//! Token accounting, honestly labelled.
//!
//! The only exact token counts are the ones a provider's CLI reports about a call that already
//! happened. Before a call, nothing here knows how a provider will tokenize a text, and different
//! providers tokenize the same text differently, so this module offers one **conservative estimate**
//! (bytes divided by three, which over-counts ordinary English prose by roughly a third) and says
//! so wherever it is shown. An estimate is never presented as a provider's count or as billing.

/// Bytes per estimated token. Smaller than the usual 4 characters per token, so that a context
/// that fits the estimate also fits the real count for ordinary prose.
pub const BYTES_PER_ESTIMATED_TOKEN: usize = 3;

pub const ESTIMATE_LABEL: &str = "estimated tokens (bytes / 3, conservative; not a provider count)";

pub fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(BYTES_PER_ESTIMATED_TOKEN)
}

/// A token figure and how much to trust it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TokenFigure {
    pub tokens: u64,
    /// `true`: reported by the provider's CLI for a finished call. `false`: our estimate.
    pub exact: bool,
}

impl TokenFigure {
    pub fn estimated(text: &str) -> Self {
        Self {
            tokens: estimate_tokens(text) as u64,
            exact: false,
        }
    }

    pub fn reported(tokens: u64) -> Self {
        Self {
            tokens,
            exact: true,
        }
    }

    pub fn label(&self) -> String {
        if self.exact {
            format!("{} tokens (reported by the provider)", self.tokens)
        } else {
            format!("~{} tokens (estimated)", self.tokens)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_estimate_is_conservative_and_labelled() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abc"), 1);
        assert_eq!(estimate_tokens("abcd"), 2);
        // Ordinary English runs about 4 characters per token: the estimate is above that.
        let prose = "The oscilloscope shows a voltage waveform over time ".repeat(20);
        assert!(estimate_tokens(&prose) > prose.len() / 4);
        assert!(!TokenFigure::estimated("abc").exact);
        assert!(
            TokenFigure::reported(5)
                .label()
                .contains("reported by the provider")
        );
        assert!(
            TokenFigure::estimated("abcdef")
                .label()
                .contains("estimated")
        );
        assert!(ESTIMATE_LABEL.contains("not a provider count"));
    }
}
