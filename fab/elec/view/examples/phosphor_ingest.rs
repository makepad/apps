//! Flat-out trigger acquisition and phosphor rasterisation, with independent worker frames.

use fab_elec_view::instruments::acquire::{Acquisition, LaneFrame, LaneSetup};
use fab_elec_view::instruments::phosphor::PhosphorMap;
use fab_elec_instr::synth::Synth;
use fab_elec_bench::trigger::{Edge, EdgeTrigger, Slope, Window};
use std::hint::black_box;
use std::mem;
use std::process::ExitCode;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const PERIODS: usize = 64;
const CHUNK: usize = 4096;
const MERGE_REPETITIONS: u32 = 10;

#[derive(Clone, Copy)]
struct Config {
    threads: usize,
    lanes: usize,
    seconds: Duration,
    points: usize,
    w: u32,
    h: u32,
    frame_interval: Duration,
}

impl Config {
    fn parse() -> Result<Self, String> {
        let mut config = Self {
            lanes: 1,
            threads: thread::available_parallelism().map_or(1, |n| n.get()),
            seconds: Duration::from_secs(3),
            points: 500,
            w: 1600,
            h: 800,
            frame_interval: Duration::from_micros(16_700),
        };
        let mut args = std::env::args().skip(1);
        while let Some(name) = args.next() {
            if !matches!(name.as_str(), "--lanes" | "--threads" | "--seconds" | "--points" | "--w" | "--h" | "--frame-ms") {
                return Err(format!("unknown argument: {name}"));
            }
            let value = args.next().ok_or_else(|| format!("missing value for {name}"))?;
            match name.as_str() {
                "--lanes" => config.lanes = parse(&name, &value)?,
                "--threads" => config.threads = parse(&name, &value)?,
                "--seconds" => config.seconds = duration(&name, &value, 1.0)?,
                "--points" => config.points = parse(&name, &value)?,
                "--w" => config.w = parse(&name, &value)?,
                "--h" => config.h = parse(&name, &value)?,
                "--frame-ms" => config.frame_interval = duration(&name, &value, 1000.0)?,
                _ => unreachable!(),
            }
        }
        if config.lanes == 0 || config.threads == 0 || config.points == 0 || config.w == 0 || config.h == 0 {
            return Err("--lanes, --threads, --points, --w and --h must be positive integers".into());
        }
        if config.points.checked_mul(PERIODS).is_none_or(|n| n > isize::MAX as usize / mem::size_of::<f64>()) {
            return Err("--points is too large for the replay buffers".into());
        }
        if (config.w as usize).checked_mul(config.h as usize).is_none_or(|n| n > isize::MAX as usize / mem::size_of::<f32>()) {
            return Err("--w and --h are too large for a hit image".into());
        }
        Ok(config)
    }
}

fn parse<T: FromStr>(name: &str, value: &str) -> Result<T, String> {
    value.parse().map_err(|_| format!("invalid value for {name}: {value}"))
}

fn duration(name: &str, value: &str, units_per_second: f64) -> Result<Duration, String> {
    let seconds = parse::<f64>(name, value)? / units_per_second;
    let duration = Duration::try_from_secs_f64(seconds).map_err(|_| format!("{name} must be a finite positive duration"))?;
    if duration.is_zero() {
        return Err(format!("{name} must be at least one nanosecond"));
    }
    Ok(duration)
}

struct WorkerResult {
    elapsed: Duration,
    samples: u64,
    events: u64,
    waveforms: u64,
    ingest_time: Duration,
    reset_time: Duration,
    resets: u64,
    frame: LaneFrame,
}

