//! The real-model sweep runner (M6-09, ADR-0043; protocol fixed by M6-08).
//!
//! **Needle-QA trials**: the model receives a compiled-pack-shaped context
//! built from the SAME deterministic corpus as the harness, with the
//! decisive fact's placement forced per trial (the position knob — the pack
//! spine itself never reorders), buried under `saturation` prior-session
//! packs of noise; the oracle is a normalized contains-match on the
//! decisive-fact token. The driver is **model-agnostic** (ids are config
//! data), **spend-capped** (hard abort + partial-results salvage), retries
//! an API error once, and **records every trial** through the transport
//! seam as dedup'd JSONL fixtures so analysis and `EvalDefaults` derivation
//! replay offline (no API key, no network).
//!
//! The REAL transport lives behind the `real-model` cargo feature and an
//! explicit CLI invocation (`rift-memd eval --real-model`) — never reachable
//! from `cargo test`; unit tests drive the sweep through a mock transport.
//! API keys arrive via environment at call time and are never stored,
//! persisted, or logged (errors carry identifiers, never key bytes).

use core::fmt::Write as _;
use std::collections::BTreeMap;

use rift_recall_substrate::worktree::sha256_hex;

use crate::{deterministic_corpus, Scenario};

/// One model under sweep: the id is data (a new model is a config edit),
/// prices pin the spend accounting, the key env VAR NAME (never the key) is
/// part of the family config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ModelSpec {
    /// The provider family (`anthropic` | `openai`).
    pub family: String,
    /// The provider's model id, verbatim.
    pub id: String,
    /// Input price, USD per million tokens (pinned at config time).
    pub price_in_per_mtok: f64,
    /// Output price, USD per million tokens.
    pub price_out_per_mtok: f64,
}

/// The forced decisive-fact placement (the position knob).
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum Position {
    /// The fact leads the context.
    Begin,
    /// The fact sits mid-context.
    Middle,
    /// The fact trails the context.
    End,
    /// The fact appears at BOTH edges (the both-ends remedy).
    Duplicated,
}

impl Position {
    /// Every position, sweep order.
    pub const ALL: [Self; 4] = [Self::Begin, Self::Middle, Self::End, Self::Duplicated];

    /// The stable config/report tag.
    #[must_use]
    pub fn tag(self) -> &'static str {
        match self {
            Self::Begin => "begin",
            Self::Middle => "middle",
            Self::End => "end",
            Self::Duplicated => "duplicated",
        }
    }
}

/// The versioned sweep configuration (committed beside the fixtures it
/// produces; the provenance stamp `EvalDefaults` carries).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SweepConfig {
    /// Config version (bump on any change).
    pub version: u32,
    /// The models under sweep (M6-08: the full ladder, both families).
    pub models: Vec<ModelSpec>,
    /// Prior-session saturation levels.
    pub saturations: Vec<u32>,
    /// Trials per (position × saturation × query-shape) cell.
    pub trials_per_cell: u32,
    /// Sampling temperature (pinned; 0 = deterministic where supported).
    pub temperature: f64,
    /// The per-sweep spend cap in USD — hard abort + salvage past it.
    pub spend_cap_usd: f64,
}

impl SweepConfig {
    /// The M6-08 v1 config: full ladder (both families, top/mid/small),
    /// 3 trials/cell, $50 cap, temperature 0. OpenAI ids/prices are pinned
    /// at run time by the owner (config data, not code); the placeholders
    /// here name the tier so the config is complete without asserting ids
    /// this repo cannot know.
    #[must_use]
    pub fn v1() -> Self {
        let m = |family: &str, id: &str, pin: f64, pout: f64| ModelSpec {
            family: family.into(),
            id: id.into(),
            price_in_per_mtok: pin,
            price_out_per_mtok: pout,
        };
        Self {
            version: 1,
            models: vec![
                m("anthropic", "claude-opus-4-8", 15.0, 75.0),
                m("anthropic", "claude-sonnet-5", 3.0, 15.0),
                m("anthropic", "claude-haiku-4-5-20251001", 0.8, 4.0),
                m("openai", "openai-top-tier-pin-at-run", 10.0, 40.0),
                m("openai", "openai-mid-tier-pin-at-run", 2.5, 10.0),
                m("openai", "openai-small-tier-pin-at-run", 0.6, 2.4),
            ],
            saturations: vec![1, 2, 5, 10],
            trials_per_cell: 3,
            temperature: 0.0,
            spend_cap_usd: 50.0,
        }
    }
}

