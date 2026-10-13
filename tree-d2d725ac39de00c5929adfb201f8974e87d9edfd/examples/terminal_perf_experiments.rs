// SPDX-FileCopyrightText: 2026 Alexey Zhokhov
// SPDX-License-Identifier: Apache-2.0

//! Headless terminal performance experiment runner.
//!
//! Captures machine-doable measurements from the termpane grid, appends them
//! as JSONL run-log events under `target/termpane-perf-runs/`, and prints a
//! run id for correlating the headless experiments.

use std::{
    fs::File,
    io::Write as _,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde_json::json;
use termpane::{DamageGrid, GridPatch};

const ROWS: u16 = 40;
const COLS: u16 = 120;
const SCROLLBACK: usize = 10_000;

#[derive(Debug)]
struct Dataset {
    name: &'static str,
    frames: Vec<Vec<u8>>,
}

#[derive(Debug)]
struct Measurement {
    dataset: &'static str,
    frames: usize,
    termpane_dirty_p99_us: u128,
    termpane_full_dump_p99_us: u128,
    termpane_text_dump_p99_us: u128,
    termpane_changed_cells_total: usize,
    termpane_patch_bytes_estimate: usize,
    termpane_text_bytes_total: usize,
}

/// Minimal JSONL run log: one `{"run_id", "kind", "message"}` object per line
/// in `<root>/<run_id>.jsonl`, mirroring the monorepo diagnostics event shape.
#[derive(Debug)]
struct RunLog {
    run_id: String,
    file: File,
}

impl RunLog {
    fn start(root: &Path, command: &str) -> std::io::Result<Self> {
        std::fs::create_dir_all(root)?;
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let run_id = format!("run-{nanos:x}-{}", std::process::id());
        let path: PathBuf = root.join(format!("{run_id}.jsonl"));
        let mut log = Self {
            run_id,
            file: File::create(path)?,
        };
        log.compact("run", &format!("command {command} started"));
        Ok(log)
    }

    fn compact(&mut self, kind: &str, message: &str) {
        let line = json!({
            "run_id": self.run_id,
            "kind": kind,
            "message": message,
        });
        // Best-effort measurement log: a failed write must not abort the run.
        let _ignored = writeln!(self.file, "{line}");
    }

    fn emit_run_summary(&mut self) {
        self.compact("run_summary", "run finished");
    }

    fn run_id(&self) -> &str {
        &self.run_id
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct ProcessSample {
    rss_kb: Option<u64>,
    cpu_percent: Option<f64>,
}

fn seq_dataset() -> Dataset {
    let frames = (1..=400).map(|n| format!("{n}\r\n").into_bytes()).collect();
    Dataset {
        name: "seq_1_100000_window",
        frames,
    }
}

fn agent_dataset() -> Dataset {
    let frames = (0..160)
        .map(|n| {
            format!(
                "\x1b[2J\x1b[HClaude Code\r\nturn={n:03}\r\n\x1b[10;1Hresponse line {n:03}\x1b[0K"
            )
            .into_bytes()
        })
        .collect();
    Dataset {
        name: "agent_response_tall_pane",
        frames,
    }
}

fn redraw_storm_dataset() -> Dataset {
    let frames = (0..120)
        .map(|frame| {
            let mut out = Vec::new();
            out.extend_from_slice(b"\x1b[2J\x1b[H");
            for row in 1..=ROWS {
                out.extend_from_slice(
                    format!("\x1b[{row};1Hframe={frame:03} row={row:02} status=running").as_bytes(),
                );
            }
            out
        })
        .collect();
    Dataset {
        name: "full_screen_redraw_storm",
        frames,
    }
}

fn yes_dataset() -> Dataset {
    let frames = (0..300).map(|_| b"y\r\n".to_vec()).collect();
    Dataset {
        name: "yes_5s_pathological_proxy",
        frames,
    }
}

fn patch_changed_cells(patch: &GridPatch<'_>) -> usize {
    patch
        .changed_spans()
        .map(|(_, _, cells)| cells.iter().filter(|cell| cell.has_contents()).count())
        .sum()
}

fn patch_bytes_estimate(patch: &GridPatch<'_>) -> usize {
    patch
        .changed_spans()
        .map(|(row_idx, start_col, cells)| {
            let cursor_move = format!("\x1b[{};{}H", row_idx + 1, start_col + 1).len();
            let row_bytes = cells
                .iter()
                .map(|cell| cell.contents().len())
                .sum::<usize>();
            cursor_move + row_bytes
        })
        .sum()
}

fn percentile_us(samples: &[Duration], percentile: usize) -> u128 {
    let mut micros = samples.iter().map(Duration::as_micros).collect::<Vec<_>>();
    micros.sort_unstable();
    let Some(last) = micros.len().checked_sub(1) else {
        return 0;
    };
    let idx = (last * percentile).div_ceil(100);
    micros.get(idx).copied().unwrap_or(0)
}

#[expect(
    clippy::disallowed_methods,
    reason = "headless perf example samples its own process metrics outside render/runtime threads"
)]
fn sample_process() -> ProcessSample {
    let pid = std::process::id().to_string();
    let Ok(output) = Command::new("ps")
        .args(["-o", "rss=", "-o", "%cpu=", "-p", &pid])
        .output()
    else {
        return ProcessSample::default();
    };
    if !output.status.success() {
        return ProcessSample::default();
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut fields = text.split_whitespace();
    let rss_kb = fields.next().and_then(|v| v.parse().ok());
    let cpu_percent = fields.next().and_then(|v| v.parse().ok());
    ProcessSample {
        rss_kb,
        cpu_percent,
    }
}

fn measure_dataset(dataset: &Dataset) -> Measurement {
    let mut grid = DamageGrid::new(ROWS, COLS, SCROLLBACK);

    let mut termpane_dirty = Vec::with_capacity(dataset.frames.len());
    let mut termpane_full = Vec::with_capacity(dataset.frames.len());
    let mut termpane_text_dump = Vec::with_capacity(dataset.frames.len());
    let mut termpane_changed_cells_total = 0usize;
    let mut termpane_patch_bytes_estimate = 0usize;
    let mut termpane_text_bytes_total = 0usize;

    for frame in &dataset.frames {
        let start = Instant::now();
        grid.process(frame);
        let patch = grid.dump_dirty_patch();
        termpane_dirty.push(start.elapsed());
        termpane_changed_cells_total += patch_changed_cells(&patch);
        termpane_patch_bytes_estimate += patch_bytes_estimate(&patch);

        let start = Instant::now();
        let snapshot = grid.dump();
        std::hint::black_box(snapshot);
        termpane_full.push(start.elapsed());

        let start = Instant::now();
        let contents = grid.dump().to_text();
        termpane_text_bytes_total += contents.len();
        std::hint::black_box(contents);
        termpane_text_dump.push(start.elapsed());
    }

    Measurement {
        dataset: dataset.name,
        frames: dataset.frames.len(),
        termpane_dirty_p99_us: percentile_us(&termpane_dirty, 99),
        termpane_full_dump_p99_us: percentile_us(&termpane_full, 99),
        termpane_text_dump_p99_us: percentile_us(&termpane_text_dump, 99),
        termpane_changed_cells_total,
        termpane_patch_bytes_estimate,
        termpane_text_bytes_total,
    }
}

fn measure_multipane() -> serde_json::Value {
    let pane_counts = [1usize, 4, 8, 16, 32];
    let mut results = Vec::new();
    for panes in pane_counts {
        let before = sample_process();
        let mut grids = (0..panes)
            .map(|_| DamageGrid::new(ROWS, COLS, SCROLLBACK))
            .collect::<Vec<_>>();
        let start = Instant::now();
        for frame in 0..800 {
            for (idx, grid) in grids.iter_mut().enumerate() {
                grid.process(format!("pane={idx:02} frame={frame:03}\r\n").as_bytes());
                std::hint::black_box(grid.dump_dirty_patch());
            }
        }
        let elapsed = start.elapsed();
        let after = sample_process();
        let frames_per_pane = 800usize;
        results.push(json!({
            "panes": panes,
            "frames_per_pane": frames_per_pane,
            "total_us": elapsed.as_micros(),
            "per_frame_us": elapsed.as_micros() / ((panes * frames_per_pane) as u128),
            "model_cells": panes * ROWS as usize * COLS as usize,
            "rss_before_kb": before.rss_kb,
            "rss_after_kb": after.rss_kb,
            "rss_delta_kb": before
                .rss_kb
                .zip(after.rss_kb)
                .map(|(before, after)| after.saturating_sub(before)),
            "cpu_percent_before": before.cpu_percent,
            "cpu_percent_after": after.cpu_percent,
        }));
    }
    json!(results)
}

fn measurement_json(measurement: &Measurement) -> serde_json::Value {
    json!({
        "dataset": measurement.dataset,
        "frames": measurement.frames,
        "termpane_dirty_p99_us": measurement.termpane_dirty_p99_us,
        "termpane_full_dump_p99_us": measurement.termpane_full_dump_p99_us,
        "termpane_text_dump_p99_us": measurement.termpane_text_dump_p99_us,
        "termpane_changed_cells_total": measurement.termpane_changed_cells_total,
        "termpane_patch_bytes_estimate": measurement.termpane_patch_bytes_estimate,
        "termpane_text_bytes_total": measurement.termpane_text_bytes_total,
    })
}

#[expect(
    clippy::print_stdout,
    reason = "example runner must print the invocation id for checklist evidence"
)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::current_dir()?.join("target/termpane-perf-runs");
    let mut run = RunLog::start(&root, "termpane-perf-experiments")?;

    let datasets = [
        seq_dataset(),
        agent_dataset(),
        redraw_storm_dataset(),
        yes_dataset(),
    ];
    let measurements = datasets.iter().map(measure_dataset).collect::<Vec<_>>();
    for measurement in &measurements {
        run.compact(
            "terminal_perf_measurement",
            &format!("{} {}", measurement.dataset, measurement_json(measurement)),
        );
    }
    run.compact(
        "terminal_perf_measurement",
        &format!("multipane_scaling_headless {}", measure_multipane()),
    );
    run.emit_run_summary();

    println!("run_id={}", run.run_id());
    println!("invocation_id={}", run.run_id());
    Ok(())
}
