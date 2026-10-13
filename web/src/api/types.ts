// The shapes the Rust API returns (src/web/api/*). Optional fields mirror what the server may omit.

export type Usage = { input_tokens?: number | null; output_tokens?: number | null; cached_input_tokens?: number | null }

export type SettingsObj = {
  mode?: string | null
  coordinator?: string | null
  agents?: string[] | null
  rounds?: string | null
  depth?: string | null
  web?: string | null
  strategy?: string | null
  length?: string | null
  timeout?: number | null
  max_calls?: number | null
  memory?: string | null
  context_budget?: number | null
  view?: string | null
  max_sources?: number | null
  freshness_days?: number | null
  max_duration?: number | null
  max_parallel?: number | null
}
export type SettingsField = keyof SettingsObj

export type Choice = { value: string; label: string; available: boolean; reason?: string | null }
export type FieldSchema = {
  key: string // "max-calls": the CLI spelling; the JSON field is key.replace('-', '_')
  kind: 'choice' | 'multi' | 'integer'
  choices: Choice[]
  min: number | null
  max: number | null
  accepts: string
  disabled_reason: string | null
}
export type ProviderCap = {
  id: string
  display_name: string
  aliases: string[]
  enabled: boolean
  installed: boolean
  usable: boolean
  reason: string | null
  web_search: string
}
export type Capabilities = {
  version: string
  project: unknown
  providers: ProviderCap[]
  modes: string[]
  fields: FieldSchema[]
  safety: string[]
  limits: { max_concurrent_consultations: number; max_question_bytes: number; max_urls: number }
  features: { memory: boolean; history: boolean; evaluation: boolean; feedback: boolean; preferences: boolean }
  config_defaults: { memory: string; context_budget_tokens: number; default_strategy: string; timeout_secs: number }
  precedence: string[]
}

export type Origin = 'auto' | 'preference' | 'session' | 'request'
export type InForce = Record<string, { value: unknown; origin: Origin; origin_label: string }>

export type JobStatus = 'running' | 'completed' | 'partial' | 'failed' | 'cancelled'

export type EventRecord = { seq: number; ts_ms: number; kind: string; data: Record<string, unknown> }

export type CreateRequest = {
  question: string
  session_settings?: SettingsObj
  settings?: SettingsObj
  urls?: string[]
  session?: string | null
  new_session?: boolean
  remember?: boolean
  dry_run?: boolean
  explain_context?: boolean
}
export type CreateResponse = {
  job_id: string
  status: string
  events_url: string
  plan: unknown
  plan_text: string
  settings_in_force: InForce
}
export type DryRunResponse = {
  dry_run: true
  plan: unknown
  plan_text: string
  context: unknown
  settings_in_force: InForce
  note: string
}

export type Consultation = {
  id: string
  kind: 'consultation'
  saved: boolean
  status: string
  question: string
  created_at_ms: number
  duration_ms: number
  calls: number
  coordinator: string
  participants: string[]
  skipped: { agent: string; reason: string }[]
  synthesized: boolean
  final_answer: string
  warnings: string[]
  mode: string | null
  strategy: string | null
  routing_text: string | null
  meta: Record<string, unknown> | null
  research: { sources: number; retrieved: number; claims: number } | null
  age_days: number
  steps: number
  summaries: AiSummary[]
}
export type AiSummary = { id: string; agent: string; text: string; created_ms: number; duration_ms: number; report_truncated?: boolean; origin: string }
export type JobHeader = {
  job_id: string
  status: JobStatus
  created_ms: number
  finished_ms: number | null
  question_preview: string
  consultation_id: string | null
  settings_in_force: InForce
  error: { kind: string; message: string } | null
  events: number
  events_dropped: number
}
export type OneResponse = {
  kind: 'job' | 'saved'
  job?: JobHeader
  consultation?: Consultation
  feedback?: unknown[]
}

export type TheaterMessage = {
  index: number
  agent: string
  role: string
  stage: string
  round: number
  started_offset_ms: number
  finished_offset_ms: number
  duration_ms: number
  ok: boolean
  text: string | null
  error: string | null
  error_kind: string | null
  settings: string | null
  technical: boolean
  usage?: Usage | null
}
export type Timeline = {
  consultation_id: string
  status: string
  coordinator: string
  participants: string[]
  skipped: [string, string][]
  duration_ms: number
  messages: TheaterMessage[]
  source: string
  note: string
}

export type HistoryItem = {
  id: string
  created_at_ms: number
  status: string
  question: string
  mode?: string | null
  coordinator: string
  strategy?: string | null
  participants: string[]
  has_research?: boolean
  retrieved_sources?: number
  answer_preview?: string
  [k: string]: unknown
}
export type HistoryResponse = {
  items: HistoryItem[]
  total: number
  limit?: number
  offset?: number
  running: JobHeader[]
  history_enabled: boolean
}

