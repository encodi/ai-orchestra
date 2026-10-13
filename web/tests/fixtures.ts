import type { Capabilities, Consultation, EventRecord, Timeline } from '../src/api/types'

const choice = (v: string, available = true, reason: string | null = null) => ({ value: v, label: v, available, reason })

export const caps: Capabilities = {
  version: '0.9.0',
  project: null,
  providers: [
    { id: 'claude', display_name: 'Claude Code', aliases: [], enabled: true, installed: true, usable: true, reason: null, web_search: 'documented' },
    { id: 'codex', display_name: 'OpenAI Codex CLI', aliases: [], enabled: true, installed: true, usable: true, reason: null, web_search: 'documented' },
    { id: 'google', display_name: 'Google Antigravity CLI', aliases: [], enabled: true, installed: true, usable: true, reason: null, web_search: 'unsupported' },
  ],
  modes: ['decision', 'research', 'review', 'learn', 'debate'],
  fields: [
    { key: 'mode', kind: 'choice', choices: ['decision', 'research', 'review', 'learn', 'debate'].map((v) => choice(v)), min: null, max: null, accepts: 'auto, decision', disabled_reason: null },
    { key: 'coordinator', kind: 'choice', choices: ['claude', 'codex', 'google'].map((v) => choice(v)), min: null, max: null, accepts: '', disabled_reason: null },
    { key: 'agents', kind: 'multi', choices: ['claude', 'codex', 'google'].map((v) => choice(v)), min: null, max: null, accepts: '', disabled_reason: null },
    { key: 'rounds', kind: 'choice', choices: [choice('1'), choice('2')], min: null, max: null, accepts: '', disabled_reason: null },
    { key: 'depth', kind: 'choice', choices: ['quick', 'standard', 'deep'].map((v) => choice(v)), min: null, max: null, accepts: '', disabled_reason: null },
    { key: 'web', kind: 'choice', choices: [choice('on', false, 'no enabled provider supports web search'), choice('off')], min: null, max: null, accepts: '', disabled_reason: null },
    { key: 'strategy', kind: 'choice', choices: ['fast', 'balanced', 'thorough'].map((v) => choice(v)), min: null, max: null, accepts: '', disabled_reason: null },
    { key: 'length', kind: 'choice', choices: [choice('brief'), choice('full')], min: null, max: null, accepts: '', disabled_reason: null },
    { key: 'timeout', kind: 'integer', choices: [], min: 1, max: 3600, accepts: '', disabled_reason: null },
    { key: 'max-calls', kind: 'integer', choices: [], min: 1, max: 50, accepts: '', disabled_reason: null },
    { key: 'memory', kind: 'choice', choices: ['off', 'session', 'project', 'global'].map((v) => choice(v)), min: null, max: null, accepts: '', disabled_reason: 'memory is not available in this server' },
    { key: 'context-budget', kind: 'integer', choices: [], min: 1, max: 100000, accepts: '', disabled_reason: null },
    { key: 'view', kind: 'choice', choices: [choice('plain'), choice('detailed')], min: null, max: null, accepts: '', disabled_reason: null },
    { key: 'max-sources', kind: 'integer', choices: [], min: 1, max: 20, accepts: '', disabled_reason: null },
    { key: 'freshness-days', kind: 'integer', choices: [], min: 1, max: 3650, accepts: '', disabled_reason: null },
    { key: 'max-duration', kind: 'integer', choices: [], min: 1, max: 3600, accepts: '', disabled_reason: null },
    { key: 'max-parallel', kind: 'integer', choices: [], min: 1, max: 8, accepts: '', disabled_reason: null },
  ],
  safety: ['Agents are read-only.', 'Nothing an agent says is executed.'],
  limits: { max_concurrent_consultations: 2, max_question_bytes: 100000, max_urls: 20 },
  features: { memory: true, history: true, evaluation: true, feedback: true, preferences: true },
  config_defaults: { memory: 'auto', context_budget_tokens: 1200, default_strategy: 'auto', timeout_secs: 300 },
  precedence: ['safety rules', 'this consultation', 'this session', 'saved defaults', 'automatic'],
}

let seq = 0
export const ev = (kind: string, data: Record<string, unknown> = {}, ts = 1_000_000 + seq * 100): EventRecord => ({ seq: seq++, ts_ms: ts, kind, data })
export const resetSeq = () => {
  seq = 0
}

/** A complete three-agent run, as the server would record it. */
export function threeAgentRun(): EventRecord[] {
  resetSeq()
  return [
    ev('planning_started', { settings: {} }),
    ev('strategy_selected', { plan_text: 'Mode: decision · panel' }),
    ev('consultation_created', { consultation_id: 'c-1', mode: 'decision', coordinator: 'claude', participants: ['codex', 'google'], skipped: [], rounds: 1 }),
    ev('provider_started', { agent: 'claude', role: 'perspective', round: 0, stage: 'independent_analysis' }),
    ev('provider_started', { agent: 'codex', role: 'perspective', round: 0, stage: 'independent_analysis' }),
    ev('provider_completed', { agent: 'codex', role: 'perspective', round: 0, stage: 'independent_analysis', ok: true, duration_ms: 900 }),
    ev('agent_message_recorded', { agent: 'codex', role: 'perspective', round: 0, stage: 'independent_analysis', technical: false, text: 'Codex says buy A.', duration_ms: 900 }),
    ev('provider_completed', { agent: 'claude', role: 'perspective', round: 0, stage: 'independent_analysis', ok: true, duration_ms: 1200 }),
    ev('agent_message_recorded', { agent: 'claude', role: 'perspective', round: 0, stage: 'independent_analysis', technical: false, text: 'Claude says buy B.', duration_ms: 1200 }),
  ]
}

export const consultation = (over: Partial<Consultation> = {}): Consultation => ({
  id: 'c-1', kind: 'consultation', saved: true, status: 'complete', question: 'Which one?', created_at_ms: 1_700_000_000_000, duration_ms: 5000, calls: 4,
  coordinator: 'claude', participants: ['codex', 'google'], skipped: [], synthesized: true, final_answer: '## Answer\n\nBuy **B** [1].', warnings: [],
  mode: 'decision', strategy: 'panel', routing_text: 'Mode: decision', meta: {}, research: null, age_days: 1, steps: 4, summaries: [], ...over,
})

export const timeline = (): Timeline => ({
  consultation_id: 'c-1', status: 'partial', coordinator: 'claude', participants: ['codex', 'google'], skipped: [], duration_ms: 6000,
  source: 'recorded', note: 'Each message is the text an agent actually returned. Nothing is simulated or added.',
  messages: [
    { index: 0, agent: 'claude', role: 'perspective', stage: 'independent_analysis', round: 0, started_offset_ms: 0, finished_offset_ms: 1000, duration_ms: 1000, ok: true, text: 'Claude says B.', error: null, error_kind: null, settings: null, technical: false },
    { index: 1, agent: 'codex', role: 'perspective', stage: 'independent_analysis', round: 0, started_offset_ms: 0, finished_offset_ms: 2000, duration_ms: 2000, ok: false, text: null, error: 'timed out', error_kind: 'timeout', settings: null, technical: false },
    { index: 2, agent: 'claude', role: 'synthesis', stage: 'synthesis', round: 1, started_offset_ms: 3000, finished_offset_ms: 6000, duration_ms: 3000, ok: true, text: '## Answer\n\nFinal.', error: null, error_kind: null, settings: null, technical: false },
  ],
})
