use fab_elec_bench::edges::Threshold;
use fab_elec_bench::uart::{self, Frame, FrameError, Parity, UartDecoder, UartFormat};
#[path = "support/serial.rs"] mod serial;

#[test]
fn uart_decodes_jitter_slopes_noise_and_errors() {
    for baud in [9600.0, 115200.0, 1_000_000.0] {
        for (bits, parity, invert) in [(8, Parity::None, false), (7, Parity::Even, true), (9, Parity::Odd, false)] {
            let format = UartFormat { baud, bits, parity, stop: 1.0, invert, threshold: Threshold { level: 0.5, hysteresis: 0.1 } };
            let values = [0x55, 0x00, 0x7F, 0x41];
            let (t, y, starts) = serial::record(format, &values, None, None);
            let frames = uart::decode(&t, &y, format);
            assert_eq!(frames.len(), values.len(), "{baud}/{bits}: every start produces one frame");
            for ((frame, value), start) in frames.iter().zip(values).zip(starts) {
                assert_eq!(frame.value, value, "{baud}/{bits}: LSB-first byte value");
                assert_eq!(frame.error, None, "{baud}/{bits}: clean frame has no error");
                assert!((frame.t0 - start).abs() < 0.05 / baud, "{baud}/{bits}: t0 is the start threshold crossing");
                let length = 1.0 + bits as f64 + f64::from(parity != Parity::None) + 0.5;
                assert_eq!(frame.t1, frame.t0 + length / baud, "{baud}/{bits}: stop centre is anchored to t0");
            }
            let mut decoder = UartDecoder::new(format);
            for _ in 0..2 {
                let mut streamed = Vec::new();
                for (t, y) in t.chunks(23).zip(y.chunks(23)) {
                    for (&t, &y) in t.iter().zip(y) {
                        if let Some(frame) = decoder.push(t, y as f64) { streamed.push(frame); }
                    }
                }
                assert_eq!(streamed, frames, "{baud}/{bits}: chunking and reset preserve every frame bit");
                decoder.reset();
            }
            let (t, y, _) = serial::record(format, &values, Some(3), Some(0));
            let errors = uart::decode(&t, &y, format);
            assert_eq!(errors.len(), 4, "{baud}/{bits}: idle re-arms after an error");
            assert_eq!(errors[0].error, Some(FrameError::Framing), "{baud}/{bits}: low stop is framing");
            assert_eq!(errors[3].error, if parity == Parity::None { None } else { Some(FrameError::Parity) }, "{baud}/{bits}: flipped parity is detected");
        }
    }
    let format = UartFormat { baud: 9600.0, bits: 8, parity: Parity::None, stop: 1.0, invert: false, threshold: Threshold { level: 0.5, hysteresis: 0.1 } };
    for stop in [1.0, 1.5, 2.0] {
        let format = UartFormat { stop, ..format };
        let (t, y, _) = serial::record(format, &[0, 65], None, Some(0));
        let frames = uart::decode(&t, &y, format);
        assert_eq!(frames[0].error, Some(FrameError::Break), "{stop} stops: all-low data and stop is break");
        assert_eq!(frames[1].value, 65, "{stop} stops: recovery after break");
        assert_eq!(frames[1].t1, frames[1].t0 + (9.0 + stop * 0.5) / format.baud, "stop interval centre respects format");
    }
    let frame = |value, error| Frame { t0: 0.0, t1: 1.0, value, error };
    assert_eq!(uart::text(&[frame(65, None), frame(10, None), frame(255, None), frame(0, Some(FrameError::Break))]), "A\\x0A\\xFF\u{FFFD}", "text renders ASCII, escaped bytes and frame errors");
}