/// One cell's coordinates in the sweep grid.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TrialCell {
    /// The model id under trial.
    pub model: String,
    /// The forced decisive-fact placement.
    pub position: Position,
    /// Prior-session packs of noise the fact is buried under.
    pub saturation: u32,
    /// Query-aware (task + focus in the question) vs bare.
    pub query_aware: bool,
    /// Trial ordinal within the cell (0-based).
    pub trial: u32,
}

/// One recorded trial (the `trials.jsonl` row).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TrialRecord {
    /// The cell coordinates.
    pub cell: TrialCell,
    /// Content-addressed ref into `packs.jsonl` (the dedup'd context).
    pub pack_sha256: String,
    /// The question asked.
    pub question: String,
    /// The model's full response text.
    pub response: String,
    /// Oracle verdict: normalized contains-match on the decisive fact.
    pub success: bool,
    /// Input tokens the provider reported.
    pub tokens_in: u64,
    /// Output tokens the provider reported.
    pub tokens_out: u64,
    /// The trial's cost at the config's pinned prices, in micro-USD
    /// (integer — fixtures stay byte-deterministic and round-trip exactly).
    pub cost_micro_usd: u64,
}

/// What a transport returns for one completion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransportReply {
    /// The response text.
    pub text: String,
    /// Provider-reported input tokens.
    pub tokens_in: u64,
    /// Provider-reported output tokens.
    pub tokens_out: u64,
}

/// The transport seam (ADR-0043): ONE trait over both families' HTTP APIs.
/// The real impl is feature-gated (`real-model`) and owner-invoked; tests
/// drive the sweep through mocks, so no test can reach the network.
pub trait ModelTransport {
    /// Complete one prompt against `model`.
    ///
    /// # Errors
    /// A transport/API failure (identifier/reason only — never key bytes).
    fn complete(
        &mut self,
        model: &ModelSpec,
        prompt: &str,
        temperature: f64,
    ) -> Result<TransportReply, String>;
}

/// The needle-QA context for one trial: the corpus scenario's pack-shaped
/// text with the decisive-fact line FORCED to `position`, buried under
/// `saturation` prior-session packs of factless noise. The spine itself is
/// never reordered — the knob moves the fact's carrying line only.
#[must_use]
pub fn trial_context(scenario: &Scenario, position: Position, saturation: u32) -> String {
    let fact_line = format!("{}\n", scenario.decisive_fact);
    let body: String = scenario
        .files
        .iter()
        .map(|f| {
            let mut section = format!("## {} \n", f.path);
            for l in f
                .content
                .lines()
                .filter(|l| !l.contains(&scenario.decisive_fact))
            {
                section.push_str(l);
                section.push('\n');
            }
            section
        })
        .collect();
    // Saturation noise: N prior-session packs (factless renderings).
    let mut noise = String::new();
    for i in 0..saturation {
        let _ = writeln!(noise, "# prior session {i}\n{body}");
    }
    let current = format!("# current session pack\n{body}");
    match position {
        Position::Begin => format!("{fact_line}{noise}{current}"),
        Position::Middle => {
            // At least one noise pack precedes the fact (div_ceil), so Middle
            // never degenerates into Begin at low saturation.
            let half = saturation.div_ceil(2);
            let mut head = String::new();
            for i in 0..half {
                let _ = writeln!(head, "# prior session {i}\n{body}");
            }
            let mut tail = String::new();
            for i in half..saturation {
                let _ = writeln!(tail, "# prior session {i}\n{body}");
            }
            format!("{head}{fact_line}{tail}{current}")
        }
        Position::End => format!("{noise}{current}{fact_line}"),
        Position::Duplicated => format!("{fact_line}{noise}{current}{fact_line}"),
    }
}

/// The needle question for a trial (query-aware carries the task + focus;
/// bare asks cold — the W5 evidence axis).
#[must_use]
pub fn trial_question(scenario: &Scenario, query_aware: bool) -> String {
    if query_aware {
        format!(
            "Task: locate the decisive fact about the retry policy. Focus files: {}. \
             Using ONLY the context above, what does the decisive fact say?",
            scenario
                .files
                .first()
                .map(|f| f.path.clone())
                .unwrap_or_default()
        )
    } else {
        "Using ONLY the context above, what does the decisive fact say?".to_string()
    }
}

