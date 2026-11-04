//! Native scenarios use the same resolver/receipt handoff as ordinary dictation.
use super::*;
use crate::injector::{
    self, AttemptIdentity, InjectionGuard, ResolvedApp, TextInjectionMode, TextInjector,
};
use crate::post_edit::{
    coordinator,
    diagnostics::{self, Reason, Status},
};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
struct Fixture {
    child: Child,
    input: ChildStdin,
    output: mpsc::Receiver<String>,
}
impl Fixture {
    fn start() -> Self {
        let mut child =
            Command::new(std::env::var("VOICEX_POST_EDIT_SESSION_FIXTURE").expect("fixture path"))
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
        let input = child.stdin.take().unwrap();
        let output = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                if tx.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), "ready");
        Self {
            child,
            input,
            output: rx,
        }
    }
    fn send(&mut self, command: &str) {
        writeln!(self.input, "{command}").unwrap();
        self.input.flush().unwrap();
        assert_eq!(
            self.output.recv_timeout(Duration::from_secs(3)).unwrap(),
            "ok"
        );
    }
    fn set(&mut self, field: &str, text: &str, caret: usize) {
        use base64::Engine;
        self.send(&format!(
            "set\t{field}\t{}\t{caret}",
            base64::engine::general_purpose::STANDARD.encode(text)
        ));
    }
    fn edit(&mut self, text: &str) {
        use base64::Engine;
        self.send(&format!(
            "edit\t{}",
            base64::engine::general_purpose::STANDARD.encode(text)
        ));
    }
    fn inject(&mut self, text: &str, mode: TextInjectionMode) {
        let attempt = coordinator::begin_attempt();
        let (tx, rx) = mpsc::sync_channel(1);
        let receipt = injector::inject_serialized_with_receipt(
            text,
            InjectionGuard::default(),
            AttemptIdentity {
                session_id: uuid::Uuid::new_v4(),
                record_id: uuid::Uuid::new_v4(),
                injection_epoch: 1,
            },
            || {
                tx.send(coordinator::prepare(attempt, self.child.id()).unwrap())
                    .unwrap();
                (
                    TextInjector::with_mode(mode, false),
                    Some(ResolvedApp {
                        process_id: self.child.id(),
                        remote_bridge: false,
                        app_name: Some("Fixture".into()),
                    }),
                )
            },
        );
        assert_eq!(receipt.outcome, injector::DispatchOutcome::CommandsPosted);
        rx.recv().unwrap().dispatched(receipt, text.into());
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            let status = diagnostics::snapshot();
            if status.status == Status::Observing {
                break;
            }
            if Instant::now() >= deadline || !matches!(status.status, Status::Verifying) {
                writeln!(self.input, "snapshot").unwrap();
                self.input.flush().unwrap();
                let actual = self.output.recv_timeout(Duration::from_secs(2));
                panic!("synthetic injection failed: {status:?}; {actual:?}");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        diagnostics::set_enabled(false);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn wait(test: impl Fn(&diagnostics::Snapshot) -> bool) -> diagnostics::Snapshot {
    let end = Instant::now() + Duration::from_secs(4);
    loop {
        let s = diagnostics::snapshot();
        if test(&s) {
            return s;
        }
        assert!(Instant::now() < end, "unexpected diagnostic: {s:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}
#[test]
#[ignore = "opens controlled synthetic AppKit fields and requires Accessibility"]
fn live_macos_session_scenarios() {
    let mut f = Fixture::start();
    diagnostics::set_enabled(true);
    f.set("plain", "", 0);
    f.inject("支座返力", TextInjectionMode::Typing);
    f.edit("支座反力");
    let s = wait(|s| s.substitutions == Some(1));
    assert_eq!(s.strategy, Some("event_with_checkpoints"));
    assert!(
        s.last_observed_ms.unwrap() < 3_000,
        "must observe before the 10s checkpoint"
    );
    coordinator::next_recording();
    let s = wait(|s| s.reason == Reason::NextRecording);
    assert!(matches!(s.status, Status::ObservedEdited | Status::Partial));
    assert_eq!(s.substitutions, Some(1));
    println!("empty native field: correction event and next-recording final read passed");

    f.set("plain", "原文：", 3);
    f.inject("支座返力", TextInjectionMode::Pasteboard);
    f.edit("原文：支座反力");
    wait(|s| s.substitutions == Some(1));
    f.send("secure");
    let s = wait(|s| {
        matches!(
            s.reason,
            Reason::FocusLeft | Reason::TargetChanged | Reason::SecureInput
        )
    });
    assert_eq!(s.status, Status::Partial);
    assert_eq!(s.substitutions, Some(1));
    let worker = ObserverWorker::start().unwrap();
    assert!(matches!(
        worker.capture(0, f.child.id(), Duration::from_millis(300)),
        Err(ReadError::SecureInput)
    ));
    println!("tail clipboard insertion: correction preserved on focus loss; password rejected");

    f.set("search", "前后", 1);
    f.inject("支座返力", TextInjectionMode::Typing);
    f.edit("前支座反力另外补一句后");
    let s = wait(|s| s.status == Status::Ambiguous);
    assert_eq!(s.reason, Reason::RangeAmbiguous);
    assert_ne!(s.insertions, Some(5));
    println!("mixed correction and continuation: rejected as ambiguous");

    f.set("plain", "", 0);
    f.inject("支座返力", TextInjectionMode::Typing);
    f.edit("支座反力");
    wait(|s| s.substitutions == Some(1));
    f.edit("");
    let s = wait(|s| s.reason == Reason::FieldCleared);
    assert_eq!(s.status, Status::Partial);
    assert_eq!(s.substitutions, Some(1));
    println!("clear after correction: last evidence retained as partial");

    f.set("plain", "", 0);
    f.inject("支座返力", TextInjectionMode::Typing);
    f.edit("支座反力");
    wait(|s| s.substitutions == Some(1));
    injector::inject_serialized_with_guard(
        TextInjectionMode::Typing,
        "重放文字",
        false,
        None,
        InjectionGuard::default(),
    )
    .unwrap();
    let s = wait(|s| s.reason == Reason::NextInjection);
    assert_eq!(s.status, Status::Partial);
    assert_eq!(s.substitutions, Some(1));
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(diagnostics::snapshot().reason, Reason::NextInjection);
    diagnostics::set_enabled(false);
    assert_eq!(diagnostics::snapshot().status, Status::Disabled);
    println!("legacy replay boundary and disable: old observation cannot publish late results");
}

#[test]
#[ignore = "measures actual dispatch into synthetic AppKit; requires Accessibility"]
fn live_macos_dispatch_samples() {
    let mut f = Fixture::start();
    let mut samples = Vec::new();
    for enabled in [false, true] {
        diagnostics::set_enabled(enabled);
        for _ in 0..40 {
            f.set("plain", "", 0);
            let attempt = coordinator::begin_attempt();
            let (tx, rx) = mpsc::sync_channel(1);
            let started = Instant::now();
            let receipt = injector::inject_serialized_with_receipt(
                "合成测试文字",
                InjectionGuard::default(),
                AttemptIdentity {
                    session_id: uuid::Uuid::new_v4(),
                    record_id: uuid::Uuid::new_v4(),
                    injection_epoch: 1,
                },
                || {
                    tx.send(coordinator::prepare(attempt, f.child.id()))
                        .unwrap();
                    (
                        TextInjector::with_mode(TextInjectionMode::Typing, false),
                        Some(ResolvedApp {
                            process_id: f.child.id(),
                            remote_bridge: false,
                            app_name: Some("Fixture".into()),
                        }),
                    )
                },
            );
            let dispatch_us = started.elapsed().as_micros() as u64;
            assert_eq!(receipt.outcome, injector::DispatchOutcome::CommandsPosted);
            let native_us = receipt
                .dispatch_finished_at
                .unwrap()
                .duration_since(receipt.dispatch_started_at.unwrap())
                .as_micros() as u64;
            if let Some(pending) = rx.recv().unwrap() {
                pending.dispatched(receipt, "合成测试文字".into());
                wait(|s| s.status == Status::Observing);
                coordinator::dispatch_boundary();
            }
            samples.push(serde_json::json!({"enabled":enabled,"dispatch_us":dispatch_us,"native_us":native_us}));
        }
    }
    std::fs::write(
        std::env::var("VOICEX_POST_EDIT_DISPATCH_SAMPLES").expect("output path"),
        serde_json::to_vec_pretty(&samples).unwrap(),
    )
    .unwrap();
}
