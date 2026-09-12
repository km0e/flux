//! Machine table tests — the reducer is the source of truth.
//!
//! Every (state, input) pair is exercised through the fact trace it
//! produces. The loop's own tests (runtime.rs) cover the pump; the tool
//! executor's mechanics live with the executor (flux-chat).

use super::*;
use flux_core::LoopFact as Fact;
use flux_core::LoopInput as Input;
use flux_core::RoundOutcome;
use flux_core::{ErrorCode, StreamChunk, ToolCall, WireEvent};

fn m() -> Machine {
    Machine::new()
}

fn call(id: &str, name: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: "{}".into(),
    }
}

fn stream_end(finish_reason: Option<&str>) -> Fact {
    Fact::Wire(WireEvent::StreamEnd {
        finish_reason: finish_reason.map(str::to_string),
    })
}

fn round_completed(finish_reason: Option<&str>) -> Fact {
    Fact::RoundEnded(RoundOutcome::Completed {
        finish_reason: finish_reason.map(str::to_string),
    })
}

fn chunk(c: StreamChunk) -> Input {
    Input::Stream(StreamEvent::Chunk(c))
}

#[test]
fn idle_user_message_starts_stream() {
    let mut m = m();
    let step = m.step(Input::UserMessage("hi".into()));
    assert_eq!(
        step.facts,
        vec![
            Fact::TranscriptCommitted(vec![Message::user("hi")]),
            Fact::ModelInputRequested(vec![Message::user("hi")]),
        ]
    );
    assert!(matches!(m.state(), &State::Streaming { .. }));
    assert_eq!(m.state_kind(), ChatStateKind::Streaming);
}

#[test]
fn streaming_text_delta_accumulates_and_emits() {
    let mut m = m();
    start_streaming(&mut m);
    let step = m.step(chunk(StreamChunk::Text("hel".into())));
    assert_eq!(
        step.facts,
        vec![Fact::Wire(WireEvent::TextDelta("hel".into()))]
    );
    let step = m.step(chunk(StreamChunk::Text("lo".into())));
    assert_eq!(
        step.facts,
        vec![Fact::Wire(WireEvent::TextDelta("lo".into()))]
    );
    let State::Streaming { output } = m.state() else {
        panic!("still streaming");
    };
    assert_eq!(output.text, "hello");
}

fn start_streaming(m: &mut Machine) {
    m.step(Input::UserMessage("hi".into()));
}

