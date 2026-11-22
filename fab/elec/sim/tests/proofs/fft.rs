//! Transform, window, and transient-to-harmonic proofs using analytic references.

use fab_elec_sim::fft::*;
use fab_elec_sim::num::C64;
use std::f64::consts::{PI, TAU};

#[test]
fn fft_transform_properties() {
    let n = 4096;
    let mut seed = 0x4d59_5df4_d0f3_3173_u64;
    let mut random = || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        2.0 * ((seed >> 11) as f64 / (1_u64 << 53) as f64) - 1.0
    };
    let input: Vec<_> = (0..n).map(|_| C64::new(random(), random())).collect();
    let maximum = input.iter().map(|x| x.norm()).fold(0.0_f64, f64::max);
    let energy = input.iter().map(|x| x.norm_sqr()).sum::<f64>();
    let mut transformed = input.clone();
    assert_eq!(
        fft(&mut transformed, false),
        Ok(()),
        "forward FFT accepts 4096 points"
    );
    let spectral_energy = transformed.iter().map(|x| x.norm_sqr()).sum::<f64>();
    let parseval_error = (spectral_energy / (n as f64 * energy) - 1.0).abs();
    assert!(
        parseval_error < 1e-11,
        "Parseval relative error: {parseval_error:e}"
    );
    assert_eq!(
        fft(&mut transformed, true),
        Ok(()),
        "inverse FFT accepts 4096 points"
    );
    let roundtrip_error = transformed
        .iter()
        .zip(&input)
        .map(|(&actual, &expected)| (actual - expected).norm())
        .fold(0.0_f64, f64::max)
        / maximum;
    assert!(
        roundtrip_error < 1e-12,
        "4096-point roundtrip relative error: {roundtrip_error:e}"
    );

    let mut tone: Vec<_> = (0..n)
        .map(|j| C64::polar(1.0, TAU * 37.0 * j as f64 / n as f64))
        .collect();
    assert_eq!(fft(&mut tone, false), Ok(()), "complex tone FFT succeeds");
    assert!(
        (tone[37].norm() / n as f64 - 1.0).abs() < 1e-12,
        "complex exponential has magnitude n at bin 37"
    );
    let leakage = tone
        .iter()
        .enumerate()
        .filter(|&(k, _)| k != 37)
        .map(|(_, value)| value.norm())
        .fold(0.0_f64, f64::max);
    assert!(
        leakage < 1e-9 * n as f64,
        "complex exponential out-of-bin leakage: {leakage:e}"
    );

    let small = &input[..64];
    let direct: Vec<_> = (0..64)
        .map(|k| {
            small
                .iter()
                .enumerate()
                .fold(C64::new(0.0, 0.0), |sum, (j, &x)| {
                    sum + x * C64::polar(1.0, -TAU * (j * k) as f64 / 64.0)
                })
        })
        .collect();
    let mut fast = small.to_vec();
    assert_eq!(fft(&mut fast, false), Ok(()), "64-point FFT succeeds");
    let direct_scale = direct.iter().map(|x| x.norm()).fold(0.0_f64, f64::max);
    let direct_error = fast
        .iter()
        .zip(&direct)
        .map(|(&a, &b)| (a - b).norm())
        .fold(0.0_f64, f64::max)
        / direct_scale;
    assert!(
        direct_error < 1e-12,
        "direct DFT relative agreement: {direct_error:e}"
    );

    let mut invalid = [C64::new(1.0, -2.0); 6];
    assert_eq!(
        fft(&mut invalid, false),
        Err(FftLenError(6)),
        "length six is rejected"
    );
    assert_eq!(
        invalid,
        [C64::new(1.0, -2.0); 6],
        "invalid FFT leaves input unchanged"
    );
    assert_eq!(
        fft(&mut [], false),
        Err(FftLenError(0)),
        "empty FFT is rejected"
    );
    let mut singleton = [C64::new(3.0, -4.0)];
    for inverse in [false, true] {
        assert_eq!(
            fft(&mut singleton, inverse),
            Ok(()),
            "length one is accepted"
        );
        assert_eq!(
            singleton,
            [C64::new(3.0, -4.0)],
            "length one is the identity"
        );
    }
    assert_eq!(
        [next_pow2(0), next_pow2(1), next_pow2(6), next_pow2(4096)],
        [1, 1, 8, 4096],
        "FFT length rounding"
    );

    // Exercise the long-length accuracy requirement without another test setup.
    let long_input: Vec<_> = (0..65536).map(|_| C64::new(random(), random())).collect();
    let mut long = long_input.clone();
    assert_eq!(
        fft(&mut long, false),
        Ok(()),
        "65536-point forward FFT succeeds"
    );
    assert_eq!(
        fft(&mut long, true),
        Ok(()),
        "65536-point inverse FFT succeeds"
    );
    let long_scale = long_input.iter().map(|x| x.norm()).fold(0.0_f64, f64::max);
    let long_error = long
        .iter()
        .zip(&long_input)
        .map(|(&a, &b)| (a - b).norm())
        .fold(0.0_f64, f64::max)
        / long_scale;
    assert!(
        long_error < 1e-12,
        "65536-point roundtrip relative error: {long_error:e}"
    );
}

