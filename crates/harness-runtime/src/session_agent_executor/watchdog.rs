//! The watchdog of a build attempt: notices an attempt that keeps working
//! without getting anywhere and stops it, so the task queue can undo it and
//! try a smaller task instead of letting it run for an hour.
//!
//! Layers:
//! - [`Limits`]: when to nudge and when to stop, per signal.
//! - [`Watchdog`]: the signals of one attempt, fed by every tool call.
//! - [`WatchedRegistry`]: wraps the attempt's tools so every call reaches the
//!   watchdog, and adds its note to the tool's result.
//!
//! The signals come from tool calls only (no model is asked): the same file
//! edited or read over and over, edits that do not apply, project checks that
//! keep failing without fewer errors, and the size of the attempt. A nudge is
//! a note on the tool result; a stop is a note that asks for the final reply,
//! after which tools are refused, and a hard stop when the model still goes on.
//! The default limits sit between the healthy and the stuck build steps of
//! the 2026-10-25 traces (healthy: at most 143 calls, 44 edits and 12 reads
//! of one file, 6 failed edits, 2 failed checks in a row without progress).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::traits::ToolRegistry;

/// The error code of an attempt the watchdog stopped.
pub(crate) const WATCHDOG_STOPPED: &str = "watchdog_stopped";

/// A nudge and a stop threshold for one signal.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Threshold {
    pub nudge: u32,
    pub stop: u32,
}

/// When the watchdog nudges and when it stops an attempt.
#[derive(Debug, Clone)]
pub(crate) struct Limits {
    pub tool_calls: Threshold,
    pub edits_of_one_file: Threshold,
    pub reads_of_one_file: Threshold,
    pub failed_edits: Threshold,
    /// Failed project checks in a row with no fewer errors than the best so far.
    pub checks_without_progress: Threshold,
    pub duration: Duration,
    /// Tool calls refused after a stop before the run is cancelled.
    pub refused_after_stop: u32,
    /// A planning step: it reads and writes a plan, so its notes ask for the
    /// plan, and it is only nudged, never stopped.
    pub planner: bool,
}

impl Limits {
    /// A task planner's limits: a nudge to write the plan after 40 calls
    /// (the healthy planners of the 2026-10-25 traces made 10-36) or the
    /// fourth read of one file; no stop, as a stopped planner has no plan.
    pub(crate) fn planner() -> Self {
        let never = Threshold {
            nudge: u32::MAX,
            stop: u32::MAX,
        };
        Self {
            tool_calls: Threshold {
                nudge: 40,
                stop: u32::MAX,
            },
            edits_of_one_file: never,
            reads_of_one_file: Threshold {
                nudge: 4,
                stop: u32::MAX,
            },
            failed_edits: never,
            checks_without_progress: never,
            duration: Duration::MAX,
            refused_after_stop: u32::MAX,
            planner: true,
        }
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            tool_calls: Threshold {
                nudge: 160,
                stop: 220,
            },
            edits_of_one_file: Threshold {
                nudge: 25,
                stop: 60,
            },
            reads_of_one_file: Threshold {
                nudge: 12,
                stop: 25,
            },
            failed_edits: Threshold { nudge: 6, stop: 12 },
            checks_without_progress: Threshold { nudge: 3, stop: 6 },
            duration: Duration::from_secs(30 * 60),
            refused_after_stop: 4,
            planner: false,
        }
    }
}

/// What the watchdog says about one tool call.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Verdict {
    Fine,
    Nudge(String),
    Stop(String),
}

#[derive(Default)]
struct Signals {
    calls: u32,
    edits: HashMap<String, u32>,
    reads: HashMap<String, u32>,
    failed_edits: u32,
    /// The fewest errors each check (by its arguments) has reported.
    best_check_errors: HashMap<String, u32>,
    checks_without_progress: u32,
    /// Nudges already given, so each is given once.
    nudged: Vec<String>,
    stopped: Option<String>,
    refused: u32,
}

/// The signals of one build attempt.
pub(crate) struct Watchdog {
    limits: Limits,
    started: Instant,
    signals: Mutex<Signals>,
    hard_stop: CancellationToken,
}

impl Watchdog {
    pub(crate) fn new(limits: Limits) -> Arc<Self> {
        Arc::new(Self {
            limits,
            started: Instant::now(),
            signals: Mutex::new(Signals::default()),
            hard_stop: CancellationToken::new(),
        })
    }

    /// Why the attempt was stopped, once it was.
    pub(crate) fn stopped(&self) -> Option<String> {
        self.signals.lock().unwrap().stopped.clone()
    }

