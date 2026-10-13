//! What each provider CLI can do for web research, and how well that is known.
//!
//! Capabilities come from three kinds of evidence, recorded per entry so nothing is overstated:
//! `Verified` (observed with the installed CLI), `Documented` (named by the installed CLI's own
//! help, binary or official documentation, but not observed working here), `Unverified` (probably
//! so, not confirmed) and `Unsupported`. A provider is used for web discovery only when its search
//! is `Verified` or `Documented`, and every source it reports is still fetched independently
//! before it counts as evidence.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Support {
    /// Observed working with the installed CLI.
    Verified,
    /// Named by the CLI or its official documentation; not observed working here.
    Documented,
    /// Plausible, but not confirmed by anything we checked.
    Unverified,
    Unsupported,
}

impl Support {
    pub fn usable(self) -> bool {
        matches!(self, Self::Verified | Self::Documented)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Documented => "documented",
            Self::Unverified => "unverified",
            Self::Unsupported => "unsupported",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResearchCapabilities {
    /// The CLI can run a web search by itself when asked to.
    pub web_search: Support,
    /// The CLI can read a given URL by itself.
    pub web_fetch: Support,
    /// Search results come back as structured data with source URLs (not only as prose).
    pub exposes_source_urls: Support,
    /// How it is switched on in non-interactive mode, for the record.
    pub how: String,
    /// What was checked and what was not.
    pub notes: String,
}

impl ResearchCapabilities {
    /// A provider with no web capability.
    pub fn none(note: &str) -> Self {
        Self {
            web_search: Support::Unsupported,
            web_fetch: Support::Unsupported,
            exposes_source_urls: Support::Unsupported,
            how: "none".to_string(),
            notes: note.to_string(),
        }
    }

    /// Whether the provider may be asked to discover sources.
    pub fn can_discover(&self) -> bool {
        self.web_search.usable()
    }
}

/// A human-readable table of the capabilities of the given providers.
pub fn render_matrix(rows: &[(String, ResearchCapabilities)]) -> String {
    let mut out = String::from(
        "Research capability matrix (what the installed CLIs can do for web research)\n\n",
    );
    out.push_str(&format!(
        "{:<8} {:<12} {:<12} {:<14}  {}\n",
        "AGENT", "WEB SEARCH", "WEB FETCH", "SOURCE URLS", "ENABLED BY"
    ));
    for (id, c) in rows {
        out.push_str(&format!(
            "{:<8} {:<12} {:<12} {:<14}  {}\n",
            id,
            c.web_search.label(),
            c.web_fetch.label(),
            c.exposes_source_urls.label(),
            c.how
        ));
    }
    out.push_str("\nverified = observed working; documented = named by the CLI or its official docs, not observed here;\nunverified = plausible, unconfirmed; unsupported = not available or not enabled.\n\n");
    for (id, c) in rows {
        out.push_str(&format!("{id}: {}\n", c.notes));
    }
    out.push_str(
        "\nWhatever a provider reports, AI Orchestra retrieves the pages itself: a URL reported by a model\nis a candidate, and only a page this program fetched counts as evidence.\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_verified_or_documented_search_can_be_used_for_discovery() {
        assert!(Support::Verified.usable() && Support::Documented.usable());
        assert!(!Support::Unverified.usable() && !Support::Unsupported.usable());
        assert!(!ResearchCapabilities::none("x").can_discover());
    }

    #[test]
    fn the_matrix_states_what_is_and_is_not_known() {
        let rows = vec![
            (
                "claude".to_string(),
                ResearchCapabilities {
                    web_search: Support::Documented,
                    web_fetch: Support::Documented,
                    exposes_source_urls: Support::Unverified,
                    how: "--allowedTools WebSearch".into(),
                    notes: "n1".into(),
                },
            ),
            ("google".to_string(), ResearchCapabilities::none("n2")),
        ];
        let text = render_matrix(&rows);
        for needle in [
            "documented",
            "unverified",
            "unsupported",
            "n1",
            "n2",
            "retrieves the pages itself",
        ] {
            assert!(text.contains(needle), "{needle}\n{text}");
        }
    }
}
