// Ported from flutter/flutter packages/flutter/lib/src/gestures/lsq_solver.dart @ 3.50.1 (ae9b586f34a8443573f82ef57c3bf914a908cbf7).
// Copyright 2014 The Flutter Authors. BSD-3-Clause, see third_party/flutter/LICENSE.

use super::physics::precisionErrorTolerance;

// TODO(abarth): Consider using vector_math.
struct Vector<'a> {
    offset: usize,
    length: usize,
    elements: &'a [f64],
}

impl Vector<'_> {
    fn get(&self, i: usize) -> f64 {
        self.elements[i + self.offset]
    }

    fn dot(&self, a: &Vector) -> f64 {
        let mut result = 0.0;
        for i in 0..self.length {
            result += self.get(i) * a.get(i);
        }
        result
    }

    fn norm(&self) -> f64 {
        self.dot(self).sqrt()
    }
}

// TODO(abarth): Consider using vector_math.
struct Matrix {
    columns: usize,
    elements: Vec<f64>,
}

impl Matrix {
    fn new(rows: usize, cols: usize) -> Self {
        Self {
            columns: cols,
            elements: vec![0.0; rows * cols],
        }
    }

    fn get(&self, row: usize, col: usize) -> f64 {
        self.elements[row * self.columns + col]
    }

    fn set(&mut self, row: usize, col: usize, value: f64) {
        self.elements[row * self.columns + col] = value;
    }

    fn get_row(&self, row: usize) -> Vector<'_> {
        Vector {
            offset: row * self.columns,
            length: self.columns,
            elements: &self.elements,
        }
    }
}

/// An nth degree polynomial fit to a dataset.
pub(crate) struct PolynomialFit {
    /// The polynomial coefficients of the fit.
    ///
    /// For each `i`, the element `coefficients[i]` is the coefficient of
    /// the `i`-th power of the variable.
    pub(crate) coefficients: Vec<f64>,

    /// An indicator of the quality of the fit.
    ///
    /// Larger values indicate greater quality.  The value ranges from 0.0 to 1.0.
    ///
    /// The confidence is defined as the fraction of the dataset's variance
    /// that is captured by variance in the fit polynomial.  In statistics
    /// textbooks this is often called "r-squared".
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "Flutter's API; only its tests read it")
    )]
    pub(crate) confidence: f64,
}

/// Uses the least-squares algorithm to fit a polynomial to a set of data.
pub(crate) struct LeastSquaresSolver<'a> {
    /// The x-coordinates of each data point.
    pub(crate) x: &'a [f64],

    /// The y-coordinates of each data point.
    pub(crate) y: &'a [f64],

    /// The weight to use for each data point.
    pub(crate) w: &'a [f64],
}

impl LeastSquaresSolver<'_> {
    /// Fits a polynomial of the given degree to the data points.
    ///
    /// When there is not enough data to fit a curve None is returned.
    pub(crate) fn solve(&self, degree: usize) -> Option<PolynomialFit> {
        let (x, y, w) = (self.x, self.y, self.w);
        debug_assert!(x.len() == y.len() && y.len() == w.len());
        if degree > x.len() {
            // Not enough data to fit a curve.
            return None;
        }

        let mut coefficients = vec![0.0; degree + 1];

        // Shorthands for the purpose of notation equivalence to original C++ code.
        let m = x.len();
        let n = degree + 1;

        // Expand the X vector to a matrix A, pre-multiplied by the weights.
        let mut a = Matrix::new(n, m);
        for h in 0..m {
            a.set(0, h, w[h]);
            for i in 1..n {
                a.set(i, h, a.get(i - 1, h) * x[h]);
            }
        }

        // Apply the Gram-Schmidt process to A to obtain its QR decomposition.

        // Orthonormal basis, column-major ordVectorer.
        let mut q = Matrix::new(n, m);
        // Upper triangular matrix, row-major order.
        let mut r = Matrix::new(n, n);
        for j in 0..n {
            for h in 0..m {
                q.set(j, h, a.get(j, h));
            }
            for i in 0..j {
                let dot = q.get_row(j).dot(&q.get_row(i));
                for h in 0..m {
                    q.set(j, h, q.get(j, h) - dot * q.get(i, h));
                }
            }

            let norm = q.get_row(j).norm();
            if norm < precisionErrorTolerance {
                // Vectors are linearly dependent or zero so no solution.
                return None;
            }

            let inverse_norm = 1.0 / norm;
            for h in 0..m {
                q.set(j, h, q.get(j, h) * inverse_norm);
            }
            for i in 0..n {
                let value = if i < j {
                    0.0
                } else {
                    q.get_row(j).dot(&a.get_row(i))
                };
                r.set(j, i, value);
            }
        }

        // Solve R B = Qt W Y to find B. This is easy because R is upper triangular.
        // We just work from bottom-right to top-left calculating B's coefficients.
        let wy: Vec<f64> = (0..m).map(|h| y[h] * w[h]).collect();
        let wy = Vector {
            offset: 0,
            length: m,
            elements: &wy,
        };
        for i in (0..n).rev() {
            coefficients[i] = q.get_row(i).dot(&wy);
            for j in (i + 1..n).rev() {
                coefficients[i] -= r.get(i, j) * coefficients[j];
            }
            coefficients[i] /= r.get(i, i);
        }

        // Calculate the coefficient of determination (confidence) as:
        //   1 - (sumSquaredError / sumSquaredTotal)
        // ...where sumSquaredError is the residual sum of squares (variance of the
        // error), and sumSquaredTotal is the total sum of squares (variance of the
        // data) where each has been weighted.
        let mut y_mean = 0.0;
        for h in 0..m {
            y_mean += y[h];
        }
        y_mean /= m as f64;

        let mut sum_squared_error = 0.0;
        let mut sum_squared_total = 0.0;
        for h in 0..m {
            let mut term = 1.0;
            let mut err = y[h] - coefficients[0];
            for i in 1..n {
                term *= x[h];
                err -= term * coefficients[i];
            }
            sum_squared_error += w[h] * w[h] * err * err;
            let v = y[h] - y_mean;
            sum_squared_total += w[h] * w[h] * v * v;
        }

        let confidence = if sum_squared_total <= precisionErrorTolerance {
            1.0
        } else {
            1.0 - (sum_squared_error / sum_squared_total)
        };

        Some(PolynomialFit {
            coefficients,
            confidence,
        })
    }
}
