//! Reports: Milestone 8 evaluation runs and measurements of the consultations saved on this machine.
//!
//! Each number says what kind of number it is. Evaluation results carry their provenance (fixture, mock,
//! recorded, live) and a sentence saying what that means; the analytics are measurements of consultations
//! this machine actually ran (durations, calls, failures, tokens only where a CLI reported them); anything
//! derived (the context size against a full-history reference) is labelled an estimate. Nothing here
//! invents a token saving, a cost or a quality improvement.

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Path, State};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Q, S};
use crate::eval::{blind, report};
use crate::web::error::{ApiError, ApiResult};

fn store(state: &crate::web::AppState) -> ApiResult<&crate::eval::run::EvalStore> {
    state.evals.as_ref().ok_or_else(|| {
        ApiError::unavailable("the evaluation location is unknown (no home directory)")
    })
}

pub async fn list(State(state): S) -> ApiResult<Json<Value>> {
    let runs = store(&state)?.list();
    Ok(Json(json!({
        "items": runs.iter().map(|r| json!({
            "id": r.id, "created_ms": r.created_ms, "kind": r.kind, "dataset": r.dataset_id, "dataset_title": r.dataset_title,
            "dataset_version": r.dataset_version, "synthetic": r.synthetic, "arms": r.arms, "cases": r.cases.len(),
            "provenance": r.provenance_classes().iter().map(|p| json!({"class": p.as_str(), "meaning": p.describe()})).collect::<Vec<_>>(),
            "judgments": r.judgments.len(),
        })).collect::<Vec<_>>(),
        "note": "Evaluations are started from the terminal (`ai-orchestra eval ...`); a live evaluation spends subscription quota and needs an explicit flag there. This page only reads their results.",
    })))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OneQuery {
    pub seed: Option<u64>,
}

pub async fn one(
    State(state): S,
    Path(id): Path<String>,
    Q(q): Q<OneQuery>,
) -> ApiResult<Json<Value>> {
    let run = store(&state)?
        .load(&id)
        .map_err(|e| ApiError::not_found(e.to_string()))?;
    let seed = q.seed.unwrap_or(1);
    Ok(Json(json!({
        "run": {"id": run.id, "created_ms": run.created_ms, "kind": run.kind, "dataset": run.dataset_id, "dataset_title": run.dataset_title,
                "dataset_version": run.dataset_version, "dataset_hash": run.dataset_hash, "synthetic": run.synthetic, "arms": run.arms, "notes": run.notes},
        "results_are": run.provenance_classes().iter().map(|p| json!({"class": p.as_str(), "meaning": p.describe()})).collect::<Vec<_>>(),
        "summaries": report::summarize(&run),
        "tradeoffs": report::tradeoffs(&run),
        "pairs": blind::results(&run, seed),
        "judgments": run.judgments.iter().map(|j| json!({"evaluator": j.evaluator, "choice": j.choice, "recorded_ms": j.recorded_ms})).collect::<Vec<_>>(),
        "reading": "A preference is not an accuracy. Intervals appear only from 10 decided comparisons. Fixture and mock results describe fixtures and scripted agents, not any real model.",
        "text": report::render(&run, seed),
    })))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnalyticsQuery {
    pub from: Option<u64>,
    pub to: Option<u64>,
}

#[derive(Default)]
struct Window {
    calls: usize,
    reporting: usize,
    input: u64,
    output: u64,
}

impl Window {
    fn add(&mut self, usage: Option<&crate::types::Usage>) {
        self.calls += 1;
        if let Some(u) = usage.filter(|u| !u.is_empty()) {
            self.reporting += 1;
            self.input += u.input_tokens.unwrap_or(0);
            self.output += u.output_tokens.unwrap_or(0);
        }
    }

    fn json(&self) -> Value {
        json!({"calls": self.calls, "calls_reporting_tokens": self.reporting, "input": self.input, "output": self.output})
    }
}

/// What one agent used, as AI Orchestra recorded it: totals, rolling windows and the last two weeks by day.
#[derive(Default)]
struct AgentUsage {
    total: Window,
    cached: u64,
    last_5h: Window,
    last_24h: Window,
    last_7d: Window,
    by_day: BTreeMap<String, Window>,
    quota_errors: usize,
    last_quota_error_ms: Option<u64>,
}

#[derive(Default)]
struct Group {
    count: usize,
    durations: Vec<f64>,
    calls: Vec<f64>,
    failures: usize,
}

fn median(v: &[f64]) -> Option<f64> {
    crate::eval::stats::median(v)
}

pub async fn analytics(State(state): S, Q(q): Q<AnalyticsQuery>) -> ApiResult<Json<Value>> {
    let Some(history) = state.orch.history() else {
        return Ok(Json(json!({"history_enabled": false, "consultations": 0})));
    };
    let mut total = 0usize;
    let mut by_status: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_strategy: BTreeMap<String, Group> = BTreeMap::new();
    let mut by_mode: BTreeMap<String, Group> = BTreeMap::new();
    let mut by_provider: BTreeMap<String, (usize, usize, usize, Vec<f64>)> = BTreeMap::new();
    let (mut tok_in, mut tok_out, mut tok_calls) = (0u64, 0u64, 0usize);
    let (mut with_ctx, mut reductions): (usize, Vec<f64>) = (0, Vec::new());
    let mut usage: BTreeMap<String, AgentUsage> = BTreeMap::new();
    let now = crate::history::now_ms();
    history.for_each(|o| {
        if q.from.is_some_and(|f| o.created_at_ms < f) || q.to.is_some_and(|t| o.created_at_ms > t)
        {
            return;
        }
        total += 1;
        *by_status.entry(o.status.as_str().to_string()).or_default() += 1;
        let meta = o.consultation.as_ref();
        for (map, key) in [
            (
                &mut by_strategy,
                meta.map_or("debate".to_string(), |m| m.strategy.clone()),
            ),
            (
                &mut by_mode,
                meta.and_then(|m| m.mode)
                    .map_or("debate".to_string(), |m| m.to_string()),
            ),
        ] {
            let g = map.entry(key).or_default();
            g.count += 1;
            g.durations.push(o.duration_ms as f64);
            g.calls.push(o.calls as f64);
            g.failures += meta.map_or(0, |m| m.provider_failures);
        }
        for s in &o.steps {
            let at = o.created_at_ms.saturating_add(s.offset_ms);
            let age = now.saturating_sub(at);
            let u = usage.entry(s.agent.to_string()).or_default();
            u.total.add(s.usage.as_ref());
            u.cached += s
                .usage
                .as_ref()
                .and_then(|x| x.cached_input_tokens)
                .unwrap_or(0);
            if age <= 5 * 3_600_000 {
                u.last_5h.add(s.usage.as_ref());
            }
            if age <= 24 * 3_600_000 {
                u.last_24h.add(s.usage.as_ref());
            }
            if age <= 7 * 86_400_000 {
                u.last_7d.add(s.usage.as_ref());
            }
            if age <= 14 * 86_400_000 {
                let day = crate::history::format_utc(at)
                    .chars()
                    .take(10)
                    .collect::<String>();
                u.by_day.entry(day).or_default().add(s.usage.as_ref());
            }
            if s.error_kind.as_deref() == Some("quota") {
                u.quota_errors += 1;
                u.last_quota_error_ms = u.last_quota_error_ms.max(Some(at));
            }
            let e = by_provider.entry(s.agent.to_string()).or_default();
            e.0 += 1;
            if !s.ok {
                e.1 += 1;
                if s.error_kind.as_deref() == Some("timeout") {
                    e.2 += 1;
                }
            } else {
                e.3.push(s.duration_ms as f64);
            }
        }
        if let Some(t) = meta.and_then(|m| m.token_usage.as_ref()) {
            tok_in += t.input_tokens;
            tok_out += t.output_tokens;
            tok_calls += t.calls_reporting;
        }
        if let Some(c) = meta.and_then(|m| m.context.as_ref()) {
            with_ctx += 1;
            if let Some(r) = c.reduction_percent {
                reductions.push(r);
            }
        }
    });
    let group = |m: BTreeMap<String, Group>| -> Value {
        json!(m.into_iter().map(|(k, g)| (k, json!({
            "consultations": g.count, "median_duration_ms": median(&g.durations), "median_calls": median(&g.calls),
            "provider_failures": g.failures,
        }))).collect::<serde_json::Map<_, _>>())
    };
    Ok(Json(json!({
        "history_enabled": true, "consultations": total, "by_status": by_status,
        "by_strategy": group(by_strategy), "by_mode": group(by_mode),
        "by_provider": by_provider.into_iter().map(|(k, (calls, failures, timeouts, ok_ms))| (k, json!({
            "recorded_calls": calls, "failed_calls": failures, "timeouts": timeouts, "median_success_ms": median(&ok_ms),
        }))).collect::<serde_json::Map<_, _>>(),
        "usage_by_agent": usage.iter().map(|(k, u)| (k.clone(), json!({
            "total": u.total.json(), "cached_input_tokens": u.cached,
            "last_5_hours": u.last_5h.json(), "last_24_hours": u.last_24h.json(), "last_7_days": u.last_7d.json(),
            "by_day": u.by_day.iter().map(|(d, w)| (d.clone(), w.json())).collect::<serde_json::Map<_, _>>(),
            "quota_errors": u.quota_errors, "last_quota_error_ms": u.last_quota_error_ms,
        }))).collect::<serde_json::Map<_, _>>(),
        "quota": {
            "remaining_known": false,
            "note": "How much of each plan's quota is left (per session, day or week) is not exposed by Claude Code, Codex or Antigravity in a way AI Orchestra can read without a model call or without reading their private session files, so no remaining quota is shown. What is shown is what AI Orchestra itself recorded: calls and the tokens each CLI reported, in rolling windows, and the times a CLI reported its limit as reached.",
        },
        "tokens_reported_by_the_clis": {"calls_reporting": tok_calls, "input": tok_in, "output": tok_out, "note": "Only calls whose CLI reported usage are counted; nothing is estimated and no cost is computed."},
        "memory_context": {"consultations_with_memory": with_ctx, "median_reduction_vs_full_history_percent": median(&reductions),
            "kind": "estimate", "note": "Compared with the characters of the whole session history, a reference figure, not a provider token count; smaller is not automatically better."},
        "kind": "measurements of consultations saved on this machine",
        "note": "These are real measurements of real consultations, not benchmarks. Mock and fixture results appear only under Evaluations, labelled as such.",
    })))
}