#[test]
fn window_and_spectrum_properties() {
    let n = 4096;
    for (window, expected) in [
        (Window::Rect, 1.0),
        (Window::Hann, 0.5),
        (Window::Hamming, 0.54),
        (Window::BlackmanHarris, 0.35875),
        (Window::FlatTop, 0.21557895),
    ] {
        let gain = window.coherent_gain(n);
        assert!(
            (gain - expected).abs() < 1e-9,
            "{window:?} coherent gain: {gain}"
        );
    }
    assert_eq!(
        Window::Rect.enbw_bins(n),
        1.0,
        "rectangular ENBW is exactly one bin"
    );
    assert!(
        (Window::Hann.enbw_bins(n) - 1.5).abs() < 1e-9,
        "Hann ENBW is 1.5 bins"
    );

    let dt = 1e-5;
    let samples: Vec<_> = (0..n)
        .map(|j| 0.7 * (TAU * 100.0 * j as f64 / n as f64 + 0.3).cos())
        .collect();
    for window in [Window::Rect, Window::Hann] {
        let measured = spectrum(&samples, dt, window);
        assert_eq!(
            measured.amp.len(),
            n / 2 + 1,
            "single-sided amplitude bin count"
        );
        assert_eq!(
            measured.phase.len(),
            measured.amp.len(),
            "spectrum phase bin count"
        );
        assert!(
            (measured.df - 1.0 / (n as f64 * dt)).abs() < 1e-12,
            "spectrum frequency spacing"
        );
        assert!(
            (measured.amp[100] - 0.7).abs() < 1e-9,
            "{window:?} bin-centred peak amplitude: {}",
            measured.amp[100]
        );
        assert!(
            (measured.phase[100] - 0.3).abs() < 1e-9,
            "{window:?} cosine phase: {}",
            measured.phase[100]
        );
        assert_eq!(
            measured.phase[300], 0.0,
            "{window:?} negligible-bin phase is zero"
        );
    }
    let dc = spectrum(&vec![0.25; n], dt, Window::Rect);
    assert_eq!(dc.amp[0], 0.25, "DC amplitude is not doubled");
    let nyquist: Vec<_> = (0..n)
        .map(|j| if j % 2 == 0 { 0.7 } else { -0.7 })
        .collect();
    let nyquist = spectrum(&nyquist, dt, Window::Rect);
    assert!(
        (nyquist.amp[n / 2] - 0.7).abs() < 1e-12,
        "Nyquist amplitude is not doubled"
    );

    let half_bin: Vec<_> = (0..n)
        .map(|j| 0.7 * (TAU * 100.5 * j as f64 / n as f64 + 0.3).cos())
        .collect();
    let flat = spectrum(&half_bin, dt, Window::FlatTop);
    let rect = spectrum(&half_bin, dt, Window::Rect);
    for k in [100, 101] {
        let flat_error = (flat.amp[k] / 0.7 - 1.0).abs();
        let rect_error = (rect.amp[k] / 0.7 - 1.0).abs();
        assert!(
            flat_error < 0.0015,
            "flat-top half-bin scalloping at bin {k}: {flat_error:e}"
        );
        assert!(
            rect_error > 0.30,
            "rectangular half-bin scalloping at bin {k}: {rect_error:e}"
        );
    }

    // Three quarters of the padded record, with an integer number of cycles
    // in the original record: the opposite-frequency image is orthogonal.
    let padded_samples: Vec<_> = (0..3072)
        .map(|j| 0.7 * (TAU * 100.0 * j as f64 / n as f64 + 0.3).cos())
        .collect();
    let padded = spectrum(&padded_samples, dt, Window::Rect);
    assert_eq!(
        padded.amp.len(),
        n / 2 + 1,
        "non-power-of-two record is zero-padded"
    );
    assert!(
        (padded.amp[100] - 0.7).abs() < 1e-12,
        "padding preserves original-record amplitude normalization"
    );
    assert!(
        (padded.phase[100] - 0.3).abs() < 1e-12,
        "padding preserves orthogonal tone phase"
    );

    // Band RMS over 20 ms of 48 kHz audio (960 samples, padded to 1024) under Hann: DC 0.25 with
    // tones of 1 and 0.5 at 1 kHz and 8 kHz. Each band reads only its own content, and both
    // bands together read the whole record's RMS.
    let rate = 48_000.0;
    let audio: Vec<_> = (0..960)
        .map(|j| {
            let t = j as f64 / rate;
            0.25 + (TAU * 1000.0 * t).cos() + 0.5 * (TAU * 8000.0 * t + 0.3).cos()
        })
        .collect();
    let band = |lo, hi| band_rms(&audio, 1.0 / rate, Window::Hann, lo, hi);
    for (lo, hi, want) in [
        (0.0, 4000.0, (0.0625_f64 + 0.5).sqrt()),
        (4000.0, 24_000.0, 0.5 / 2.0_f64.sqrt()),
        (0.0, 24_000.0, (0.0625_f64 + 0.5 + 0.125).sqrt()),
    ] {
        let got = band(lo, hi);
        assert!(
            (got / want - 1.0).abs() < 1e-3,
            "band RMS {lo}..{hi} Hz: {got:.6}, want {want:.6}"
        );
    }
    assert_eq!(
        band_rms(&[], 1.0 / rate, Window::Hann, 0.0, 24_000.0),
        0.0,
        "an empty record reads zero"
    );
}

