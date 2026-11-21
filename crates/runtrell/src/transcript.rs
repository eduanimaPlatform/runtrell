// SPDX-License-Identifier: Apache-2.0
//! Shared control-store rules for successful transcript purge.
use crate::*;
use serde_json::{Value, json};

impl RunSpec {
    #[doc(hidden)]
    pub fn clear_transcript(&mut self) {
        self.input.clear();
        self.images.clear();
        self.continuation.instruction.clear();
        self.completion.output_schema = None;
    }
}

impl RunRecord {
    pub fn transcript_purged(&self) -> bool {
        self.purge_marker().is_some()
    }
    fn purge_marker(&self) -> Option<&Value> {
        if self.state != RunState::Terminal(Terminal::Succeeded)
            || self.spec.transcript != TranscriptPolicy::PurgeOnSuccess
        {
            return None;
        }
        self.checkpoint.as_ref()?.payload.get("transcript_purge")
    }
    #[doc(hidden)]
    pub fn matches_purged_request(&self, spec: &RunSpec) -> bool {
        self.purge_marker()
            .is_some_and(|marker| marker["request"] == digest(&json!(spec)))
    }
    #[doc(hidden)]
    pub fn matches_completion(&self, completion: &Completion) -> bool {
        self.completion.as_ref() == Some(&completion.id)
            && if let Some(marker) = self.purge_marker() {
                marker["completion"] == digest(&json!(completion))
            } else {
                self.output.as_ref() == Some(&completion.output)
                    && self.receipt.as_ref() == Some(&completion.receipt)
            }
    }
    #[doc(hidden)]
    pub fn confirm_completion(
        &mut self,
        completion: Completion,
        request_digest: Option<String>,
        now: u64,
    ) {
        if let Some(request) = request_digest {
            self.spec.clear_transcript();
            self.checkpoint = Some(Checkpoint {
                schema: 1,
                revision: self.revision,
                cursor: self.checkpoint.as_ref().map_or(0, |cp| cp.cursor),
                messages: vec![],
                payload: json!({"transcript_purge":{
                    "request":request,
                    "completion":digest(&json!(completion)),
                    "at":now,
                }}),
            });
        } else {
            self.output = Some(completion.output);
            self.receipt = Some(completion.receipt);
        }
        self.state = RunState::Terminal(Terminal::Succeeded);
        self.completion = Some(completion.id);
        self.revision += 1;
    }
}

impl Operation {
    #[doc(hidden)]
    pub fn transcript_purge_ready(&self) -> bool {
        self.settled
            && !self.unknown_usage
            && matches!(self.state, OperationState::Applied | OperationState::Failed)
    }

    #[doc(hidden)]
    pub fn clear_transcript(&mut self) {
        // A child admission stores only the fingerprint needed for idempotence.
        if self.request.kind != OperationKind::ChildRun {
            self.request.input = Value::Null;
        }
        self.request.recovery_proof = None;
        if let Some(result) = &mut self.result {
            result.clear_transcript(self.request.kind == OperationKind::ChildRun);
        }
        for usage in &mut self.usage_observations {
            usage.clear_transcript();
        }
        if let Some(recovery) = &mut self.recovery {
            recovery.previous.clear_transcript();
            recovery.settled.clear_transcript();
        }
        self.unsettled_result = None;
    }
}

impl ExternalResult {
    #[doc(hidden)]
    pub fn clear_transcript(&mut self, keep_child_identity: bool) {
        match self {
            Self::Applied {
                physical_evidence,
                output,
                usage,
                ..
            } => {
                *physical_evidence = None;
                if !keep_child_identity {
                    *output = Value::Null;
                }
                usage.clear_transcript();
            }
            Self::Failed {
                physical_evidence,
                usage,
                code,
                repair_hint,
                provider_diagnostic,
                ..
            } => {
                *physical_evidence = None;
                code.clear();
                *repair_hint = None;
                *provider_diagnostic = None;
                usage.clear_transcript();
            }
            Self::UnknownDiagnostic(_) => *self = Self::Unknown,
            _ => {}
        }
    }
}

impl Usage {
    #[doc(hidden)]
    pub fn clear_transcript(&mut self) {
        if let Some(svg) = &mut self.svg_usage {
            let reported = &svg.reported;
            svg.reported = if reported.is_object() {
                let mut clean = serde_json::Map::new();
                clean.insert(
                    "model".into(),
                    match (&svg.model, reported.get("model")) {
                        (Some(model), Some(Value::String(actual))) if actual == model => {
                            json!(model)
                        }
                        _ => Value::Bool(false),
                    },
                );
                for field in [
                    "input_tokens",
                    "output_tokens",
                    "reasoning_tokens",
                    "total_tokens",
                ] {
                    let value = match reported.get(field) {
                        None | Some(Value::Null) => Value::Null,
                        Some(value) if value.as_u64().is_some() => value.clone(),
                        _ => Value::Bool(false),
                    };
                    clean.insert(field.into(), value);
                }
                Value::Object(clean)
            } else {
                Value::Null
            };
        }
    }
}
