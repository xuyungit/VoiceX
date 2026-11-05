//! Native scenarios use the same resolver/receipt handoff as ordinary dictation.
use super::*;
use crate::injector::{
    self, AttemptIdentity, InjectionGuard, ResolvedApp, TextInjectionMode, TextInjector,
};
use crate::post_edit::{
    coordinator,
    diagnostics::{self, Reason, Status},
    records::{self, Record},
};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
struct Fixture {
    child: Child,
    input: ChildStdin,
    output: mpsc::Receiver<String>,
    pending: Option<(String, std::thread::JoinHandle<Option<Record>>)>,
    db: rusqlite::Connection,
}
impl Fixture {
    fn start() -> Self {
        Self::start_binary(
            std::env::var("VOICEX_POST_EDIT_SESSION_FIXTURE").expect("fixture path"),
            &[],
        )
    }
    fn start_binary(path: String, args: &[&str]) -> Self {
        let mut child = Command::new(path)
            .args(args)
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
        let db = rusqlite::Connection::open_in_memory().unwrap();
        records::create_schema(&db).unwrap();
        Self {
            child,
            input,
            output: rx,
            pending: None,
            db,
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
    fn edit_at(&mut self, text: &str, caret: usize) {
        use base64::Engine;
        self.send(&format!(
            "edit\t{}\t{caret}",
            base64::engine::general_purpose::STANDARD.encode(text)
        ));
    }
    fn inject(&mut self, text: &str, mode: TextInjectionMode) {
        assert!(
            self.pending.is_none(),
            "finish the previous observation first"
        );
        // Ordinary injection resolves the foreground app before prepare().
        // Include that native initialization and verify the fixture owns focus.
        assert_eq!(
            objc2_app_kit::NSWorkspace::sharedWorkspace()
                .frontmostApplication()
                .unwrap()
                .processIdentifier() as u32,
            self.child.id()
        );
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
        let id = receipt.injection_id.to_string();
        let handle = rx
            .recv()
            .unwrap()
            .dispatched_with_schedule(
                receipt,
                text.into(),
                [
                    Duration::from_secs(3),
                    Duration::from_secs(6),
                    Duration::from_secs(9),
                ],
            )
            .unwrap();
        self.pending = Some((id, handle));
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
    fn saved(&mut self) -> Option<Record> {
        use rusqlite::OptionalExtension;
        let (id, handle) = self.pending.take().unwrap();
        let deadline = Instant::now() + Duration::from_secs(4);
        while !handle.is_finished() {
            assert!(Instant::now() < deadline, "observation did not finish");
            std::thread::sleep(Duration::from_millis(20));
        }
        if let Some(record) = handle.join().unwrap() {
            // Exercise production insertion and deserialize the actual row,
            // without opening or modifying the user's history database.
            assert!(records::insert(&self.db, &record).unwrap());
        }
        let payload: Option<String> = self
            .db
            .query_row(
                "SELECT payload FROM post_edit_record WHERE id=?1",
                [&id],
                |r| r.get(0),
            )
            .optional()
            .unwrap();
        payload.map(|p| serde_json::from_str(&p).unwrap())
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
    wait_for(Duration::from_secs(4), test)
}
fn wait_for(
    timeout: Duration,
    test: impl Fn(&diagnostics::Snapshot) -> bool,
) -> diagnostics::Snapshot {
    let end = Instant::now() + timeout;
    loop {
        let s = diagnostics::snapshot();
        if test(&s) {
            return s;
        }
        assert!(Instant::now() < end, "unexpected diagnostic: {s:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn stable_correction() -> diagnostics::Snapshot {
    let first = wait(|s| s.substitutions == Some(1));
    wait(|s| {
        s.substitutions == Some(1)
            && s.last_observed_ms.unwrap_or(0) >= first.last_observed_ms.unwrap() + 800
    })
}
fn assert_correction(record: Record, reason: &str, complete: bool) {
    assert_eq!(record.original_text, "支座返力");
    assert_eq!(record.edited_text, "支座反力");
    assert_eq!(record.substitutions, Some(1));
    assert_eq!(record.end_reason, reason);
    assert!(record.stable);
    assert_eq!(record.complete, complete);
}

fn emit_fixture_resources(pid: u32, phase: &str, started: Instant) {
    let output = Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "cputime=,rss="])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value = String::from_utf8(output.stdout).unwrap();
    let mut values = value.split_whitespace();
    println!(
        "native_target_metric={}",
        serde_json::json!({
            "phase": phase, "elapsedUs": started.elapsed().as_micros(),
            "cpuTime": values.next().unwrap(), "rssKiB": values.next().unwrap().parse::<u64>().unwrap(),
        })
    );
}
#[test]
#[ignore = "opens controlled synthetic AppKit fields and requires Accessibility"]
fn live_macos_session_scenarios() {
    let _guard = diagnostics::TEST_LOCK.lock().unwrap();
    super::macos::take_measurements();
    let mut f = Fixture::start();
    diagnostics::set_enabled(true);
    f.set("plain", "", 0);
    // The newly launched AppKit fixture lazily initializes its AX endpoint.
    // Check readiness explicitly before testing the bounded observation reads.
    let startup = ObserverWorker::start().unwrap();
    let startup_at = Instant::now();
    let empty = startup
        .capture(0, f.child.id(), Duration::from_millis(800))
        .unwrap();
    assert!(empty.text.is_empty());
    println!("synthetic AppKit AX ready after {:?}", startup_at.elapsed());
    // Resource samples are emitted raw; summarize-reader.mjs does the analysis.
    f.set("plain", &"测".repeat(4096), 2048);
    startup.read(0, Duration::from_millis(300)).unwrap();
    let measured_at = Instant::now();
    emit_fixture_resources(f.child.id(), "idle_start", measured_at);
    std::thread::sleep(Duration::from_secs(1));
    emit_fixture_resources(f.child.id(), "idle_end", measured_at);
    for _ in 0..20 {
        startup.read(0, Duration::from_millis(300)).unwrap();
        std::thread::sleep(Duration::from_millis(50));
    }
    emit_fixture_resources(f.child.id(), "read_end", measured_at);
    f.set("plain", "", 0);
    drop(startup);
    f.inject("支座返力", TextInjectionMode::Typing);
    f.edit("支座反力");
    let s = stable_correction();
    assert_eq!(s.strategy, Some("event_with_polling"));
    assert!(
        s.last_observed_ms.unwrap() < 3_000,
        "must observe before the 10s checkpoint"
    );
    coordinator::next_recording();
    let s = wait(|s| s.reason == Reason::NextRecording);
    assert!(matches!(s.status, Status::ObservedEdited | Status::Partial));
    assert_eq!(s.substitutions, Some(1));
    assert_correction(f.saved().unwrap(), "next_recording", true);
    println!("empty native field: correction event and next-recording final read passed");

    f.set("plain", "原文：", 3);
    f.inject("支座返力", TextInjectionMode::Pasteboard);
    f.edit("原文：支座反力");
    stable_correction();
    f.send("secure");
    let s = wait(|s| {
        matches!(
            s.reason,
            Reason::FocusLeft | Reason::TargetChanged | Reason::SecureInput
        )
    });
    assert_eq!(s.status, Status::Partial);
    assert_eq!(s.substitutions, Some(1));
    let record = f.saved().unwrap();
    assert_correction(
        record,
        serde_json::to_value(s.reason).unwrap().as_str().unwrap(),
        false,
    );
    let worker = ObserverWorker::start().unwrap();
    assert!(matches!(
        worker.capture(0, f.child.id(), Duration::from_millis(300)),
        Err(ReadError::SecureInput)
    ));
    println!("tail clipboard insertion: correction preserved on focus loss; password rejected");

    f.set("search", "前后", 1);
    f.inject("支座返力", TextInjectionMode::Typing);
    let before_reads = super::macos::BODY_READS.load(Ordering::SeqCst);
    f.edit("前支座反力另外补一句后");
    wait(|_| super::macos::BODY_READS.load(Ordering::SeqCst) >= before_reads + 2);
    assert_eq!(diagnostics::snapshot().status, Status::Observing);
    f.edit("前支座反力后");
    stable_correction();
    coordinator::next_recording();
    wait(|s| s.reason == Reason::NextRecording);
    assert_correction(f.saved().unwrap(), "next_recording", true);
    println!("temporary ambiguity: recovered and saved the corrected slice");

    f.set("search", "前后", 1);
    f.inject("支座返力", TextInjectionMode::Typing);
    f.edit("前支座反力另外补一句后");
    let s = wait_for(Duration::from_secs(12), |s| s.status == Status::Ambiguous);
    assert_eq!(s.reason, Reason::RangeAmbiguous);
    assert_ne!(s.insertions, Some(5));
    assert!(f.saved().is_none());
    println!("persistent ambiguity: ended without saving a comparison");

    f.set("plain", "", 0);
    f.inject("支座返力", TextInjectionMode::Typing);
    f.edit("支座反力");
    stable_correction();
    f.edit("");
    let s = wait(|s| s.reason == Reason::FieldCleared);
    assert_eq!(s.status, Status::Partial);
    assert_eq!(s.substitutions, Some(1));
    assert_correction(f.saved().unwrap(), "field_cleared", false);
    println!("clear after correction: last evidence retained as partial");

    f.set("plain", "", 0);
    f.inject("支座返力", TextInjectionMode::Typing);
    f.edit("支座反力");
    stable_correction();
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
    assert_correction(f.saved().unwrap(), "next_injection", false);
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(diagnostics::snapshot().reason, Reason::NextInjection);
    println!("legacy replay boundary and disable: old observation cannot publish late results");

    let prefix = "前".repeat(30_000);
    let suffix = "后".repeat(30_000);
    f.set("plain", &format!("{prefix}{suffix}"), 30_000);
    let probe = ObserverWorker::start().unwrap();
    let local = probe
        .capture(0, f.child.id(), Duration::from_millis(300))
        .unwrap();
    assert_eq!(local.text.encode_utf16().count(), 4096);
    assert_eq!(local.selection_utf16, 2048..2048);
    drop(probe);
    f.inject("支座返力", TextInjectionMode::Typing);
    f.edit_at(&format!("{prefix}支座反力{suffix}"), 30_004);
    stable_correction();
    coordinator::next_recording();
    wait(|s| s.reason == Reason::NextRecording);
    assert_correction(f.saved().unwrap(), "next_recording", true);
    println!(
        "60K native document: bounded local read, actual injection and saved correction passed"
    );
    diagnostics::set_enabled(false);
    assert_eq!(diagnostics::snapshot().status, Status::Disabled);
    let measurements = super::macos::take_measurements();
    assert!(measurements.iter().any(|m| m.operation == "read"));
    for metric in measurements {
        if metric.operation == "read" || metric.operation == "reframe" {
            assert_eq!(metric.framework_checks, 0);
            assert_eq!(metric.capability_checks, 0);
        }
        println!(
            "native_read_metric={}",
            serde_json::json!({
                "operation": metric.operation, "attributeQueries": metric.attribute_queries,
                "elapsedUs": metric.elapsed_us, "frameworkChecks": metric.framework_checks,
                "capabilityChecks": metric.capability_checks,
            })
        );
    }
}

#[test]
#[ignore = "foreground synthetic WebKit reader, selection, editability and password checks"]
fn live_webkit_marker_ranges() {
    use crate::post_edit::range::{RangeTracker, Snapshot};
    use base64::Engine;
    let _guard = diagnostics::TEST_LOCK.lock().unwrap();
    let path = std::env::var("VOICEX_POST_EDIT_WEBKIT_FIXTURE").expect("compiled WebKit fixture");
    let mut f = Fixture::start_binary(path, &["editable"]);
    let worker = ObserverWorker::start().unwrap();
    let baseline = worker
        .capture(0, f.child.id(), Duration::from_millis(800))
        .unwrap();
    assert_eq!(
        super::macos::fixture_role_diagnostic(f.child.id()).unwrap(),
        "AXWebArea"
    );
    assert_eq!(baseline.text, "VoiceX test A");
    let tracker = RangeTracker::from_post_anchored(
        &Snapshot {
            target: baseline.target,
            text: &baseline.text,
            selection_utf16: baseline.selection_utf16,
        },
        "VoiceX test A",
    )
    .unwrap();
    f.send(&format!(
        "set\t{}\t13",
        base64::engine::general_purpose::STANDARD.encode("VoiceX test B")
    ));
    let edited = worker.read(0, Duration::from_millis(300)).unwrap();
    assert_eq!(
        tracker.slice(&Snapshot {
            target: edited.target,
            text: &edited.text,
            selection_utf16: edited.selection_utf16
        }),
        Ok("VoiceX test B")
    );
    let document = format!(
        "{}VoiceX test A{}",
        "前".repeat(30_000),
        "后".repeat(30_000)
    );
    f.send(&format!(
        "set\t{}\t30013",
        base64::engine::general_purpose::STANDARD.encode(document)
    ));
    let local = worker
        .capture(0, f.child.id(), Duration::from_millis(300))
        .unwrap();
    assert!(local.text.encode_utf16().count() <= 4096);
    assert!(RangeTracker::from_post_anchored(
        &Snapshot {
            target: local.target,
            text: &local.text,
            selection_utf16: local.selection_utf16
        },
        "VoiceX test A"
    )
    .is_ok());
    for (mode, expected) in [
        ("readonly", ReadError::Unsupported),
        ("password", ReadError::SecureInput),
    ] {
        f.send("mode\teditable");
        worker
            .capture(0, f.child.id(), Duration::from_millis(300))
            .unwrap();
        f.send(&format!("mode\t{mode}"));
        let before = super::macos::BODY_READS.load(Ordering::SeqCst);
        let error = worker.read(0, Duration::from_millis(300)).unwrap_err();
        assert!(
            error == expected || error == ReadError::TargetChanged,
            "cached read after {mode}: {error:?}"
        );
        assert_eq!(
            worker
                .capture(0, f.child.id(), Duration::from_millis(300))
                .unwrap_err(),
            expected
        );
        assert_eq!(super::macos::BODY_READS.load(Ordering::SeqCst), before);
    }
}

#[test]
#[ignore = "foreground synthetic AppKit marked-text states; requires Accessibility"]
fn live_appkit_marked_text() {
    let _guard = diagnostics::TEST_LOCK.lock().unwrap();
    let path = std::env::var("VOICEX_POST_EDIT_IME_FIXTURE").expect("compiled IME fixture");
    let mut f = Fixture::start_binary(path, &[]);
    let worker = ObserverWorker::start().unwrap();
    let initial = worker
        .capture(0, f.child.id(), Duration::from_millis(800))
        .unwrap();
    assert_eq!(initial.composition, super::CompositionState::Inactive);
    let before = super::macos::BODY_READS.load(Ordering::SeqCst);
    f.send("mark");
    assert_eq!(
        worker.read(0, Duration::from_millis(300)).unwrap_err(),
        ReadError::CompositionActive
    );
    assert_eq!(super::macos::BODY_READS.load(Ordering::SeqCst), before);
    f.send("commit");
    let committed = worker.read(0, Duration::from_millis(300)).unwrap();
    assert_eq!(committed.composition, super::CompositionState::Inactive);
    assert_eq!(committed.text, "支座反力计算");
}

#[test]
#[ignore = "reads user-prepared synthetic drafts in named editors; requires foreground interaction"]
fn live_macos_editors() {
    use crate::post_edit::{
        observation::{EndReason, Observation},
        range::{RangeTracker, Snapshot},
    };
    const ORIGINAL: &str = "VoiceX test A";
    const EDITED: &str = "VoiceX test B";
    let raw = std::env::var("VOICEX_POST_EDIT_EDITOR_PID")
        .expect("editor PID from the foreground launcher");
    let db = rusqlite::Connection::open_in_memory().unwrap();
    records::create_schema(&db).unwrap();
    let pid: u32 = raw.parse().unwrap();
    let target =
        objc2_app_kit::NSRunningApplication::runningApplicationWithProcessIdentifier(pid as i32)
            .expect("running target");
    let bundle = target.bundleIdentifier().unwrap().to_string();
    assert!(matches!(
        bundle.as_str(),
        "com.anysphere.sand"
            | "com.tencent.workbuddy.mac"
            | "com.deepseek.dsh"
            | "md.obsidian"
            | "com.apple.mail"
            | "abnerworks.Typora"
    ));
    println!("waiting for synthetic original in {bundle}");
    let worker = ObserverWorker::start().unwrap();
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut attempts = 0;
    let mut last_state = String::from("target has not been foreground");
    let baseline = loop {
        assert!(
            Instant::now() < deadline,
            "synthetic original not ready in {bundle}: {attempts} captures; {last_state}"
        );
        if objc2_app_kit::NSWorkspace::sharedWorkspace()
            .frontmostApplication()
            .is_some_and(|app| app.processIdentifier() as u32 == pid)
        {
            attempts += 1;
            match worker.capture(0, pid, Duration::from_millis(300)) {
                Ok(snapshot)
                    if RangeTracker::from_post_anchored(
                        &Snapshot {
                            target: snapshot.target,
                            text: &snapshot.text,
                            selection_utf16: snapshot.selection_utf16.clone(),
                        },
                        ORIGINAL,
                    )
                    .is_ok() =>
                {
                    break snapshot
                }
                Ok(snapshot) => {
                    last_state = format!(
                        "read {} UTF-16 units; original_present={}, edited_present={}, selection={:?}, original_after_space_normalization={}",
                        snapshot.text.encode_utf16().count(),
                        snapshot.text.contains(ORIGINAL),
                        snapshot.text.contains(EDITED),
                        snapshot.selection_utf16,
                        snapshot.text.replace('\u{00a0}', " ").contains(ORIGINAL),
                    );
                }
                Err(
                    error @ (ReadError::NoValue
                    | ReadError::Unsupported
                    | ReadError::CompositionActive),
                ) => {
                    last_state = format!("native read: {error:?}");
                }
                Err(error) => panic!("native baseline error for {bundle}: {error:?}"),
            }
            if attempts == 1 || attempts % 10 == 0 {
                println!("{bundle}: {attempts} captures; {last_state}");
                println!(
                    "focused role={:?}, subrole={:?}",
                    super::macos::fixture_role_diagnostic(pid),
                    super::macos::fixture_subrole_diagnostic(pid)
                );
            }
        }
        std::thread::sleep(Duration::from_millis(350));
    };
    let tracker = RangeTracker::from_post_anchored(
        &Snapshot {
            target: baseline.target,
            text: &baseline.text,
            selection_utf16: baseline.selection_utf16.clone(),
        },
        ORIGINAL,
    )
    .expect("synthetic original must be anchored at its caret");
    let mut observation = Observation::new(0, tracker, baseline.read_at);
    println!("baseline ready in {bundle}; waiting for correction");
    let deadline = Instant::now() + Duration::from_secs(25);
    let mut text_event = false;
    loop {
        assert!(
            Instant::now() < deadline,
            "stable correction not observed in {bundle}"
        );
        let events = worker.events.wait(0, Duration::from_millis(350));
        if events & TEXT_CHANGED != 0 {
            text_event = true;
            observation.text_changed();
        }
        let sample = match worker.read(0, Duration::from_millis(300)) {
            Ok(sample) => sample,
            Err(ReadError::CompositionActive) => {
                observation.unresolved_input();
                continue;
            }
            Err(error) => panic!("native observation error for {bundle}: {error:?}"),
        };
        if observation.sample_read(0, &sample).is_err() {
            continue;
        }
        let result = observation.finish(EndReason::Timeout);
        if result.confirmed.as_ref().is_some_and(|s| s.text == EDITED) {
            let receipt = injector::InjectionReceipt {
                session_id: uuid::Uuid::new_v4(),
                record_id: uuid::Uuid::new_v4(),
                injection_id: uuid::Uuid::new_v4(),
                injection_epoch: 1,
                outcome: injector::DispatchOutcome::CommandsPosted,
                dispatch_started_at: Some(baseline.read_at),
                dispatch_finished_at: Some(baseline.read_at),
                resolved_app: Some(ResolvedApp {
                    process_id: pid,
                    remote_bridge: false,
                    app_name: Some(bundle.clone()),
                }),
            };
            // This validates reader/range/storage with a synthetic identity;
            // ASR and actual injection dispatch are outside this test.
            let record = coordinator::record_for(
                &receipt,
                receipt.resolved_app.as_ref().unwrap(),
                baseline.read_at,
                &observation,
                &result,
                Reason::Timeout,
                "event_with_checkpoints",
                "post_anchored",
            )
            .unwrap();
            assert!(records::insert(&db, &record).unwrap());
            let payload: String = db
                .query_row(
                    "SELECT payload FROM post_edit_record WHERE id=?1",
                    [&record.id],
                    |r| r.get(0),
                )
                .unwrap();
            let stored: Record = serde_json::from_str(&payload).unwrap();
            assert_eq!(stored.original_text, ORIGINAL);
            assert_eq!(stored.edited_text, EDITED);
            assert_eq!(stored.substitutions, Some(1));
            assert!(stored.stable && stored.complete);
            println!("{bundle}: native read, stable correction and isolated saved pair passed; subscription={}, text_event={text_event}", baseline.event_driven);
            break;
        }
    }
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