/// The M6-08 oracle: normalized (case-insensitive, whitespace-collapsed)
/// contains-match on the decisive-fact token.
#[must_use]
pub fn oracle_success(response: &str, decisive_fact: &str) -> bool {
    let norm = |s: &str| {
        s.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    };
    norm(response).contains(&norm(decisive_fact))
}

/// `tokens` × `usd_per_mtok` in integer micro-USD (rounded).
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
fn micro_cost(tokens: u64, usd_per_mtok: f64) -> u64 {
    ((tokens as f64) * usd_per_mtok).round() as u64
}

/// Micro-USD → USD for display.
#[allow(clippy::cast_precision_loss)]
fn to_usd(micros: u64) -> f64 {
    (micros as f64) / 1_000_000.0
}

/// The sweep outcome: every recorded trial, the spend, and whether the cap
/// aborted the run (partial results are salvaged, never discarded).
#[derive(Clone, Debug, PartialEq)]
pub struct SweepOutcome {
    /// Every completed trial, in deterministic grid order.
    pub trials: Vec<TrialRecord>,
    /// The dedup'd contexts, content-addressed (the `packs.jsonl` map).
    pub packs: BTreeMap<String, String>,
    /// Total spend at the pinned prices, micro-USD.
    pub spent_micro_usd: u64,
    /// `Some(reason)` when the run aborted (spend cap / repeated API error).
    pub aborted: Option<String>,
}

/// Run the sweep grid through `transport` (M6-08 §3): for each model ×
/// position × saturation × query-shape × trial, build the needle context,
/// ask, judge, record. The spend cap hard-aborts BEFORE the call that would
/// exceed it (salvaging completed trials); an API error retries once, then
/// aborts with salvage.
pub fn run_sweep(config: &SweepConfig, transport: &mut dyn ModelTransport) -> SweepOutcome {
    let corpus = deterministic_corpus();
    // One scenario anchors the needle trials; saturation supplies the scale.
    let Some(scenario) = corpus.first() else {
        return SweepOutcome {
            trials: Vec::new(),
            packs: BTreeMap::new(),
            spent_micro_usd: 0,
            aborted: Some("empty corpus".into()),
        };
    };
    let mut out = SweepOutcome {
        trials: Vec::new(),
        packs: BTreeMap::new(),
        spent_micro_usd: 0,
        aborted: None,
    };
    for model in &config.models {
        for position in Position::ALL {
            for &saturation in &config.saturations {
                for query_aware in [true, false] {
                    for trial in 0..config.trials_per_cell {
                        let context = trial_context(scenario, position, saturation);
                        let question = trial_question(scenario, query_aware);
                        let prompt = format!("{context}\n\n{question}");
                        let pack_sha256 = sha256_hex(context.as_bytes());
                        // Estimate the call's cost ceiling from the context
                        // size; abort BEFORE spending past the cap.
                        let est_in = u64::from(rift_tokens::count(&prompt));
                        let est_micros = micro_cost(est_in, model.price_in_per_mtok)
                            .saturating_add(micro_cost(500, model.price_out_per_mtok));
                        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                        let cap_micros = (config.spend_cap_usd * 1_000_000.0).round() as u64;
                        if out.spent_micro_usd.saturating_add(est_micros) > cap_micros {
                            out.aborted = Some(format!(
                                "spend cap ${:.2} reached at ${:.2} (salvaged {} trials)",
                                config.spend_cap_usd,
                                to_usd(out.spent_micro_usd),
                                out.trials.len()
                            ));
                            return out;
                        }
                        // One retry on an API error, then abort-with-salvage.
                        let reply = match transport.complete(model, &prompt, config.temperature) {
                            Ok(r) => r,
                            Err(_first) => {
                                match transport.complete(model, &prompt, config.temperature) {
                                    Ok(r) => r,
                                    Err(second) => {
                                        out.aborted = Some(format!(
                                            "api error after retry on {}: {second} (salvaged {} trials)",
                                            model.id,
                                            out.trials.len()
                                        ));
                                        return out;
                                    }
                                }
                            }
                        };
                        let cost_micro_usd = micro_cost(reply.tokens_in, model.price_in_per_mtok)
                            .saturating_add(micro_cost(reply.tokens_out, model.price_out_per_mtok));
                        out.spent_micro_usd = out.spent_micro_usd.saturating_add(cost_micro_usd);
                        out.packs.entry(pack_sha256.clone()).or_insert(context);
                        out.trials.push(TrialRecord {
                            cell: TrialCell {
                                model: model.id.clone(),
                                position,
                                saturation,
                                query_aware,
                                trial,
                            },
                            pack_sha256,
                            question,
                            success: oracle_success(&reply.text, &scenario.decisive_fact),
                            response: reply.text,
                            tokens_in: reply.tokens_in,
                            tokens_out: reply.tokens_out,
                            cost_micro_usd,
                        });
                    }
                }
            }
        }
    }
    out
}