    /// Fired when the model keeps calling tools after the stop.
    pub(crate) fn hard_stop(&self) -> CancellationToken {
        self.hard_stop.clone()
    }

    /// Before a call runs: refuses it when the attempt was stopped (or ran
    /// out of time), with the message the model gets instead of a result.
    fn refuse(&self) -> Option<String> {
        let mut signals = self.signals.lock().unwrap();
        if signals.stopped.is_none() && self.started.elapsed() >= self.limits.duration {
            signals.stopped = Some(format!(
                "the attempt ran for more than {} minutes",
                self.limits.duration.as_secs() / 60
            ));
        }
        let reason = signals.stopped.clone()?;
        signals.refused += 1;
        if signals.refused >= self.limits.refused_after_stop {
            self.hard_stop.cancel();
        }
        Some(stop_note(&reason))
    }

    /// Records a finished call and says whether to nudge or stop.
    pub(crate) fn observe(
        &self,
        tool: &str,
        arguments: &Value,
        failed: bool,
        output: &Value,
    ) -> Verdict {
        let mut signals = self.signals.lock().unwrap();
        signals.calls += 1;
        let path = file_argument(arguments);
        let calls_note = if self.limits.planner {
            format!("This planning step has made {} tool calls. You have read enough: write the plan now from what you know; the builder reads the details itself.", signals.calls)
        } else {
            format!("This attempt has made {} tool calls. Finish the task with fewer, larger steps: read whole files once and make each change in one edit.", signals.calls)
        };
        let mut found: Vec<(String, u32, Threshold, String)> = vec![(
            "calls".into(),
            signals.calls,
            self.limits.tool_calls,
            calls_note,
        )];
        match (tool, path) {
            ("edit_file" | "write_file", Some(path)) => {
                let count = bump(&mut signals.edits, &path);
                found.push((format!("edits:{path}"), count, self.limits.edits_of_one_file, format!("You have edited {path} {count} times. Read the whole file once, decide the complete new content, and write it with one write_file instead of many small edits.")));
                if failed {
                    signals.failed_edits += 1;
                    found.push(("failed_edits".into(), signals.failed_edits, self.limits.failed_edits, format!("{} edits did not apply. Re-read the whole file before editing; for a file under a few hundred lines, write its complete content with write_file.", signals.failed_edits)));
                }
            }
            ("read_file", Some(path)) => {
                let count = bump(&mut signals.reads, &path);
                let note = if self.limits.planner {
                    format!("You have read {path} {count} times. Use what you already read; the builder will read it again anyway.")
                } else {
                    format!("You have read {path} {count} times. Read it whole once (no line range) and work from that copy.")
                };
                found.push((
                    format!("reads:{path}"),
                    count,
                    self.limits.reads_of_one_file,
                    note,
                ));
            }
            ("run_check", _) => {
                if let Some(count) = check_progress(&mut signals, arguments, failed, output) {
                    found.push(("checks".into(), count, self.limits.checks_without_progress, format!("The last {count} checks failed without fewer errors. Stop patching symptoms: re-read the errors, find their common cause, and fix that; if the task is larger than one change, finish a smaller part that compiles and say what remains.")));
                }
            }
            _ => {}
        }
        // A stop wins over a nudge of the same call.
        if let Some((key, count, _, message)) = found
            .iter()
            .find(|(_, count, threshold, _)| *count >= threshold.stop)
        {
            let reason = format!(
                "{} ({key} reached {count})",
                message.split(". ").next().unwrap_or(message)
            );
            signals.stopped = Some(reason.clone());
            return Verdict::Stop(reason);
        }
        for (key, count, threshold, message) in found {
            if count >= threshold.nudge && !signals.nudged.contains(&key) {
                signals.nudged.push(key);
                return Verdict::Nudge(message);
            }
        }
        Verdict::Fine
    }
}

fn bump(counts: &mut HashMap<String, u32>, path: &str) -> u32 {
    let count = counts.entry(path.to_owned()).or_default();
    *count += 1;
    *count
}

fn file_argument(arguments: &Value) -> Option<String> {
    ["path", "file_path"]
        .iter()
        .find_map(|key| arguments.get(key).and_then(Value::as_str))
        .map(|path| path.trim_start_matches("./").to_owned())
}