export type Source = {
  id: number
  original_url: string
  final_url?: string | null
  canonical_url?: string | null
  display_url: string
  title?: string | null
  domain: string
  author?: string | null
  published?: string | null
  updated?: string | null
  retrieved_at_ms?: number | null
  age_days?: number | null
  status: string
  retrieved: boolean
  provenance: string
  error?: string | null
  error_kind?: string | null
  text_chars: number
  text_preview: string
  kind?: string
  kind_note?: string
  upstream_group?: number | null
  excerpts?: { text: string; offset: number }[]
  limitations?: string[]
  discovery?: { method: string; provider?: string | null }[]
}
export type EvidenceLink = {
  source_id: number
  relation: 'supports' | 'contradicts' | 'unrelated' | 'unclear'
  passage: string
  offset: number
  assessed_by: string[]
  suitability_note?: string
  reason?: string
}
export type Claim = {
  id: number
  text: string
  category?: string
  originating_agents: string[]
  needs_external_evidence?: boolean
  cited_by_agents?: number[]
  status: 'SUPPORTED' | 'CONTRADICTED' | 'MIXED_EVIDENCE' | 'INSUFFICIENT_EVIDENCE' | 'NOT_CHECKED' | string
  evidence: EvidenceLink[]
  supporting_independent_sources: number
  contradicting_independent_sources: number
  notes?: string[]
}
export type ResearchResponse = {
  consultation_id: string
  web_occurred: boolean
  note?: string
  retrieved_sources?: number
  depth?: string
  web_requested?: string
  max_sources?: number
  freshness_days?: number | null
  discovery_agent?: string | null
  sources?: Source[]
  claims?: Claim[]
  contradictions?: { claim_id: number; supporting_sources: number[]; contradicting_sources: number[] }[]
  citations?: { cited: number[]; unknown_removed: number[]; unretrieved_removed: number[] }
  assessors?: string[]
  limitations?: string[]
  search_queries?: string[]
  subquestions?: string[]
  budget?: Record<string, number>
  judgement_note?: string
  freshness_note?: string
}

export type ProviderRow = {
  id: string
  display_name: string
  aliases: string[]
  enabled: boolean
  executable: string | null
  installed: boolean
  version: string | null
  usable: boolean
  reason: string | null
  authentication: { status: string; note: string }
  capabilities: Record<string, unknown>
  configured_model: string | null
  configured_effort: string | null
  history: null | {
    invocations: number
    successes: number
    failures: number
    timeouts: number
    quota_errors: number
    auth_errors: number
    latency_ms: Record<string, { mean: number; samples: number }>
    last_success_ms: number | null
    last_failure_ms: number | null
    last_failure_kind: string | null
    failure_streak: number
    tokens_reported: { calls: number; input: number; output: number }
  }
}
export type ProvidersResponse = { providers: ProviderRow[]; history_note: string; checked_ms: number; cached_for_secs: number }

export type MemoryRecord = {
  id: string
  text: string
  scope: string
  kind: string
  origin?: string
  status?: string
  topic?: string | null
  tags?: string[]
  sensitive?: boolean
  masked?: boolean
  consultation_id?: string | null
  source_url?: string | null
  created_ms?: number
  updated_ms?: number
  retrieved_ms?: number | null
  expires_ms?: number | null
  authorization?: unknown
  score?: number
}
export type MemoryList = { items: MemoryRecord[]; project: unknown; note: string }
export type SessionRow = { id: string; [k: string]: unknown }

export type ReportRow = {
  id: string
  created_ms: number
  kind: string
  dataset: string
  dataset_title: string
  dataset_version: string
  synthetic: boolean
  arms: unknown
  cases: number
  provenance: { class: string; meaning: string }[]
  judgments: number
}
export type ReportsResponse = { items: ReportRow[]; note: string }
export type Analytics = {
  history_enabled: boolean
  consultations: number
  by_status?: Record<string, number>
  by_strategy?: Record<string, { consultations: number; median_duration_ms: number | null; median_calls: number | null; provider_failures: number }>
  by_mode?: Record<string, { consultations: number; median_duration_ms: number | null; median_calls: number | null; provider_failures: number }>
  by_provider?: Record<string, { recorded_calls: number; failed_calls: number; timeouts: number; median_success_ms: number | null }>
  tokens_reported_by_the_clis?: { calls_reporting: number; input: number; output: number; note: string }
  usage_by_agent?: Record<string, AgentUsage>
  quota?: { remaining_known: boolean; note: string }
  memory_context?: { consultations_with_memory: number; median_reduction_vs_full_history_percent: number | null; kind: string; note: string }
  kind?: string
  note?: string
}

export type UiPrefs = { theme: 'dark' | 'light' | 'system'; language: 'es' | 'en'; reduced_motion: 'system' | 'on' | 'off'; theater_default: boolean; show_stage: boolean }
export type Health = { ok: boolean; version: string; uptime_ms: number; running_consultations: number; frontend_built: boolean }

export type Appearance = { palette: number; accessory: 'none' | 'glasses' | 'cap' | 'headphones' | string }
export type CharacterEntry = { name: string; personality: string; appearance: Appearance }
export type CharactersFile = { reply_language: 'auto' | 'es' | 'en'; conversational: boolean; characters: Record<string, CharacterEntry> }
export type CharactersResponse = {
  settings: CharactersFile
  limits: { name_chars: number; personality_chars: number; palettes: number; accessories: string[] }
  note: string
  writable: boolean
}

export type UsageWindow = { calls: number; calls_reporting_tokens: number; input: number; output: number }
export type AgentUsage = {
  total: UsageWindow
  cached_input_tokens: number
  last_5_hours: UsageWindow
  last_24_hours: UsageWindow
  last_7_days: UsageWindow
  by_day: Record<string, UsageWindow>
  quota_errors: number
  last_quota_error_ms: number | null
}
