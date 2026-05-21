use std::convert::Infallible;

use crate::proposed_event::ProposedEvent;
use crate::provider::{AgentBridge, SignalAnalysisInput};

/// Deterministic stub adapter — extracts a handful of tags by keyword and
/// echoes a moderate-confidence proposed signal. Useful for tests and for
/// proving the boundary without committing to any LLM provider.
pub struct FakeAgent;

impl AgentBridge for FakeAgent {
    type Error = Infallible;

    fn analyze_signal(&self, input: SignalAnalysisInput) -> Result<Vec<ProposedEvent>, Infallible> {
        let lower = input.text.to_lowercase();
        let mut tags: Vec<String> = ["ai", "voice", "agent", "finance", "health"]
            .iter()
            .filter(|kw| lower.contains(*kw))
            .map(|s| (*s).to_string())
            .collect();
        for hint in input.hints {
            if !tags.contains(&hint) {
                tags.push(hint);
            }
        }
        Ok(vec![ProposedEvent {
            source: "fake-agent".into(),
            summary: input.text,
            tags,
            confidence: 0.6,
            rationale: "stub adapter: keyword-based tag extraction".into(),
        }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_agent_extracts_keyword_tags() {
        let agent = FakeAgent;
        let out = agent
            .analyze_signal(SignalAnalysisInput {
                text: "Realtime voice models from AI labs".into(),
                hints: vec!["realtime".into()],
            })
            .unwrap();
        assert_eq!(out.len(), 1);
        let ev = &out[0];
        assert!(ev.tags.contains(&"ai".into()));
        assert!(ev.tags.contains(&"voice".into()));
        assert!(ev.tags.contains(&"realtime".into()));
    }
}