#[test]
fn tool_call_preview_is_pure_passthrough_wire_signaling() {
    let mut m = m();
    start_streaming(&mut m);
    // Identity event → exactly one Wire fact; NO state change (the preview
    // is forward signaling — the ToolCalls batch at End stays the sole
    // dispatch source).
    let step = m.step(chunk(StreamChunk::ToolCallPreview {
        id: "call_1".into(),
        name: Some("bash".into()),
        args_delta: Some(r#"{"cmd":"e""#.into()),
    }));
    assert_eq!(
        step.facts,
        vec![Fact::Wire(WireEvent::ToolCallPreview {
            id: "call_1".into(),
            name: Some("bash".into()),
            args_delta: Some(r#"{"cmd":"e""#.into()),
        })]
    );
    // Arg-delta event → same passthrough.
    let step = m.step(chunk(StreamChunk::ToolCallPreview {
        id: "call_1".into(),
        name: None,
        args_delta: Some(r#"cho hi"}"#.into()),
    }));
    assert_eq!(
        step.facts,
        vec![Fact::Wire(WireEvent::ToolCallPreview {
            id: "call_1".into(),
            name: None,
            args_delta: Some(r#"cho hi"}"#.into()),
        })]
    );
    // The accumulated output is untouched by previews.
    let State::Streaming { output } = m.state() else {
        panic!("still streaming");
    };
    assert_eq!(output.text, "");
    assert!(output.tool_calls.is_empty());
    // A stale preview arriving at Idle is absorbed without facts.
    let mut m2 = Machine::new();
    let step = m2.step(chunk(StreamChunk::ToolCallPreview {
        id: "x".into(),
        name: Some("t".into()),
        args_delta: None,
    }));
    assert!(step.facts.is_empty());
}

#[test]
fn streaming_reasoning_and_usage() {
    let mut m = m();
    start_streaming(&mut m);
    let step = m.step(chunk(StreamChunk::Reasoning("think".into())));
    assert_eq!(
        step.facts,
        vec![Fact::Wire(WireEvent::ReasoningDelta("think".into()))]
    );
    let step = m.step(chunk(StreamChunk::Usage {
        prompt_tokens: 10,
        completion_tokens: 2,
        cached_tokens: 4,
    }));
    assert_eq!(
        step.facts,
        vec![Fact::Wire(WireEvent::Usage {
            prompt_tokens: 10,
            completion_tokens: 2,
            cached_tokens: 4,
        })]
    );
    let State::Streaming { output } = m.state() else {
        panic!("still streaming");
    };
    assert_eq!(output.reasoning.as_deref(), Some("think"));
}

#[test]
fn streaming_end_without_tools_ends_round() {
    let mut m = m();
    start_streaming(&mut m);
    let step = m.step(chunk(StreamChunk::Text("answer".into())));
    assert_eq!(step.facts.len(), 1);
    // End: transcript fact (the assistant reply — the user message was
    // committed at round start) precedes the StreamEnd wire event
    // (persist-before-announce), and the stream handle drops.
    let step = m.step(chunk(StreamChunk::End {
        finish_reason: None,
    }));
    assert_eq!(step.facts.len(), 3);
    assert!(matches!(&step.facts[0], Fact::TranscriptCommitted(msgs) if msgs.len() == 1));
    assert_eq!(step.facts[1], stream_end(None));
    assert_eq!(step.facts[2], round_completed(None));
    assert_eq!(m.state(), &State::Idle);
    assert_eq!(m.state_kind(), ChatStateKind::Idle);
}

#[test]
fn streaming_end_carries_finish_reason_into_stream_end() {
    let mut m = m();
    start_streaming(&mut m);
    let step = m.step(chunk(StreamChunk::End {
        finish_reason: Some("length".into()),
    }));
    assert_eq!(step.facts[1], stream_end(Some("length")));
}

#[test]
fn streaming_end_with_tools_dispatches_first_tool() {
    let mut m = m();
    start_streaming(&mut m);
    m.step(chunk(StreamChunk::ToolCalls(vec![call("c1", "t")])));
    let step = m.step(chunk(StreamChunk::End {
        finish_reason: None,
    }));
    // The tool_calls assistant message lands in the transcript buffer
    // (persisted at round end), the batch dispatches one at a time.
    assert_eq!(
        step.facts,
        vec![Fact::ToolDispatched {
            call: call("c1", "t"),
            arguments: HashMap::new(),
        }]
    );
    let State::ProcessingTools { inflight, .. } = m.state() else {
        panic!("processing tools");
    };
    assert_eq!(inflight[&"c1".to_string()], "t");
}

#[test]
fn tool_round_final_stream_end_carries_follow_up_reason() {
    // A tool round's first stream End is absorbed silently (its reason is
    // dropped by design); the final StreamEnd carries the FOLLOW-UP
    // stream's own reason.
    let mut m = m();
    start_streaming(&mut m);
    m.step(chunk(StreamChunk::ToolCalls(vec![call("c1", "t")])));
    m.step(chunk(StreamChunk::End {
        finish_reason: None,
    }));
    let step = m.step(Input::ToolFinished {
        call: call("c1", "t"),
        result: "r".into(),
        ends_round: false,
    });
    // Tool result wire fact first, then the continuation stream request
    // over the dual-written tool result.
    assert_eq!(
        step.facts,
        vec![
            Fact::Wire(WireEvent::ToolResult {
                id: "c1".into(),
                name: "t".into(),
                result: "r".into(),
            }),
            Fact::ModelInputRequested(vec![Message::tool("c1", "r")]),
        ]
    );
    let step = m.step(chunk(StreamChunk::End {
        finish_reason: Some("stop".into()),
    }));
    assert_eq!(step.facts[1], stream_end(Some("stop")));
}

#[test]
fn streaming_error_persists_partial_and_emits() {
    let mut m = m();
    start_streaming(&mut m);
    m.step(chunk(StreamChunk::Text("partial".into())));
    let step = m.step(Input::Stream(StreamEvent::Failed {
        message: "boom".into(),
        code: Some(ErrorCode::ProviderConnection),
    }));
    assert_eq!(
        step.facts,
        vec![
            Fact::Wire(WireEvent::StreamError {
                message: "boom".into(),
                code: Some(ErrorCode::ProviderConnection),
            }),
            Fact::TranscriptCommitted(vec![Message {
                role: Role::Assistant,
                content: "partial".into(),
                reasoning_content: None,
                tool_calls: Vec::new(),
                tool_call_id: None,
            }]),
            stream_end(None),
            Fact::RoundEnded(RoundOutcome::Failed {
                message: "boom".into(),
                code: Some(ErrorCode::ProviderConnection),
            }),
        ]
    );
    assert_eq!(m.state(), &State::Idle);
}

#[test]
fn streaming_cancel_drops_handle_and_emits_cancelled() {
    let mut m = m();
    start_streaming(&mut m);
    // The chat layer opened the connection; the handle rides in.
    let (handle, token) = StreamHandle::new();
    m.step(Input::StreamHandle(handle));
    assert!(!token.is_cancelled());
    m.step(chunk(StreamChunk::Text("partial".into())));
    let step = m.step(Input::Cancel);
    // The handle dropped → the connection's push stops immediately.
    assert!(token.is_cancelled());
    assert_eq!(step.facts[0], Fact::Wire(WireEvent::Cancelled));
    assert!(matches!(&step.facts[1], Fact::TranscriptCommitted(msgs) if msgs.len() == 1));
    assert_eq!(step.facts[2], stream_end(None));
    assert_eq!(step.facts[3], Fact::RoundEnded(RoundOutcome::Cancelled));
    assert_eq!(m.state(), &State::Idle);
}

#[test]
fn handle_arriving_at_idle_is_dropped_on_sight() {
    let mut m = m();
    let (handle, token) = StreamHandle::new();
    m.step(Input::StreamHandle(handle));
    assert!(token.is_cancelled(), "stale handle drops on arrival");
    assert_eq!(m.state(), &State::Idle);
}

fn enter_processing(m: &mut Machine, calls: Vec<ToolCall>) {
    m.step(Input::UserMessage("go".into()));
    m.step(chunk(StreamChunk::ToolCalls(calls)));
    m.step(chunk(StreamChunk::End {
        finish_reason: None,
    }));
}

#[test]
fn processing_batch_dispatches_one_at_a_time() {
    let mut m = m();
    enter_processing(&mut m, vec![call("c1", "t"), call("c2", "t")]);
    let step = m.step(Input::ToolFinished {
        call: call("c1", "t"),
        result: "done".into(),
        ends_round: false,
    });
    // Result wire fact, then the SECOND tool dispatches (not the
    // continuation stream — the batch is not done).
    assert_eq!(
        step.facts,
        vec![
            Fact::Wire(WireEvent::ToolResult {
                id: "c1".into(),
                name: "t".into(),
                result: "done".into(),
            }),
            Fact::ToolDispatched {
                call: call("c2", "t"),
                arguments: HashMap::new(),
            },
        ]
    );
    // Last tool result → continuation stream over every dual-written result.
    let step = m.step(Input::ToolFinished {
        call: call("c2", "t"),
        result: "done2".into(),
        ends_round: false,
    });
    assert!(matches!(
        &step.facts[1],
        Fact::ModelInputRequested(pending) if pending.len() == 2
    ));
}

#[test]
fn processing_tool_executed_dual_writes_and_wraps_up() {
    let mut m = m();
    enter_processing(&mut m, vec![call("c1", "t")]);
    m.step(Input::ToolFinished {
        call: call("c1", "t"),
        result: "42".into(),
        ends_round: false,
    });
    let step = m.step(chunk(StreamChunk::End {
        finish_reason: None,
    }));
    // Wrap-up: transcript (user + assistant tool_calls + tool result) is
    // persisted before the StreamEnd.
    let Fact::TranscriptCommitted(msgs) = &step.facts[0] else {
        panic!("transcript fact");
    };
    // user persisted at round start; transcript = assistant(tool_calls) +
    // tool result + the continuation stream's (empty) assistant message.
    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[1], Message::tool("c1", "42"));
    assert_eq!(step.facts[1], stream_end(None));
    assert_eq!(m.state(), &State::Idle);
}

#[test]
fn processing_cancel_interrupts_in_flight_voids_rest_then_ends_round() {
    let mut m = m();
    enter_processing(
        &mut m,
        vec![call("inflight", "slow"), call("queued", "later")],
    );
    // First dispatch consumed "inflight"; "queued" sits in the exec queue.
    let step = m.step(Input::Cancel);
    // The interrupt goes to the executor; the queued tool is voided into
    // pending (deferred delivery); the round wraps when the in-flight
    // result lands.
    assert_eq!(step.facts, vec![Fact::InterruptTools]);
    let State::ProcessingTools {
        exec_queue,
        cancelled,
        ..
    } = m.state()
    else {
        panic!("still processing");
    };
    assert!(exec_queue.is_empty(), "remaining batch voided");
    assert!(*cancelled, "cancel absorbed into the flag");
    // The in-flight result resolves the round: Cancelled + wrap-up.
    let step = m.step(Input::ToolFinished {
        call: call("inflight", "slow"),
        result: "partial output".into(),
        ends_round: false,
    });
    assert_eq!(
        step.facts[0],
        Fact::Wire(WireEvent::ToolResult {
            id: "inflight".into(),
            name: "slow".into(),
            result: "partial output".into(),
        })
    );
    assert_eq!(step.facts[1], Fact::Wire(WireEvent::Cancelled));
    let Fact::TranscriptCommitted(msgs) = &step.facts[2] else {
        panic!("transcript fact");
    };
    // The void for the queued tool rides the transcript (its tool_call was
    // committed to the provider prefix and needs a matching result).
    assert!(msgs.contains(&Message::tool("queued", "cancelled by user")));
    assert_eq!(step.facts[3], stream_end(None));
}

#[test]
fn cancel_mid_batch_next_round_request_resolves_every_tool_call() {
    // The deferred voids ride `pending` into the NEXT round's request —
    // the provider prefix never keeps a tool_call without a result.
    let mut m = m();
    enter_processing(
        &mut m,
        vec![call("inflight", "slow"), call("queued", "later")],
    );
    m.step(Input::Cancel);
    m.step(Input::ToolFinished {
        call: call("inflight", "slow"),
        result: "partial".into(),
        ends_round: false,
    });
    // Idle now; pending = [void]. The next user message carries it.
    let step = m.step(Input::UserMessage("next".into()));
    let Fact::ModelInputRequested(pending) = &step.facts[1] else {
        panic!("model input requested");
    };
    assert_eq!(
        pending,
        &vec![
            Message::tool("queued", "cancelled by user"),
            Message::tool("inflight", "partial"),
            Message::user("next"),
        ]
    );
}

#[test]
fn processing_double_cancel_absorbs_once() {
    let mut m = m();
    enter_processing(&mut m, vec![call("c1", "t")]);
    let step = m.step(Input::Cancel);
    assert_eq!(step.facts, vec![Fact::InterruptTools]);
    // A second cancel is a no-op — the interrupt already went out.
    let step = m.step(Input::Cancel);
    assert!(step.facts.is_empty());
    let State::ProcessingTools { cancelled, .. } = m.state() else {
        panic!("still processing");
    };
    assert!(*cancelled);
}

#[test]
fn ends_round_tool_wraps_without_follow_up_stream() {
    let mut m = m();
    enter_processing(&mut m, vec![call("c1", "feature_done")]);
    let step = m.step(Input::ToolFinished {
        call: call("c1", "feature_done"),
        result: "orchestrated".into(),
        ends_round: true,
    });
    // Result wire fact, then wrap-up — NO continuation ModelInputRequested.
    assert_eq!(
        step.facts[0],
        Fact::Wire(WireEvent::ToolResult {
            id: "c1".into(),
            name: "feature_done".into(),
            result: "orchestrated".into(),
        })
    );
    assert!(matches!(&step.facts[1], Fact::TranscriptCommitted(_)));
    assert_eq!(step.facts[2], stream_end(None));
    assert_eq!(
        step.facts[3],
        Fact::RoundEnded(RoundOutcome::ToolEnded {
            call: call("c1", "feature_done"),
            result: "orchestrated".into(),
        })
    );
    assert_eq!(m.state(), &State::Idle);
}

#[test]
fn user_cancel_during_ends_round_tool_wins_no_wrap_interrupt() {
    // feature_done in flight is EXEMPT from user interruption — its result
    // is the next feature's opening context; a cancel is absorbed and the
    // exempt tool's completion concludes the round normally.
    let mut m = m();
    enter_processing(&mut m, vec![call("c1", "feature_done")]);
    let step = m.step(Input::Cancel);
    assert!(step.facts.is_empty(), "exempt: no interrupt fact");
    let State::ProcessingTools { cancelled, .. } = m.state() else {
        panic!("still processing");
    };
    assert!(*cancelled, "absorbed");
    // The completion still ends the round — with Cancelled (the user said
    // stop) — and the caller (chat layer) skips the rebase+inject on it.
    let step = m.step(Input::ToolFinished {
        call: call("c1", "feature_done"),
        result: "orchestrated".into(),
        ends_round: true,
    });
    assert_eq!(step.facts[1], Fact::Wire(WireEvent::Cancelled));
    assert_eq!(step.facts[3], stream_end(None));
    assert_eq!(step.facts[4], Fact::RoundEnded(RoundOutcome::Cancelled));
}

#[test]
fn feature_done_in_flight_is_exempt_from_interrupt() {
    // While OTHER tools run alongside, a cancel still interrupts them —
    // only the feature_done flight itself is exempt.
    let mut m = m();
    enter_processing(
        &mut m,
        vec![call("c1", "feature_done"), call("c2", "normal")],
    );
    let step = m.step(Input::Cancel);
    // c1 (feature_done) is IN FLIGHT (dispatched first) — exempt, so no
    // interrupt fact and no batch void (the exempt tool's completion
    // concludes the round; a second cancel is a no-op).
    assert!(step.facts.is_empty());
    let State::ProcessingTools { exec_queue, .. } = m.state() else {
        panic!("still processing");
    };
    assert!(exec_queue.is_empty());
}

// ── the control-plane gate (Hold / GateReleased) ───────────────────────

#[test]
fn hold_at_idle_fires_the_gate_immediately() {
    // At Idle the gate engages AND drains in the same step: nothing is
    // running, and everything sent before the decision precedes the Hold
    // in the FIFO — the rebuild flow can act at once.
    let mut m = m();
    let step = m.step(Input::Hold);
    assert_eq!(step.facts, vec![Fact::GateReleased]);
    assert_eq!(m.state(), &State::Idle);
    assert_eq!(m.state_kind(), ChatStateKind::Idle);
    // The flag is not sticky: a later Hold fires again (coalescing of
    // repeated rebuild intents lives in the consumer's gate flag).
    let step = m.step(Input::Hold);
    assert_eq!(step.facts, vec![Fact::GateReleased]);
}

#[test]
fn gate_fires_with_pending_voids_intact() {
    // Post-cancel pending residue (deferred void delivery) SURVIVES the
    // gate: the next round's request must still resolve every committed
    // tool_call.
    let mut m = m();
    enter_processing(&mut m, vec![call("c1", "t")]);
    m.step(Input::Cancel);
    m.step(Input::ToolFinished {
        call: call("c1", "t"),
        result: "partial".into(),
        ends_round: false,
    });
    assert_eq!(m.state(), &State::Idle);
    let step = m.step(Input::Hold);
    assert_eq!(step.facts, vec![Fact::GateReleased]);
    // Pending survived: the next request still carries the tool result.
    let step = m.step(Input::UserMessage("next".into()));
    let Fact::ModelInputRequested(pending) = &step.facts[1] else {
        panic!("model input requested");
    };
    assert_eq!(
        pending,
        &vec![Message::tool("c1", "partial"), Message::user("next")]
    );
}

#[test]
fn hold_mid_round_arms_the_gate_and_engages_at_wrap_up() {
    // A live round is never interrupted: the Hold arms, the round
    // finishes first, and the gate fires at its wrap-up.
    let mut m = m();
    start_streaming(&mut m);
    let step = m.step(Input::Hold);
    assert!(step.facts.is_empty(), "mid-round: only armed");
    assert!(matches!(m.state(), &State::Streaming { .. }));
    let step = m.step(Input::Stream(StreamEvent::Chunk(StreamChunk::End {
        finish_reason: None,
    })));
    // Wrap facts (transcript + StreamEnd) then the fired gate.
    assert_eq!(step.facts[1], stream_end(None));
    assert_eq!(*step.facts.last().unwrap(), Fact::GateReleased);
    assert_eq!(m.state(), &State::Idle);
}

#[test]
fn hold_while_processing_tools_arms_too() {
    let mut m = m();
    enter_processing(&mut m, vec![call("c1", "t")]);
    assert!(m.step(Input::Hold).facts.is_empty());
    // Batch done → continuation stream → its end wraps the round through
    // the armed gate.
    m.step(Input::ToolFinished {
        call: call("c1", "t"),
        result: "done".into(),
        ends_round: false,
    });
    assert!(matches!(m.state(), &State::Streaming { .. }));
    let step = m.step(Input::Stream(StreamEvent::Chunk(StreamChunk::End {
        finish_reason: None,
    })));
    assert_eq!(*step.facts.last().unwrap(), Fact::GateReleased);
    assert_eq!(m.state(), &State::Idle);
}

#[test]
fn hold_then_cancel_fires_the_gate_at_the_cancelled_wrap() {
    // The gate survives a user cancel: the round wraps with Cancelled and
    // the armed gate fires at that wrap (the rebuild still applies — the
    // truth sources changed regardless).
    let mut m = m();
    start_streaming(&mut m);
    m.step(Input::Hold);
    let step = m.step(Input::Cancel);
    assert!(step.facts.contains(&Fact::Wire(WireEvent::Cancelled)));
    assert_eq!(*step.facts.last().unwrap(), Fact::GateReleased);
    assert_eq!(m.state(), &State::Idle);
}

#[test]
fn stale_cancel_while_idle_is_a_noop() {
    let mut m = m();
    let step = m.step(Input::Cancel);
    assert!(step.facts.is_empty());
    assert_eq!(m.state(), &State::Idle);
}

#[test]
fn stale_tool_feedback_without_dispatch_is_absorbed() {
    let mut m = m();
    let step = m.step(Input::ToolFinished {
        call: call("ghost", "t"),
        result: "x".into(),
        ends_round: false,
    });
    assert!(step.facts.is_empty());
    assert_eq!(m.state(), &State::Idle);
}

// ── the machine turn queue (mid-round user turns) ───────────────────────────

fn assistant_empty() -> Message {
    Message {
        role: Role::Assistant,
        content: String::new(),
        reasoning_content: None,
        tool_calls: Vec::new(),
        tool_call_id: None,
    }
}

#[test]
fn streaming_user_message_queues_and_starts_at_wrap() {
    // A turn sent mid-stream is QUEUED, not dropped: it starts in the same
    // step that wraps the current round, with the boundary REPORTED
    // (RoundState(Idle) — the feature hook and snapshots read it; the
    // loop's own transition check cannot emit it because the step
    // re-enters Streaming).
    let mut m = m();
    start_streaming(&mut m);
    let step = m.step(Input::UserMessage("next".into()));
    assert!(step.facts.is_empty(), "mid-round: queued, not started");
    let step = m.step(chunk(StreamChunk::End {
        finish_reason: None,
    }));
    assert_eq!(
        step.facts,
        vec![
            Fact::TranscriptCommitted(vec![assistant_empty()]),
            stream_end(None),
            round_completed(None),
            Fact::RoundState(ChatStateKind::Idle),
            Fact::TranscriptCommitted(vec![Message::user("next")]),
            Fact::ModelInputRequested(vec![Message::user("next")]),
            Fact::RoundState(ChatStateKind::Streaming),
        ]
    );
    assert!(matches!(m.state(), &State::Streaming { .. }));
    assert_eq!(m.state_kind(), ChatStateKind::Streaming);
}

#[test]
fn queued_turns_run_in_order() {
    let mut m = m();
    start_streaming(&mut m);
    m.step(Input::UserMessage("first".into()));
    m.step(Input::UserMessage("second".into()));
    m.step(chunk(StreamChunk::End {
        finish_reason: None,
    }));
    // The first queued turn started; the second waits for ITS wrap.
    let step = m.step(chunk(StreamChunk::End {
        finish_reason: None,
    }));
    assert!(
        step.facts
            .contains(&Fact::ModelInputRequested(vec![Message::user("second")]))
    );
    assert!(matches!(m.state(), &State::Streaming { .. }));
    // The second turn's round wraps → the machine finally rests Idle.
    let step = m.step(chunk(StreamChunk::End {
        finish_reason: None,
    }));
    assert_eq!(step.facts[1], stream_end(None));
    assert_eq!(m.state(), &State::Idle);
}

#[test]
fn processing_user_message_queues_until_the_round_wraps() {
    // A turn sent during a tool flight waits for the ROUND (the batch may
    // continue into a follow-up stream first), not for the batch.
    let mut m = m();
    enter_processing(&mut m, vec![call("c1", "t")]);
    assert!(m.step(Input::UserMessage("next".into())).facts.is_empty());
    // Batch done → continuation stream: the turn is still queued.
    let step = m.step(Input::ToolFinished {
        call: call("c1", "t"),
        result: "done".into(),
        ends_round: false,
    });
    assert!(
        !step.facts.iter().any(
            |f| matches!(f, Fact::ModelInputRequested(p) if p == &vec![Message::user("next")])
        ),
        "the turn must not start before the round wraps"
    );
    // The continuation's end wraps the round → the queued turn starts.
    let step = m.step(chunk(StreamChunk::End {
        finish_reason: None,
    }));
    assert!(
        step.facts
            .contains(&Fact::ModelInputRequested(vec![Message::user("next")]))
    );
    assert_eq!(
        *step.facts.last().unwrap(),
        Fact::RoundState(ChatStateKind::Streaming)
    );
    assert!(matches!(m.state(), &State::Streaming { .. }));
}

#[test]
fn cancel_discards_queued_turns() {
    // Stop means stop: the wrap after a cancel does NOT auto-start queued
    // turns (a turn sent after the cancel still runs — the usual interject
    // order is cancel-first).
    let mut m = m();
    start_streaming(&mut m);
    m.step(Input::UserMessage("queued".into()));
    let step = m.step(Input::Cancel);
    assert!(
        !step
            .facts
            .iter()
            .any(|f| matches!(f, Fact::ModelInputRequested(_))),
        "queued turns die with the cancel"
    );
    assert_eq!(m.state(), &State::Idle);
    // A fresh turn still starts normally.
    let step = m.step(Input::UserMessage("fresh".into()));
    assert!(
        step.facts
            .contains(&Fact::ModelInputRequested(vec![Message::user("fresh")]))
    );
}

#[test]
fn cancel_during_tool_flight_discards_queued_turns() {
    let mut m = m();
    enter_processing(&mut m, vec![call("c1", "t")]);
    m.step(Input::UserMessage("queued".into()));
    m.step(Input::Cancel); // the interrupt goes out; the queue is cleared
    let step = m.step(Input::ToolFinished {
        call: call("c1", "t"),
        result: "partial".into(),
        ends_round: false,
    });
    // The absorbed cancel resolves: Cancelled + wrap — no queued round.
    assert!(step.facts.contains(&Fact::Wire(WireEvent::Cancelled)));
    assert!(
        !step
            .facts
            .iter()
            .any(|f| matches!(f, Fact::ModelInputRequested(_)))
    );
    assert_eq!(m.state(), &State::Idle);
}

#[test]
fn armed_gate_defers_to_queued_turns() {
    // A turn queued before the gate engages runs FIRST — it belongs to the
    // pre-rebuild context (the rebuild lands it inside the archive); the
    // gate fires at ITS wrap.
    let mut m = m();
    start_streaming(&mut m);
    m.step(Input::Hold); // armed
    m.step(Input::UserMessage("queued".into()));
    let step = m.step(chunk(StreamChunk::End {
        finish_reason: None,
    }));
    assert!(
        !step.facts.contains(&Fact::GateReleased),
        "the gate defers to the queued round"
    );
    assert!(matches!(m.state(), &State::Streaming { .. }));
    // The queued round wraps → NOW the gate fires.
    let step = m.step(chunk(StreamChunk::End {
        finish_reason: None,
    }));
    assert_eq!(*step.facts.last().unwrap(), Fact::GateReleased);
    assert_eq!(m.state(), &State::Idle);
}