/// Serialize the dedup'd fixtures (M6-08 §4): `packs.jsonl` (one line per
/// distinct context, content-addressed) + `trials.jsonl` (one line per
/// trial). Deterministic order; offline analysis replays from these alone.
#[must_use]
pub fn render_fixtures(outcome: &SweepOutcome) -> (String, String) {
    let packs = outcome
        .packs
        .iter()
        .filter_map(|(sha, text)| {
            serde_json::to_string(&serde_json::json!({ "sha256": sha, "text": text })).ok()
        })
        .fold(String::new(), |mut acc, l| {
            acc.push_str(&l);
            acc.push('\n');
            acc
        });
    let trials = outcome
        .trials
        .iter()
        .filter_map(|t| serde_json::to_string(t).ok())
        .fold(String::new(), |mut acc, l| {
            acc.push_str(&l);
            acc.push('\n');
            acc
        });
    (packs, trials)
}

/// Parse `trials.jsonl` back (the offline-replay entry for derivation).
#[must_use]
pub fn parse_trials(jsonl: &str) -> Vec<TrialRecord> {
    jsonl
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// Scan recorded fixtures for forbidden spans (the injection-corpus posture:
/// recordings carry golden-repo content only, never secrets). Returns every
/// `(span, where)` hit; empty = clean.
#[must_use]
pub fn scan_recordings(packs_jsonl: &str, trials_jsonl: &str, forbidden: &[&str]) -> Vec<String> {
    let mut hits = Vec::new();
    for span in forbidden {
        if packs_jsonl.contains(span) {
            hits.push(format!("{span} in packs.jsonl"));
        }
        if trials_jsonl.contains(span) {
            hits.push(format!("{span} in trials.jsonl"));
        }
    }
    hits
}

#[cfg(feature = "real-model")]
pub use real::UreqTransport;

#[cfg(feature = "real-model")]
mod real {
    //! The paid transport (feature-gated; owner-invoked via
    //! `rift-memd eval --real-model` — never reachable from cargo test).

    use super::{ModelSpec, ModelTransport, TransportReply};

    /// Read a `usage.<field>` counter from a provider reply (0 when absent).
    fn usage_u64(reply: &serde_json::Value, field: &str) -> u64 {
        reply
            .get("usage")
            .and_then(|u| u.get(field))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
    }

    /// The thin `ureq`-level client over both families' HTTP APIs (ADR-0043:
    /// no vendor SDKs — two APIs, one trait). Keys are read from the
    /// environment AT CALL TIME (`ANTHROPIC_API_KEY` / `OPENAI_API_KEY`) and
    /// never stored on the struct, persisted, or logged.
    pub struct UreqTransport;

    impl ModelTransport for UreqTransport {
        fn complete(
            &mut self,
            model: &ModelSpec,
            prompt: &str,
            temperature: f64,
        ) -> Result<TransportReply, String> {
            match model.family.as_str() {
                "anthropic" => {
                    let key = std::env::var("ANTHROPIC_API_KEY")
                        .map_err(|_| "ANTHROPIC_API_KEY not set".to_string())?;
                    let body = serde_json::json!({
                        "model": model.id,
                        "max_tokens": 500,
                        "temperature": temperature,
                        "messages": [{ "role": "user", "content": prompt }],
                    });
                    let reply: serde_json::Value =
                        ureq::post("https://api.anthropic.com/v1/messages")
                            .header("x-api-key", &key)
                            .header("anthropic-version", "2023-06-01")
                            .send_json(&body)
                            .map_err(|e| format!("anthropic api: {e}"))?
                            .body_mut()
                            .read_json()
                            .map_err(|e| format!("anthropic decode: {e}"))?;
                    Ok(TransportReply {
                        text: reply
                            .get("content")
                            .and_then(|c| c.get(0))
                            .and_then(|c| c.get("text"))
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        tokens_in: usage_u64(&reply, "input_tokens"),
                        tokens_out: usage_u64(&reply, "output_tokens"),
                    })
                }
                "openai" => {
                    let key = std::env::var("OPENAI_API_KEY")
                        .map_err(|_| "OPENAI_API_KEY not set".to_string())?;
                    let body = serde_json::json!({
                        "model": model.id,
                        "max_tokens": 500,
                        "temperature": temperature,
                        "messages": [{ "role": "user", "content": prompt }],
                    });
                    let reply: serde_json::Value =
                        ureq::post("https://api.openai.com/v1/chat/completions")
                            .header("authorization", &format!("Bearer {key}"))
                            .send_json(&body)
                            .map_err(|e| format!("openai api: {e}"))?
                            .body_mut()
                            .read_json()
                            .map_err(|e| format!("openai decode: {e}"))?;
                    Ok(TransportReply {
                        text: reply
                            .get("choices")
                            .and_then(|c| c.get(0))
                            .and_then(|c| c.get("message"))
                            .and_then(|m| m.get("content"))
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        tokens_in: usage_u64(&reply, "prompt_tokens"),
                        tokens_out: usage_u64(&reply, "completion_tokens"),
                    })
                }
                other => Err(format!("unknown model family: {other}")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scripted mock: position-sensitive success (finds the fact only at
    /// Begin/Duplicated), fixed usage, optional failure injection.
    struct MockTransport {
        calls: u32,
        fail_on_call: Option<u32>,
        fail_twice: bool,
    }

    impl MockTransport {
        fn new() -> Self {
            Self {
                calls: 0,
                fail_on_call: None,
                fail_twice: false,
            }
        }
    }

    impl ModelTransport for MockTransport {
        fn complete(
            &mut self,
            _model: &ModelSpec,
            prompt: &str,
            _temperature: f64,
        ) -> Result<TransportReply, String> {
            self.calls += 1;
            if let Some(n) = self.fail_on_call {
                if self.calls == n || (self.fail_twice && self.calls == n + 1) {
                    return Err("simulated api error".into());
                }
            }
            // Position-sensitive: the mock "finds" the fact iff it appears in
            // the first or last 200 chars of the context (edge attention).
            let head = &prompt[..prompt.len().min(200)];
            let tail = &prompt[prompt.len().saturating_sub(400)..];
            let found = head.contains("THE_DECISIVE_FACT") || tail.contains("THE_DECISIVE_FACT");
            Ok(TransportReply {
                text: if found {
                    "THE_DECISIVE_FACT: retry policy moved to backoff.rs".into()
                } else {
                    "I could not find a decisive fact in the context.".into()
                },
                // Realistic usage: providers report ≈ the prompt's size, which
                // keeps the pre-call estimate proportional to the actual spend.
                tokens_in: u64::from(rift_tokens::count(prompt)),
                tokens_out: 20,
            })
        }
    }

    fn tiny_config() -> SweepConfig {
        SweepConfig {
            models: vec![ModelSpec {
                family: "anthropic".into(),
                id: "mock-model".into(),
                price_in_per_mtok: 3.0,
                price_out_per_mtok: 15.0,
            }],
            saturations: vec![1, 2],
            trials_per_cell: 1,
            ..SweepConfig::v1()
        }
    }

    #[test]
    fn the_grid_runs_and_positions_differentiate() {
        let cfg = tiny_config();
        let mut t = MockTransport::new();
        let out = run_sweep(&cfg, &mut t);
        assert!(out.aborted.is_none());
        // 1 model × 4 positions × 2 saturations × 2 query shapes × 1 trial.
        assert_eq!(out.trials.len(), 16);
        let rate = |p: Position| {
            let hits = out
                .trials
                .iter()
                .filter(|t| t.cell.position == p && t.success)
                .count();
            let n = out.trials.iter().filter(|t| t.cell.position == p).count();
            (hits, n)
        };
        // The edge-attention mock: Duplicated always succeeds; Middle never.
        assert_eq!(rate(Position::Duplicated).0, rate(Position::Duplicated).1);
        assert_eq!(rate(Position::Middle).0, 0, "buried mid-context is lost");
    }

    #[test]
    fn spend_cap_hard_aborts_with_salvage() {
        // Calibrate: a full run under an unlimited cap prices the grid…
        let cfg = tiny_config();
        let mut t = MockTransport::new();
        let full = run_sweep(&cfg, &mut t);
        assert!(full.aborted.is_none());
        // …then HALF that spend must abort mid-grid, salvaging the completed
        // prefix and never spending past the cap (the pre-call estimate is
        // conservative, so the abort lands before the breaching call).
        let mut capped = tiny_config();
        capped.spend_cap_usd = to_usd(full.spent_micro_usd) / 2.0;
        let mut t2 = MockTransport::new();
        let out = run_sweep(&capped, &mut t2);
        let reason = out.aborted.expect("the cap must abort");
        assert!(reason.contains("spend cap"), "{reason}");
        assert!(!out.trials.is_empty(), "completed trials are salvaged");
        assert!(out.trials.len() < full.trials.len());
        assert!(
            to_usd(out.spent_micro_usd) <= capped.spend_cap_usd,
            "never spends past the cap"
        );
    }

    #[test]
    fn api_errors_retry_once_then_abort_with_salvage() {
        // A single transient failure: the retry recovers, the sweep completes.
        let cfg = tiny_config();
        let mut flaky = MockTransport::new();
        flaky.fail_on_call = Some(3);
        let out = run_sweep(&cfg, &mut flaky);
        assert!(out.aborted.is_none(), "one retry absorbs a transient error");
        assert_eq!(out.trials.len(), 16);

        // A double failure: abort, salvaging what completed.
        let mut dead = MockTransport::new();
        dead.fail_on_call = Some(3);
        dead.fail_twice = true;
        let out = run_sweep(&cfg, &mut dead);
        let reason = out.aborted.expect("a repeated error aborts");
        assert!(reason.contains("api error after retry"), "{reason}");
        assert_eq!(out.trials.len(), 2, "trials before the failure survive");
    }

    #[test]
    fn fixtures_are_dedup_d_and_replay_offline() {
        let cfg = tiny_config();
        let mut t = MockTransport::new();
        let out = run_sweep(&cfg, &mut t);
        let (packs, trials) = render_fixtures(&out);
        // Dedup: 4 positions × 2 saturations distinct contexts, NOT 16.
        assert_eq!(out.packs.len(), 8);
        assert_eq!(packs.lines().count(), 8);
        assert_eq!(trials.lines().count(), 16);
        // Offline replay: parse back and the records round-trip.
        let parsed = parse_trials(&trials);
        assert_eq!(parsed, out.trials);
        // The recordings scan clean against a forbidden-span list.
        assert!(scan_recordings(&packs, &trials, &["SECRET_KEY", "api_key="]).is_empty());
        assert_eq!(
            scan_recordings("has SECRET_KEY inside", &trials, &["SECRET_KEY"]).len(),
            1
        );
    }

    #[test]
    fn the_oracle_is_normalized_contains() {
        assert!(oracle_success(
            "the answer:  the_decisive_fact:   retry policy moved to backoff.rs.",
            "THE_DECISIVE_FACT: retry policy moved to backoff.rs"
        ));
        assert!(!oracle_success("no idea", "THE_DECISIVE_FACT"));
    }

    #[test]
    fn the_position_knob_moves_the_fact_not_the_spine() {
        let s = &deterministic_corpus()[0];
        for (pos, pred) in [(Position::Begin, 0usize), (Position::End, usize::MAX)] {
            let ctx = trial_context(s, pos, 2);
            let at = ctx.find("THE_DECISIVE_FACT").unwrap();
            if pred == 0 {
                assert_eq!(at, 0, "Begin leads the context");
            } else {
                assert!(at > ctx.len() / 2, "End trails the context");
            }
        }
        let dup = trial_context(s, Position::Duplicated, 2);
        assert_eq!(dup.matches("THE_DECISIVE_FACT").count(), 2, "both edges");
        // The spine (file-section order) is identical across positions.
        let strip = |c: &str| {
            c.lines()
                .filter(|l| l.starts_with("## "))
                .map(String::from)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            strip(&trial_context(s, Position::Begin, 2)),
            strip(&trial_context(s, Position::Middle, 2)),
            "the knob never reorders the spine"
        );
    }

    #[test]
    fn v1_config_matches_the_m6_08_protocol() {
        let c = SweepConfig::v1();
        assert_eq!(c.version, 1);
        assert_eq!(c.models.len(), 6, "full ladder, both families");
        assert_eq!(c.trials_per_cell, 3);
        assert!((c.spend_cap_usd - 50.0).abs() < f64::EPSILON);
        assert!((c.temperature - 0.0).abs() < f64::EPSILON);
        assert_eq!(c.saturations, vec![1, 2, 5, 10]);
        // Round-trips as config data (a new model is a config edit).
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(serde_json::from_str::<SweepConfig>(&json).unwrap(), c);
    }
}