/// Counts failed checks that did not lower the error count. Each check
/// (typecheck, a test file, the whole suite: told apart by the call's
/// arguments) is compared only with itself, so a focused test's single
/// failure never makes a typecheck's falling count look stuck. A passing
/// check, or fewer errors than that check ever had, starts over; the first
/// result of a check counts neither way. `None` when it passed.
fn check_progress(
    signals: &mut Signals,
    arguments: &Value,
    failed: bool,
    output: &Value,
) -> Option<u32> {
    let check = arguments.to_string();
    if !failed {
        signals.best_check_errors.remove(&check);
        signals.checks_without_progress = 0;
        return None;
    }
    match (
        error_count(&output_text(output)),
        signals.best_check_errors.get(&check).copied(),
    ) {
        (Some(now), Some(best)) if now < best => {
            signals.best_check_errors.insert(check, now);
            signals.checks_without_progress = 0;
        }
        (Some(now), None) => {
            signals.best_check_errors.insert(check, now);
        }
        _ => signals.checks_without_progress += 1,
    }
    Some(signals.checks_without_progress)
}

/// The text of a tool result: its strings with their real line breaks (a
/// check's failure comes as `{"error": "..."}`, whose JSON form would escape
/// them).
fn output_text(output: &Value) -> String {
    match output {
        Value::String(text) => text.clone(),
        Value::Array(items) => items.iter().map(output_text).collect::<Vec<_>>().join("\n"),
        Value::Object(fields) => fields
            .values()
            .map(output_text)
            .collect::<Vec<_>>()
            .join("\n"),
        other => other.to_string(),
    }
}

/// The first "N errors" (or "N error") in a check's output.
fn error_count(text: &str) -> Option<u32> {
    let words: Vec<&str> = text.split_whitespace().collect();
    words.windows(2).find_map(|pair| {
        let word = pair[1].trim_end_matches(|c: char| !c.is_alphanumeric());
        (word == "errors" || word == "error")
            .then(|| pair[0].parse().ok())
            .flatten()
    })
}

fn stop_note(reason: &str) -> String {
    format!("The workflow stopped this attempt: {reason}. Do not call more tools. Reply now with status checkpoint: what you changed, what still fails and why, and what a smaller first step would be. The workflow will undo this attempt and plan the task again in smaller steps.")
}

/// Adds the watchdog's note to a tool result.
fn with_note(output: Value, note: &str) -> Value {
    match output {
        Value::String(text) => Value::String(format!("{text}\n\n[workflow] {note}")),
        Value::Object(mut fields) => {
            fields.insert("workflow_note".into(), Value::String(note.to_owned()));
            Value::Object(fields)
        }
        other => json!({ "result": other, "workflow_note": note }),
    }
}

/// An attempt's tools, each call watched.
pub(crate) struct WatchedRegistry {
    pub inner: Arc<dyn ToolRegistry>,
    pub watchdog: Arc<Watchdog>,
}

impl ToolRegistry for WatchedRegistry {
    fn register(
        &self,
        executor: Arc<dyn harness_tools::ToolExecutor>,
    ) -> Result<(), harness_tools::RegistrationError> {
        self.inner.register(executor)
    }

    fn get_executor(&self, tool_id: &str) -> Option<Arc<dyn harness_tools::ToolExecutor>> {
        let inner = self.inner.get_executor(tool_id)?;
        Some(Arc::new(WatchedTool {
            name: tool_id.to_owned(),
            inner,
            watchdog: self.watchdog.clone(),
        }))
    }

    fn descriptors(&self) -> Vec<harness_tools::ToolDescriptor> {
        self.inner.descriptors()
    }
}

struct WatchedTool {
    name: String,
    inner: Arc<dyn harness_tools::ToolExecutor>,
    watchdog: Arc<Watchdog>,
}

#[async_trait]
impl harness_tools::ToolExecutor for WatchedTool {
    fn descriptor(&self) -> harness_tools::ToolDescriptor {
        self.inner.descriptor()
    }

    fn call_timeout(&self, arguments: &Value, default: Duration) -> Option<Duration> {
        self.inner.call_timeout(arguments, default)
    }

    async fn execute(
        &self,
        input: harness_tools::ToolInput,
        cancel: CancellationToken,
    ) -> Result<harness_tools::ToolResult, harness_tools::ToolError> {
        if let Some(refusal) = self.watchdog.refuse() {
            return Ok(harness_tools::ToolResult {
                call_id: self.name.clone(),
                output: json!({ "error": refusal }),
                is_error: true,
            });
        }
        let arguments = input.arguments.clone();
        let mut result = self.inner.execute(input, cancel).await?;
        match self
            .watchdog
            .observe(&self.name, &arguments, result.is_error, &result.output)
        {
            Verdict::Fine => {}
            Verdict::Nudge(note) => result.output = with_note(result.output, &note),
            Verdict::Stop(reason) => result.output = with_note(result.output, &stop_note(&reason)),
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests;