#[test]
fn fourier_resample_properties() {
    let mut times = vec![0.0];
    for i in 0..39 {
        times.push(times.last().copied().unwrap() + (1 + i % 5) as f64);
    }
    let end = *times.last().unwrap();
    for t in &mut times {
        *t /= end;
    }
    let cubic = |t: f64| 1.0 - 2.0 * t + 0.5 * t * t + 3.0 * t * t * t;
    let derivative = |t: f64| -2.0 + t + 9.0 * t * t;
    let quadratic = |t: f64| 1.0 - 2.0 * t + 0.5 * t * t;
    let linear = |t: f64| -0.25 + 1.75 * t;
    let y: Vec<_> = times.iter().map(|&t| cubic(t)).collect();
    let dy: Vec<_> = times.iter().map(|&t| derivative(t)).collect();
    let q: Vec<_> = times.iter().map(|&t| quadratic(t)).collect();
    let l: Vec<_> = times.iter().map(|&t| linear(t)).collect();
    let dt = 1.0 / 999.0;
    let cubic_samples = resample_hermite(&times, &y, Some(&dy), 0.0, dt, 1000);
    let quadratic_samples = resample_hermite(&times, &q, None, 0.0, dt, 1000);
    let linear_samples = resample_hermite(&times, &l, None, 0.0, dt, 1000);
    for k in 0..1000 {
        let t = k as f64 * dt;
        assert!(
            (cubic_samples[k] - cubic(t)).abs() < 1e-12,
            "Hermite cubic reproduction at target {k}"
        );
        assert!(
            (quadratic_samples[k] - quadratic(t)).abs() < 1e-12,
            "estimated parabola derivatives reproduce quadratic at target {k}"
        );
        assert!(
            (linear_samples[k] - linear(t)).abs() < 1e-12,
            "linear reproduction including end intervals at target {k}"
        );
    }
    let clamped = resample_hermite(&times, &y, Some(&dy), -0.25, 1.5, 2);
    assert_eq!(
        clamped,
        vec![y[0], y[y.len() - 1]],
        "resampling clamps outside the record"
    );
    let secant = resample_hermite(&[0.0, 1.0], &[linear(0.0), linear(1.0)], None, 0.0, 0.25, 5);
    for (k, value) in secant.into_iter().enumerate() {
        assert!(
            (value - linear(k as f64 * 0.25)).abs() < 1e-12,
            "two-point secant at target {k}"
        );
    }

    // E3 T18: three periods, starting a quarter period before a rising edge.
    // Each discontinuity has its own narrow straddling interval; every other
    // interval is constant. Both endpoints are in the negative half-cycle.
    let period = 1e-3;
    let epsilon = period * 1e-9;
    let mut square_t = vec![-0.25 * period];
    let mut square_y = vec![-1.0];
    for edge in 0..6 {
        let jump = edge as f64 * 0.5 * period;
        let after = if edge % 2 == 0 { 1.0 } else { -1.0 };
        square_t.extend([
            jump - epsilon,
            jump + epsilon,
            jump + 0.07 * period,
            jump + 0.19 * period,
        ]);
        square_y.extend([-after, after, after, after]);
    }
    square_t.push(2.75 * period);
    square_y.push(-1.0);
    assert!(
        square_t.windows(2).all(|pair| pair[0] < pair[1]),
        "square-wave times are strictly increasing"
    );
    let square_dy = vec![0.0; square_t.len()];
    let square = fourier(&square_t, &square_y, Some(&square_dy), 1000.0, 9, 4096);
    // The 4096-point grid hits the two jumps at their Hermite midpoint (zero).
    // Its odd amplitudes are (4/N)*cot(pi*h/N), rather than 4/(pi*h).
    // The relative deficit is 1-z*cot(z) ~ z^2/3, at most 1.59e-5 for h<=9.
    // Use 2e-5 for odd amplitudes and THD. Even bins and DC cancel analytically;
    // 1e-8 allows roundoff when locating a midpoint in a 2e-12-second interval.
    for h in 1..=9 {
        if h % 2 == 1 {
            let expected = 4.0 / (PI * h as f64);
            let error = (square.amp[h] / expected - 1.0).abs();
            assert!(
                error < 2e-5,
                "T18 square-wave odd harmonic {h} relative error: {error:e}"
            );
        } else {
            assert!(
                square.amp[h] < 1e-8,
                "T18 square-wave even harmonic {h}: {}",
                square.amp[h]
            );
        }
    }
    assert!(square.dc.abs() < 1e-8, "T18 square-wave DC: {}", square.dc);
    let expected_thd = (3..=9)
        .step_by(2)
        .map(|h| (4.0 / (PI * h as f64)).powi(2))
        .sum::<f64>()
        .sqrt()
        / (4.0 / PI);
    assert!(
        (square.thd() / expected_thd - 1.0).abs() < 2e-5,
        "T18 square-wave truncated THD: {} versus {expected_thd}",
        square.thd()
    );

    // Dense nonuniform samples make cubic interpolation error O((w*dt)^4).
    // A fractional-period endpoint also exercises phase referenced to time zero.
    let mut tone_t = vec![0.0];
    for i in 0..2400 {
        tone_t.push(tone_t.last().copied().unwrap() + (1 + i % 5) as f64);
    }
    let span = *tone_t.last().unwrap();
    for t in &mut tone_t {
        *t *= 3.125 * period / span;
    }
    let w = TAU / period;
    let tone_y: Vec<_> = tone_t
        .iter()
        .map(|&t| 2.0 * (w * t).cos() + 0.1 * (3.0 * w * t + 0.5).cos())
        .collect();
    let tone_dy: Vec<_> = tone_t
        .iter()
        .map(|&t| -2.0 * w * (w * t).sin() - 0.3 * w * (3.0 * w * t + 0.5).sin())
        .collect();
    let harmonics = fourier(&tone_t, &tone_y, Some(&tone_dy), 1000.0, 9, 4096);
    assert!(
        (harmonics.thd() - 0.05).abs() < 1e-6,
        "two-tone THD: {}",
        harmonics.thd()
    );
    assert!(
        (harmonics.phase[3] - 0.5).abs() < 1e-6,
        "third-harmonic absolute cosine phase: {}",
        harmonics.phase[3]
    );
    assert!(
        (harmonics.amp[1] - 2.0).abs() < 1e-6,
        "fundamental peak amplitude: {}",
        harmonics.amp[1]
    );
    assert!(
        (harmonics.amp[3] - 0.1).abs() < 1e-6,
        "third-harmonic peak amplitude: {}",
        harmonics.amp[3]
    );
    assert_eq!(
        harmonics.amp[0], 0.0,
        "harmonics amplitude index zero is reserved"
    );
    assert_eq!(
        harmonics.phase[0], 0.0,
        "harmonics phase index zero is reserved"
    );
    let dc = fourier(&[0.0, period], &[-0.25, -0.25], None, 1000.0, 3, 3);
    assert_eq!(
        dc.dc, -0.25,
        "Fourier DC is signed and undersized grids are rounded up"
    );
    assert_eq!(dc.thd(), 0.0, "THD is zero without a fundamental");
}
