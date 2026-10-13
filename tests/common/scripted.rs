//! Scripted in-process agents shared by the Milestone 6 integration tests and the benchmark.
//!
//! An agent answers every kind of request by rule, recognising the request from marker phrases in
//! the prompt. It takes a position (`STANCE: <word>`), can end its answers with a claims block, can
//! be slow, and can fail. A fake verifier reads the passages in its prompt and says whether each
//! contains the number the claim states, so tests do not depend on call order or numbering.

#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use ai_orchestra::providers::mock::{MockProvider, MockReply};

pub const CLAIM: &str = "The Model X has 100 MHz of analog bandwidth";

/// Artificial latency of an ordinary scripted agent call and of the slow one.
pub const NORMAL_MS: u64 = 12;
pub const SLOW_MS: u64 = 120;

#[derive(Clone, Copy, Debug)]
pub enum Fault {
    /// Every call fails with a generic error.
    Always,
    /// Only the first call fails with a usage-limit error (and it is never retried).
    QuotaOnce,
}

/// How one scripted agent behaves.
#[derive(Clone)]
pub struct Behavior {
    pub delay_ms: u64,
    /// The position it takes when asked for a recommendation (`STANCE: <word>`).
    pub stance: &'static str,
    /// Ends its independent answer with a claims block for the bandwidth claim.
    pub claims: bool,
    pub fault: Option<Fault>,
}

impl Behavior {
    pub fn normal(stance: &'static str) -> Self {
        Self {
            delay_ms: NORMAL_MS,
            stance,
            claims: false,
            fault: None,
        }
    }
}

pub fn claims_block() -> String {
    format!(
        "```claims\n[{{\"claim\": \"{CLAIM}\", \"category\": \"product_specification\", \"sources\": [1], \"needs_external_evidence\": true}}]\n```"
    )
}

/// An honest fake verifier: a passage with "100 MHz" supports a 100 MHz claim, one with "70 MHz"
/// contradicts it, anything else is unrelated.
pub fn verdicts(prompt: &str) -> String {
    let mut out: Vec<serde_json::Value> = Vec::new();
    let mut claim_id = 0u64;
    let mut claim_text = String::new();
    let mut passages: Vec<(String, &'static str)> = Vec::new();
    let flush = |claim_id: u64,
                 passages: &mut Vec<(String, &'static str)>,
                 out: &mut Vec<serde_json::Value>| {
        if claim_id > 0 && !passages.is_empty() {
            out.push(serde_json::json!({
                "claim": claim_id,
                "passages": passages.iter().map(|(id, rel)| serde_json::json!({"id": id, "relation": rel, "reason": "by rule"})).collect::<Vec<_>>(),
            }));
        }
        passages.clear();
    };
    let mut rest = prompt;
    while let Some(i) = rest.find('<') {
        rest = &rest[i..];
        if let Some(r) = rest.strip_prefix("<claim id=\"") {
            flush(claim_id, &mut passages, &mut out);
            let end = r.find('"').unwrap_or(0);
            claim_id = r[..end].parse().unwrap_or(0);
            let body_start = r.find('>').map_or(0, |n| n + 1);
            let body_end = r.find("</claim>").unwrap_or(r.len());
            claim_text = r[body_start..body_end].to_string();
            rest = &r[body_end..];
        } else if let Some(r) = rest.strip_prefix("<passage id=\"") {
            let end = r.find('"').unwrap_or(0);
            let id = r[..end].to_string();
            let body_start = r.find('>').map_or(0, |n| n + 1);
            let body_end = r.find("</passage>").unwrap_or(r.len());
            let body = &r[body_start..body_end];
            let wants_100 = claim_text.contains("100 MHz");
            let rel = if wants_100 && body.contains("100 MHz") {
                "supports"
            } else if wants_100 && body.contains("70 MHz") {
                "contradicts"
            } else {
                "unrelated"
            };
            passages.push((id, rel));
            rest = &r[body_end..];
        } else {
            rest = &rest[1..];
        }
    }
    flush(claim_id, &mut passages, &mut out);
    serde_json::to_string(&out).unwrap_or_default()
}

/// The stance another assistant's text declares (`STANCE: word`), if any.
pub fn stances_in(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("STANCE:"))
        .map(|s| s.trim().to_string())
        .collect()
}

pub fn answer_for(b: &Behavior, prompt: &str) -> String {
    if prompt.contains("You are the research planner") {
        "{}".to_string()
    } else if prompt.contains("You are checking factual claims against passages") {
        verdicts(prompt)
    } else if prompt.contains("only AI assistant") {
        // The single-agent prompt asks for the final format; the answer still ends with the claims.
        let claims = if b.claims {
            claims_block()
        } else {
            String::new()
        };
        format!(
            "## Answer\n\nMy answer: {} [1].\nSTANCE: {}\n\n{claims}",
            b.stance, b.stance
        )
    } else if prompt.contains("<answer_under_review") {
        // A targeted review: the verdict compares the stance of the answer with its own.
        let theirs = stances_in(prompt);
        if theirs.iter().any(|s| s != b.stance) {
            format!(
                "I would choose {} instead; the answer weighs the trade-offs differently.\nVERDICT: MATERIAL_DISAGREEMENT",
                b.stance
            )
        } else {
            "The answer is consistent and complete; nothing material to add.\nVERDICT: AGREE"
                .to_string()
        }
    } else if prompt.contains("disagreed materially") {
        format!(
            "After the review I still hold: {}.\nSTANCE: {}",
            b.stance, b.stance
        )
    } else if prompt.contains("Write the FINAL answer") || prompt.contains("complete final answer")
    {
        format!(
            "## Answer\n\nBased on the answers and reviews, the recommendation is {} [1].\n\n## Significant disagreements\n\nNone.\n",
            b.stance
        )
    } else if prompt.contains("cross-review") {
        // A reviewer says the others disagree with it only if a declared stance differs.
        let theirs = stances_in(prompt);
        if theirs.iter().any(|s| s != b.stance) {
            format!(
                "Review: I disagree with the recommendation of the others; I would choose {}.",
                b.stance
            )
        } else {
            "Review: the answers are consistent; nothing material to add.".to_string()
        }
    } else {
        let claims = if b.claims {
            claims_block()
        } else {
            String::new()
        };
        format!(
            "## Answer\n\nMy answer: {} [1].\nSTANCE: {}\n\n{claims}",
            b.stance, b.stance
        )
    }
}

pub fn scripted(name: &str, b: &Behavior, web: bool) -> Arc<MockProvider> {
    let behavior = b.clone();
    let reply = MockReply::Responder(Arc::new(move |req| answer_for(&behavior, &req.prompt)));
    let mut m = MockProvider::new(name).with_delay(Duration::from_millis(b.delay_ms));
    match b.fault {
        // A scripted failure is consumed once, so queue plenty: this agent never works.
        Some(Fault::Always) => {
            for _ in 0..60 {
                m = m.with_reply(MockReply::Failure("scripted failure".into()));
            }
        }
        // The first call hits the usage limit and is never retried; later calls would work.
        Some(Fault::QuotaOnce) => {
            m = m
                .with_reply(MockReply::Quota("scripted usage limit".into()))
                .with_reply(reply);
        }
        None => m = m.with_reply(reply),
    }
    Arc::new(if web { m.with_web_search() } else { m })
}
