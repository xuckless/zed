// Ported from flutter/flutter packages/flutter/test/gestures/lsq_solver_test.dart @ 3.50.1 (ae9b586f34a8443573f82ef57c3bf914a908cbf7).
// Copyright 2014 The Flutter Authors. BSD-3-Clause, see third_party/flutter/LICENSE.

use super::lsq_solver::LeastSquaresSolver;

fn approx(value: f64, expectation: f64) -> bool {
    const EPS: f64 = 1e-6;
    (value - expectation).abs() < EPS
}

const X: [f64; 9] = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
const W: [f64; 9] = [1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0];
const LINE: [f64; 9] = [1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0];
const SLOPED_LINE: [f64; 9] = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0];

/// Fits of `y` over `X` (weights `W`) at a degree: the expected coefficients and confidence.
/// To add one: a row with the Flutter test's name, `y`, the degree and the expected fit.
const FITS: &[(&str, &[f64], usize, &[f64], f64)] = &[
    (
        "Least-squares fit: linear polynomial to line",
        &LINE,
        1,
        &[1.0, 0.0],
        1.0,
    ),
    (
        "Least-squares fit: linear polynomial to sloped line",
        &SLOPED_LINE,
        1,
        &[1.0, 1.0],
        1.0,
    ),
    (
        "Least-squares fit: quadratic polynomial to line",
        &LINE,
        2,
        &[1.0, 0.0, 0.0],
        1.0,
    ),
    (
        "Least-squares fit: quadratic polynomial to sloped line",
        &SLOPED_LINE,
        2,
        &[1.0, 1.0, 0.0],
        1.0,
    ),
];

#[test]
fn least_squares_fits() {
    for (name, y, degree, coefficients, confidence) in FITS {
        let solver = LeastSquaresSolver { x: &X, y, w: &W };
        let fit = solver.solve(*degree).expect(name);

        assert_eq!(fit.coefficients.len(), coefficients.len(), "{name}");
        for (actual, expected) in fit.coefficients.iter().zip(*coefficients) {
            assert!(approx(*actual, *expected), "{name}: {:?}", fit.coefficients);
        }
        assert!(
            approx(fit.confidence, *confidence),
            "{name}: confidence {}",
            fit.confidence
        );
    }
}