#[test]
fn uart_centres_use_edge_time_and_wait_for_confirmation() {
    let format = UartFormat { baud: 1.0, bits: 5, parity: Parity::None, stop: 1.0, invert: false, threshold: Threshold { level: 0.5, hysteresis: 0.2 } };
    // Start at t=1. The first data centre is t=2.5; a rising edge is
    // interpolated to 2.55, although both are resolved by the sample at 2.8.
    let mut decoder = UartDecoder::new(format);
    let samples = [(0.0, 1.0), (0.8, 1.0), (1.2, 0.0), (2.3, 0.0), (2.8, 1.0), (7.6, 1.0)];
    let frames: Vec<_> = samples.into_iter().filter_map(|(t, y)| decoder.push(t, y)).collect();
    assert_eq!(frames.len(), 1, "sparse samples complete one frame");
    assert_eq!(frames[0].value, 0b11110, "a later edge cannot change an earlier bit centre");
    assert_eq!(frames[0].t0, 1.0, "start anchors all centres");
    assert_eq!(frames[0].t1, 7.5, "completion uses the stop centre rather than the sample time");
    assert_eq!(frames[0].error, None, "high stop is valid");
    decoder.reset();
    // The threshold crossed at 2.3, before the data centre, but confirmation
    // waits until 2.8. The intermediate sample must not freeze that bit low.
    let samples = [(0.0, 1.0), (0.8, 1.0), (1.2, 0.0), (2.0, 0.4), (2.6, 0.6 - 0.000001), (2.8, 1.0), (7.6, 1.0)];
    let frames: Vec<_> = samples.into_iter().filter_map(|(t, y)| decoder.push(t, y)).collect();
    assert_eq!(frames[0].value, 0b11111, "pending hysteresis crossing defers a centre until its state is known");
    assert_eq!(frames[0].error, None, "deferred centre preserves framing");
}

#[test]
fn uart_estimates_closed_form_rates_and_reports_clock_error() {
    let threshold = Threshold { level: 0.5, hysteresis: 0.1 };
    let mut levels = vec![true; 3];
    for &byte in b"Hello\n" {
        levels.push(false);
        for bit in 0..8 { levels.push(byte & (1 << bit) != 0); }
        levels.push(true);
        levels.extend([true; 3]);
    }
    for nominal in [9600.0, 115200.0, 1_000_000.0] {
        for speed in [1.0, 1.02] {
            let rate = nominal * speed;
            // Closed-form rectangular 8N1, independent of the decoder and edge detector.
            let t: Vec<_> = (0..levels.len() * 16).map(|i| i as f64 / (rate * 16.0)).collect();
            let y: Vec<_> = (0..t.len()).map(|i| if levels[i / 16] { 1.0 } else { 0.0 }).collect();
            let edges = uart::edge_times(&t, &y, threshold);
            let estimate = uart::estimate_baud(&edges).expect("Hello has enough edges to estimate baud");
            assert_eq!(estimate.nominal, Some(nominal), "{nominal}/{speed}: nearest standard rate is exact");
            assert!((estimate.measured / rate - 1.0).abs() < 0.005, "{nominal}/{speed}: measured baud is within half a percent of the line rate");
            assert!(estimate.bits >= 8, "{nominal}/{speed}: estimate rests on at least eight bit times");
            if speed != 1.0 {
                assert!(estimate.measured > nominal * 1.015, "{nominal}/{speed}: reported measurement retains the fast clock rather than snapping");
            }
            assert_eq!(uart::estimate_baud(&edges[..8]), None, "{nominal}/{speed}: seven intervals are too few to estimate baud");
        }
    }
    assert_eq!(uart::estimate_baud(&[]), None, "DC has no intervals to estimate baud");
    // A rounded shortest interval does not determine b0; long idle is not a bit multiple.
    let intervals = [0.9, 1.1, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 37.25];
    let mut edges = vec![0.0];
    for dt in intervals { edges.push(edges.last().unwrap() + dt); }
    let estimate = uart::estimate_baud(&edges).expect("eight short intervals support an estimate");
    assert_eq!(estimate.bits, 8, "long idle contributes no bit times to refinement");
    assert!((estimate.measured - 1.0).abs() < 1e-12, "median seed and refinement tolerate a rounded shortest interval");
    assert_eq!(estimate.nominal, None, "nonstandard rate has no nominal baud");
}