fn worker(index: usize, config: Config, stop: &AtomicBool) -> WorkerResult {
    let source = Synth { seed: 1 + index as u64, ..Synth::pwm() };
    let period = source.period();
    let fs = config.points as f64 / period;
    let n = PERIODS * config.points;
    let values = source.uniform(fs, n);
    let base_times: Vec<f64> = (0..n).map(|k| k as f64 / fs).collect();
    let mut times = vec![0.0; n];
    let map = PhosphorMap::screen(config.w, config.h, period / 2.0, period, source.lo - 0.5, source.hi + 0.5);
    let mut acquisition = Acquisition::new(
        EdgeTrigger { level: (source.lo + source.hi) / 2.0, hyst: 0.1, slope: Slope::Rising, holdoff: 0.0 },
        0,
        Window { pre: period / 2.0, post: period / 2.0 },
        vec![LaneSetup::time(map); config.lanes],
    );
    let setups = vec![LaneSetup::time(map); config.lanes];
    let mut point = vec![0.0; config.lanes];
    let mut current = LaneFrame::new(&setups);
    let mut spare = LaneFrame::new(&setups);
    let mut samples = 0;
    let mut rasterised = 0;
    let mut events = 0;
    let mut ingest_time = Duration::ZERO;
    let mut reset_time = Duration::ZERO;
    let mut resets = 0;
    let mut pass = 0_u64;
    // Source generation and initial frame allocation are outside the ingest measurement.
    let started = Instant::now();
    let mut frame_started = started;
    // Finish a replay before stopping so its final rising edge has its post-trigger samples.
    while !stop.load(Ordering::Relaxed) {
        let offset = pass as f64 * PERIODS as f64 * period;
        for (time, base) in times.iter_mut().zip(&base_times) {
            *time = offset + base;
        }
        for (chunk_t, chunk_y) in times.chunks(CHUNK).zip(values.chunks(CHUNK)) {
            let before = current.waveforms();
            let ingest_started = Instant::now();
            if config.lanes == 1 {
                for (&t, &y) in chunk_t.iter().zip(chunk_y) {
                    if let Edge::Accepted(_) = acquisition.push(t, &[y], &mut current.lanes) { events += 1; }
                }
            } else {
                for (&t, &y) in chunk_t.iter().zip(chunk_y) {
                    point.fill(y);
                    if let Edge::Accepted(_) = acquisition.push(t, &point, &mut current.lanes) { events += 1; }
                }
            }
            ingest_time += ingest_started.elapsed();
            samples += chunk_t.len() as u64;
            rasterised += current.waveforms() - before;

            let now = Instant::now();
            if now.duration_since(frame_started) >= config.frame_interval {
                mem::swap(&mut current, &mut spare);
                // Make every completed frame observable, even though only the last is retained.
                black_box(&spare);
                let reset_started = Instant::now();
                current.clear();
                reset_time += reset_started.elapsed();
                resets += 1;
                frame_started = now;
            }
        }
        pass += 1;
    }
    let elapsed = started.elapsed();
    black_box(&current);
    WorkerResult {
        elapsed,
        samples,
        events,
        waveforms: rasterised,
        ingest_time,
        reset_time,
        resets,
        // Prefer a full display frame; a very short run may only have a partial frame.
        frame: if resets == 0 { current } else { spare },
    }
}

fn grouped(value: impl ToString) -> String {
    let text = value.to_string();
    let (integer, fraction) = text.split_once('.').unwrap_or((&text, ""));
    let mut result = String::with_capacity(text.len() + integer.len() / 3);
    for (i, ch) in integer.chars().enumerate() {
        if i != 0 && (integer.len() - i) % 3 == 0 {
            result.push(' ');
        }
        result.push(ch);
    }
    if !fraction.is_empty() {
        result.push('.');
        result.push_str(fraction);
    }
    result
}

fn rate(count: u64, elapsed: Duration) -> f64 {
    if elapsed.is_zero() { 0.0 } else { count as f64 / elapsed.as_secs_f64() }
}

fn average_us(elapsed: Duration, count: u64) -> String {
    if count == 0 { "n/a".into() } else { grouped(format!("{:.2}", elapsed.as_secs_f64() * 1e6 / count as f64)) }
}

