//! Loop pump tests — the wiring between the two channels: fact order,
//! RoundState transitions, and halt-on-close.

use super::*;
use flux_core::{
    ChatStateKind, LoopFact as Fact, LoopInput as Input, Message, Role, StreamChunk, StreamEvent,
    WireEvent,
};
use tokio::sync::mpsc;

#[tokio::test]
async fn pump_forwards_facts_in_order_and_tracks_round_state() {
    let (in_tx, in_rx) = mpsc::unbounded_channel::<LoopInput>();
    let (out_tx, mut out_rx) = mpsc::channel::<LoopFact>(OUT_CAPACITY);
    tokio::spawn(Loop::new(Machine::new(), in_rx, out_tx).run());

    in_tx.send(Input::UserMessage("hi".into())).unwrap();
    in_tx
        .send(Input::Stream(StreamEvent::Chunk(StreamChunk::Text(
            "a".into(),
        ))))
        .unwrap();
    in_tx
        .send(Input::Stream(StreamEvent::Chunk(StreamChunk::End {
            finish_reason: None,
        })))
        .unwrap();
    drop(in_tx); // all peers gone → the loop exits

    let mut facts = Vec::new();
    while let Some(f) = out_rx.recv().await {
        facts.push(f);
    }

    // Round start facts, the delta, then the wrap-up; RoundState(Streaming)
    // follows the start facts, RoundState(Idle) follows the wrap-up.
    assert_eq!(
        facts,
        vec![
            Fact::TranscriptCommitted(vec![Message::user("hi")]),
            Fact::ModelInputRequested(vec![Message::user("hi")]),
            Fact::RoundState(ChatStateKind::Streaming),
            Fact::Wire(WireEvent::TextDelta("a".into())),
            Fact::TranscriptCommitted(vec![Message {
                role: Role::Assistant,
                content: "a".into(),
                reasoning_content: None,
                tool_calls: Vec::new(),
                tool_call_id: None,
            }]),
            Fact::Wire(WireEvent::StreamEnd {
                finish_reason: None
            }),
            Fact::RoundEnded(flux_core::RoundOutcome::Completed {
                finish_reason: None,
            }),
            Fact::RoundState(ChatStateKind::Idle),
        ]
    );
}

#[tokio::test]
async fn pump_exits_when_the_fact_consumer_is_gone() {
    let (in_tx, in_rx) = mpsc::unbounded_channel::<LoopInput>();
    let (out_tx, out_rx) = mpsc::channel::<LoopFact>(OUT_CAPACITY);
    drop(out_rx); // the consumer died
    let task = tokio::spawn(Loop::new(Machine::new(), in_rx, out_tx).run());
    in_tx.send(Input::UserMessage("hi".into())).unwrap();
    // The send failure ends the loop task — it does not hang forever.
    tokio::time::timeout(std::time::Duration::from_secs(2), task)
        .await
        .expect("loop must exit when the consumer is gone")
        .unwrap();
}
