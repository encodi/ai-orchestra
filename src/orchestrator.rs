//! The orchestrator wires configuration, providers, the process manager and
//! the conversation store together. Milestone 1 exposes single-agent `ask`
//! and `doctor`; collaboration workflows build on the same pieces.

use std::sync::Arc;

use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::config::Config;
use crate::doctor::{DoctorReport, check_provider};
use crate::error::{OrchestraError, Result};
use crate::history::DebateStore;
use crate::memory::service::MemoryService;
use crate::provider::{AgentProvider, InvokeContext, ORCHESTRA_ACTIVE_ENV, ProviderRegistry};
use crate::providers::builtin_registry;
use crate::smart::profiles::{Observation, ProfileSnapshot, ProfileStore};
use crate::store::{Author, ConversationId, ConversationStore, EntryKind};
use crate::types::{AgentRequest, AgentResponse, ProviderId};

/// Upper bound on retained log entries per process.
const STORE_CAPACITY: usize = 10_000;

pub struct Orchestrator {
    config: Config,
    registry: ProviderRegistry,
    ctx: InvokeContext,
    store: Arc<ConversationStore>,
    history: Option<DebateStore>,
    profiles: Option<Arc<ProfileStore>>,
    memory: Option<Arc<MemoryService>>,
}

impl Orchestrator {
    /// Builds an orchestrator with the built-in Claude, Codex and Google adapters.
    pub fn new(config: Config) -> Result<Self> {
        let registry = builtin_registry(&config);
        Self::with_registry(config, registry)
    }

    /// Builds an orchestrator over a custom registry (used with mock providers).
    pub fn with_registry(config: Config, registry: ProviderRegistry) -> Result<Self> {
        let ctx = InvokeContext::from_config(&config);
        Self::with_context(config, registry, ctx)
    }

    pub fn with_context(
        config: Config,
        registry: ProviderRegistry,
        ctx: InvokeContext,
    ) -> Result<Self> {
        config.validate()?;
        // Catch typos like `[providers.claud]` instead of silently ignoring them.
        for name in config.providers.keys() {
            registry.resolve(name).map_err(|_| {
                OrchestraError::Config(format!(
                    "providers.{name}: unknown provider (known providers: {})",
                    registry.names().join(", ")
                ))
            })?;
        }
        // Bad model/effort values fail at startup, not on the first request.
        for provider in registry.all() {
            provider
                .resolve_settings(&AgentRequest::new(""))
                .map_err(|e| match e {
                    OrchestraError::Config(m) => {
                        OrchestraError::Config(format!("providers.{}: {m}", provider.id()))
                    }
                    other => other,
                })?;
        }
        registry.resolve(&config.general.coordinator).map_err(|_| {
            OrchestraError::Config(format!(
                "general.coordinator = \"{}\" is not a known provider (known providers: {})",
                config.general.coordinator,
                registry.names().join(", ")
            ))
        })?;
        Ok(Self {
            config,
            registry,
            ctx,
            store: Arc::new(ConversationStore::new(STORE_CAPACITY)),
            history: None,
            profiles: None,
            memory: None,
        })
    }

    /// Keeps a local history of how each provider performed (durations and outcomes only) and
    /// uses it to order agents. The CLI wires this up; library and test users get none unless
    /// they ask for it.
    pub fn with_profiles(mut self, store: ProfileStore) -> Self {
        self.profiles = Some(Arc::new(store));
        self
    }

    /// Attaches the memory service: consultations may then recall earlier work and remember new
    /// work, according to each request's memory mode. The CLI wires this up; library and test
    /// users get no memory unless they ask for it.
    pub fn with_memory(mut self, memory: Arc<MemoryService>) -> Self {
        self.memory = Some(memory);
        self
    }

    pub fn memory(&self) -> Option<&Arc<MemoryService>> {
        self.memory.as_ref()
    }

    pub fn profiles(&self) -> Option<&Arc<ProfileStore>> {
        self.profiles.as_ref()
    }

    /// The provider history as of now. Without a store, or if the file cannot be read, it is
    /// empty and the second value says why (so a plan can disclose it).
    pub fn profile_snapshot(&self) -> (ProfileSnapshot, Option<String>) {
        let settings = self.config.smart.profile_settings();
        let now = crate::history::now_ms();
        match &self.profiles {
            Some(store) => store.snapshot(now),
            None => (ProfileSnapshot::empty(settings, now), None),
        }
    }