fn run() -> Result<bool, String> {
    let config = Config::parse()?;
    println!(
        "phosphor ingest: {} lanes, {} threads, {} points a waveform, {} x {}, frames every {} ms",
        grouped(config.lanes),
        grouped(config.threads),
        grouped(config.points),
        grouped(config.w),
        grouped(config.h),
        grouped(config.frame_interval.as_secs_f64() * 1000.0),
    );
    let stop = AtomicBool::new(false);
    let results = thread::scope(|scope| {
        let mut handles = Vec::new();
        let mut failure = None;
        for index in 0..config.threads {
            let stop = &stop;
            match thread::Builder::new().spawn_scoped(scope, move || worker(index, config, stop)) {
                Ok(handle) => handles.push(handle),
                Err(error) => {
                    failure = Some(format!("could not start thread {index}: {error}"));
                    break;
                }
            }
        }
        if failure.is_none() {
            thread::sleep(config.seconds);
        }
        stop.store(true, Ordering::Relaxed);
        let mut results = Vec::with_capacity(handles.len());
        for (index, handle) in handles.into_iter().enumerate() {
            match handle.join() {
                Ok(result) => results.push(result),
                Err(_) => failure = Some(format!("thread {index} panicked")),
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(results),
        }
    })?;

    let mut merged = LaneFrame { generation: 0, published: None, spectra: (0..config.lanes).map(|_| None).collect(), lanes: (0..config.lanes).map(|_| fab_elec_view::instruments::phosphor::Hits::new(config.w, config.h)).collect() };
    let mut merge_time = Duration::ZERO;
    for _ in 0..MERGE_REPETITIONS {
        // Measure only summing the frames; destination allocation and reset are excluded.
        merged.clear();
        let merge_started = Instant::now();
        for result in &results {
            merged.add(black_box(&result.frame));
        }
        black_box(&merged);
        merge_time += merge_started.elapsed();
    }

    let mut total_rate = 0.0;
    let mut total_sample_rate = 0.0;
    let mut total_events = 0_u128;
    let mut total_waveforms = 0_u128;
    let mut equal = true;
    for (index, result) in results.iter().enumerate() {
        let waveform_rate = rate(result.waveforms, result.elapsed);
        let sample_rate = rate(result.samples, result.elapsed);
        total_rate += waveform_rate;
        total_sample_rate += sample_rate;
        total_events += u128::from(result.events);
        total_waveforms += u128::from(result.waveforms);
        equal &= result.events == result.waveforms;
        let reset = if result.resets == 0 {
            "n/a (no closed frames)".into()
        } else {
            format!("{} ms", grouped(format!("{:.2}", result.reset_time.as_secs_f64() * 1000.0 / result.resets as f64)))
        };
        println!(
            "  thread {}: {} waveforms/s ({} M samples/s), ingest {} ns a sample ({} us a waveform), frame reset {}",
            grouped(index),
            grouped(format!("{waveform_rate:.0}")),
            grouped(format!("{:.1}", sample_rate / 1e6)),
            if result.samples == 0 { "n/a".into() } else { grouped(format!("{:.2}", result.ingest_time.as_secs_f64() * 1e9 / result.samples as f64)) },
            average_us(result.ingest_time, result.waveforms),
            reset,
        );
        println!(
            "    events: {}; waveforms rasterised: {} ({})",
            grouped(result.events),
            grouped(result.waveforms),
            if result.events == result.waveforms { "equal" } else { "MISMATCH" },
        );
    }
    println!(
        "  total: {} waveforms/s, {} M samples/s; merging {} frames: {} ms",
        grouped(format!("{total_rate:.0}")),
        grouped(format!("{:.0}", total_sample_rate / 1e6)),
        grouped(results.len()),
        grouped(format!("{:.1}", merge_time.as_secs_f64() * 1000.0 / f64::from(MERGE_REPETITIONS))),
    );
    println!(
        "  events: {}; waveforms rasterised: {} ({})",
        grouped(total_events),
        grouped(total_waveforms),
        if equal { "equal" } else { "MISMATCH: every accepted event must be rasterised" },
    );
    Ok(equal)
}

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("phosphor ingest: {error}");
            eprintln!("usage: phosphor_ingest [--lanes K] [--threads N] [--seconds S] [--points P] [--w W] [--h H] [--frame-ms MS]");
            ExitCode::FAILURE
        }
    }
}