    /// Adds a finished consultation's observed calls to the provider history. Best effort: a
    /// history that cannot be written never affects the consultation.
    pub(crate) fn record_profiles(&self, outcome: &crate::debate::DebateOutcome) {
        let Some(store) = &self.profiles else {
            return;
        };
        let observations: Vec<Observation> =
            outcome.steps.iter().map(Observation::from_step).collect();
        let availability: Vec<(String, bool, String)> = self
            .registry
            .all()
            .iter()
            .map(|p| {
                (
                    p.id().to_string(),
                    p.availability(&self.ctx).is_ok(),
                    format!("{:?}", p.research_capabilities().web_search),
                )
            })
            .collect();
        if let Err(why) = store.record(&observations, &availability, crate::history::now_ms()) {
            tracing::debug!(error = %why, "could not update the provider history");
        }
    }

    /// Saves every debate to `store` (the CLI wires this up; library and test
    /// users get no persistence unless they ask for it).
    pub fn with_history(mut self, store: DebateStore) -> Self {
        self.history = Some(store);
        self
    }

    pub fn history(&self) -> Option<&DebateStore> {
        self.history.as_ref()
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn registry(&self) -> &ProviderRegistry {
        &self.registry
    }

    pub fn context(&self) -> &InvokeContext {
        &self.ctx
    }

    pub fn store(&self) -> &Arc<ConversationStore> {
        &self.store
    }

    /// Refuses to run when an agent started by ai-orchestra is itself running
    /// ai-orchestra, which would be an unbounded orchestration loop.
    pub fn check_not_nested(&self) -> Result<()> {
        match self.ctx.depth_violation() {
            None => Ok(()),
            Some(marker) => Err(OrchestraError::NestedInvocation {
                provider: ProviderId::new("ai-orchestra"),
                marker: if self.ctx.host_env.contains(ORCHESTRA_ACTIVE_ENV)
                    && !self.ctx.host_env.contains(marker)
                {
                    ORCHESTRA_ACTIVE_ENV.to_string()
                } else {
                    marker.to_string()
                },
            }),
        }
    }

    /// Sends one prompt to one agent and records the exchange. Returns the
    /// conversation the exchange was logged under alongside the response.
    pub async fn ask(
        &self,
        provider: &str,
        request: AgentRequest,
        cancel: &CancellationToken,
    ) -> Result<(ConversationId, AgentResponse)> {
        self.check_not_nested()?;
        let provider = self.registry.resolve(provider)?;
        let conversation = self.store.new_conversation();
        let response = exchange(
            &self.store,
            &self.ctx,
            conversation,
            Author::User,
            provider,
            request,
            cancel,
        )
        .await?;
        Ok((conversation, response))
    }

    /// Checks every provider concurrently (bounded by the process manager).
    pub async fn doctor(&self) -> DoctorReport {
        let mut tasks = JoinSet::new();
        for (index, provider) in self.registry.all().iter().cloned().enumerate() {
            let ctx = self.ctx.clone();
            tasks.spawn(async move { (index, check_provider(provider.as_ref(), &ctx).await) });
        }
        let mut statuses = Vec::new();
        while let Some(joined) = tasks.join_next().await {
            if let Ok(entry) = joined {
                statuses.push(entry);
            }
        }
        statuses.sort_by_key(|(index, _)| *index);

        let mut warnings = Vec::new();
        let billing = self.ctx.billing_vars_present();
        if !billing.is_empty() {
            let action = if self.config.general.scrub_api_key_env {
                "they are removed from agent child processes (general.scrub_api_key_env = true)"
            } else {
                "they WILL be passed to agent processes and may cause usage-billed API access \
                 (general.scrub_api_key_env = false)"
            };
            warnings.push(format!(
                "API-key variables set in this environment: {}; {action}",
                billing.join(", ")
            ));
        }
        DoctorReport {
            providers: statuses.into_iter().map(|(_, s)| s).collect(),
            warnings,
        }
    }
}

/// One logged request/response with one agent. A free function (not a method)
/// so concurrent workflow tasks can run it without borrowing the orchestrator.
pub(crate) async fn exchange(
    store: &ConversationStore,
    ctx: &InvokeContext,
    conversation: ConversationId,
    prompt_author: Author,
    provider: Arc<dyn AgentProvider>,
    request: AgentRequest,
    cancel: &CancellationToken,
) -> Result<AgentResponse> {
    if !provider.config().enabled {
        return Err(OrchestraError::ProviderDisabled {
            provider: provider.id().clone(),
        });
    }
    store.record(
        conversation,
        prompt_author,
        EntryKind::Prompt,
        &request.prompt,
    );
    info!(provider = %provider.id(), access = ?request.access, "asking agent");

    let agent = Author::Agent(provider.id().clone());
    match provider.invoke(ctx, request, cancel).await {
        Ok(response) => {
            store.record(conversation, agent, EntryKind::Response, &response.text);
            Ok(response)
        }
        Err(e) => {
            store.record(conversation, agent, EntryKind::Error, &e.to_string());
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::AgentProvider;
    use crate::providers::mock::{MockProvider, MockReply};
    use std::time::Duration;

    fn orchestrator(
        providers: Vec<Arc<dyn AgentProvider>>,
        coordinator: &str,
    ) -> Result<Orchestrator> {
        let mut config = Config::default();
        config.general.coordinator = coordinator.to_string();
        Orchestrator::with_registry(config, ProviderRegistry::new(providers))
    }

    #[tokio::test]
    async fn ask_records_prompt_and_response() {
        let mock = Arc::new(MockProvider::new("alpha").with_reply(MockReply::Text("42".into())));
        let o = orchestrator(vec![mock.clone()], "alpha").unwrap();
        let (conv, resp) = o
            .ask(
                "alpha",
                AgentRequest::new("meaning?"),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(resp.text, "42");
        let entries = o.store().entries(conv);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].kind, EntryKind::Prompt);
        assert_eq!(entries[1].kind, EntryKind::Response);
        assert_eq!(mock.requests().len(), 1);
    }

    #[tokio::test]
    async fn ask_records_errors_with_agent_identity() {
        let mock =
            Arc::new(MockProvider::new("alpha").with_reply(MockReply::Quota("limit".into())));
        let o = orchestrator(vec![mock], "alpha").unwrap();
        let err = o
            .ask("ALPHA", AgentRequest::new("x"), &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(err, OrchestraError::Quota { .. }));
        assert_eq!(err.exit_code(), 4);
        let last = o.store().entries(ConversationId(1)).pop().unwrap();
        assert_eq!(last.kind, EntryKind::Error);
        assert!(matches!(last.author, Author::Agent(ref id) if id.as_str() == "alpha"));
    }

    #[tokio::test]
    async fn unknown_and_disabled_providers_are_typed_errors() {
        let o = orchestrator(vec![Arc::new(MockProvider::new("alpha"))], "alpha").unwrap();
        let err = o
            .ask("nope", AgentRequest::new("x"), &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(
            matches!(err, OrchestraError::UnknownProvider { ref known, .. } if known == "alpha")
        );

        // MockProvider carries its own config, so exercise `enabled = false`
        // through a built-in adapter, which reads the shared config.
        let o = Orchestrator::new({
            let mut c = Config::default();
            c.providers.insert(
                "codex".into(),
                crate::config::ProviderConfig {
                    enabled: false,
                    ..Default::default()
                },
            );
            c
        })
        .unwrap();
        let err = o
            .ask("codex", AgentRequest::new("x"), &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(err, OrchestraError::ProviderDisabled { .. }));
    }

    #[tokio::test]
    async fn config_typos_are_rejected() {
        let mut config = Config::default();
        config.providers.insert("claud".into(), Default::default());
        assert!(matches!(
            Orchestrator::new(config).err().unwrap(),
            OrchestraError::Config(_)
        ));
        let mut config = Config::default();
        config.general.coordinator = "gpt".into();
        assert!(matches!(
            Orchestrator::new(config).err().unwrap(),
            OrchestraError::Config(_)
        ));
        // Aliases are valid in config.
        let mut config = Config::default();
        config.providers.insert("agy".into(), Default::default());
        Orchestrator::new(config).unwrap();
    }

    #[tokio::test]
    async fn cancellation_reaches_the_provider() {
        let mock = Arc::new(MockProvider::new("slow").with_delay(Duration::from_secs(30)));
        let o = Arc::new(orchestrator(vec![mock], "slow").unwrap());
        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            c2.cancel();
        });
        let err = o
            .ask("slow", AgentRequest::new("x"), &cancel)
            .await
            .unwrap_err();
        assert!(matches!(err, OrchestraError::Cancelled { .. }));
    }

    #[tokio::test]
    async fn invalid_model_or_effort_in_config_fails_at_startup() {
        let mut config = Config::default();
        config.providers.insert(
            "claude".into(),
            crate::config::ProviderConfig {
                effort: Some("turbo".into()),
                ..Default::default()
            },
        );
        let err = Orchestrator::new(config).err().unwrap();
        assert!(
            matches!(&err, OrchestraError::Config(m) if m.contains("providers.claude") && m.contains("turbo")),
            "{err}"
        );
        let mut config = Config::default();
        config.providers.insert(
            "codex".into(),
            crate::config::ProviderConfig {
                model: Some("--dangerous".into()),
                ..Default::default()
            },
        );
        assert!(Orchestrator::new(config).is_err());
    }
}
