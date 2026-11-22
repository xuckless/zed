// Ported from flutter/flutter packages/flutter/lib/src/gestures/velocity_tracker.dart @ 3.50.1 (ae9b586f34a8443573f82ef57c3bf914a908cbf7).
// Copyright 2014 The Flutter Authors. BSD-3-Clause, see third_party/flutter/LICENSE.
//
// Flutter's three trackers are one `VelocityTracker` here, its `VelocityTrackerKind` picking the estimate (Flutter's
// subclasses). `Velocity` is GPUI's `Point<f32>` and `Offset` a `Point<f64>`; `VelocityEstimate`'s `confidence` and
// `duration` are dropped (no GPUI caller); its `offset` stays for the recognizer's `isFlingGesture` check.
// Flutter's `_sinceLastSample` stopwatch is `now` minus the newest sample's time: GPUI timestamps samples with the
// clock it passes as `now`.

use scheduler::Instant;

use super::lsq_solver::LeastSquaresSolver;
use crate::{Pixels, Point, point};

/// Which of Flutter's velocity trackers estimates a touch's release velocity.
///
/// Flutter's `ScrollBehavior.velocityTrackerBuilder` picks one per platform: iOS uses
/// [`Self::IOSScrollViewFling`], macOS [`Self::MacOSScrollViewFling`], Android, Fuchsia, Linux and Windows
/// [`Self::LeastSquares`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VelocityTrackerKind {
    /// Flutter's `VelocityTracker`: a least-squares quadratic fit over the
    /// last 100 ms of continuous motion, differentiated at the newest sample.
    LeastSquares,
    /// Flutter's `IOSScrollViewFlingVelocityTracker`: a close approximation of
    /// the velocity an iOS scroll view reports when the touch that initiates a
    /// fling is released, as a weighted average of the latest few velocity
    /// samples.
    IOSScrollViewFling,
    /// Flutter's `MacOSScrollViewFlingVelocityTracker`: the same approach,
    /// weighted as a macOS scroll view is.
    MacOSScrollViewFling,
}

const ASSUME_POINTER_MOVE_STOPPED_MILLISECONDS: u128 = 40;
const HISTORY_SIZE: usize = 20;
const HORIZON_MILLISECONDS: f64 = 100.;
const MIN_SAMPLE_SIZE: usize = 3;

/// Weights of `_previousVelocityAt(-2)`, `(-1)` and `(0)` in the iOS and macOS
/// fling estimates. To add a kind of that shape: one row here and one arm in
/// [`VelocityTracker::get_velocity`].
const IOS_SCROLL_VIEW_FLING_WEIGHTS: [(isize, f64); 3] = [(-2, 0.6), (-1, 0.35), (0, 0.05)];
const MAC_OS_SCROLL_VIEW_FLING_WEIGHTS: [(isize, f64); 3] = [(-2, 0.15), (-1, 0.65), (0, 0.2)];

/// A velocity in two dimensions and the distance it was estimated over.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct VelocityEstimate {
    /// The number of pixels per second of velocity in the x and y directions.
    pub(crate) pixels_per_second: Point<f32>,
    /// The difference between the first and last position sample used
    /// to compute [`Self::pixels_per_second`].
    pub(crate) offset: Point<f32>,
}

#[derive(Clone, Copy, Debug)]
struct PointAtTime {
    time: Instant,
    point: Point<f64>,
}

/// Computes a pointer's velocity based on data from move events.
///
/// The input data is provided by calling [`Self::add_position`]. Adding data
/// is cheap.
///
/// To obtain a velocity, call [`Self::get_velocity`]. This will compute the
/// velocity based on the data added so far. Only call this when you need to
/// use the velocity, as it is comparatively expensive.
///
/// The quality of the velocity estimation will be better if more data points
/// have been received.
pub(crate) struct VelocityTracker {
    kind: VelocityTrackerKind,
    // Circular buffer; current sample at index. `IOSScrollViewFlingVelocityTracker`'s
    // `_touchSamples` (its `_sampleSize` is also 20) is this same buffer: the
    // iOS tracker keeps 20 samples so `VelocityEstimate.offset` is large enough
    // to count as a fling, and adds them the same way.
    samples: [Option<PointAtTime>; HISTORY_SIZE],
    index: usize,
}

impl VelocityTracker {
    /// Create a new velocity tracker of a [`VelocityTrackerKind`].
    pub(crate) fn with_kind(kind: VelocityTrackerKind) -> Self {
        Self {
            kind,
            samples: [None; HISTORY_SIZE],
            index: 0,
        }
    }

    /// Adds a position as the given time to the tracker.
    pub(crate) fn add_position(&mut self, time: Instant, position: Point<Pixels>) {
        if let (
            VelocityTrackerKind::IOSScrollViewFling | VelocityTrackerKind::MacOSScrollViewFling,
            Some(previous_point),
        ) = (self.kind, self.samples[self.index])
        {
            debug_assert!(
                previous_point.time <= time,
                "The position being added ({position:?}) has a smaller timestamp ({time:?}) \
                 than its predecessor: {previous_point:?}."
            );
        }
        self.index = (self.index + 1) % HISTORY_SIZE;
        self.samples[self.index] = Some(PointAtTime {
            time,
            point: position.map(|value| f64::from(f32::from(value))),
        });
    }

    /// Returns an estimate of the velocity of the object being tracked by the
    /// tracker given the current information available to the tracker, in
    /// pixels per second. `now` is the clock the samples were added with.
    ///
    /// Returns zero if there is no data from which to compute an estimate, or
    /// if the pointer has not moved for 40 ms.
    pub(crate) fn get_velocity_estimate(&self, now: Instant) -> VelocityEstimate {
        let Some(newest_sample) = self.samples[self.index] else {
            debug_assert!(
                self.kind == VelocityTrackerKind::LeastSquares,
                "There must be at least 1 point in _touchSamples: {:?}",
                self.samples
            );
            return VelocityEstimate::default();
        };
        // Has user recently moved since last sample?
        if now.duration_since(newest_sample.time).as_millis()
            > ASSUME_POINTER_MOVE_STOPPED_MILLISECONDS
        {
            return VelocityEstimate::default();
        }
        let (pixels_per_second, oldest_sample) = match self.kind {
            VelocityTrackerKind::LeastSquares => self.least_squares_velocity(newest_sample),
            // The velocity estimated using this expression is an approximation of the
            // scroll velocity of an iOS scroll view at the moment the user touch was
            // released, not the final velocity of the iOS pan gesture recognizer
            // installed on the scroll view would report. Typically in an iOS scroll
            // view the velocity values are different between the two, because the
            // scroll view usually slows down when the touch is released.
            VelocityTrackerKind::IOSScrollViewFling => (
                self.weighted_velocity(IOS_SCROLL_VIEW_FLING_WEIGHTS),
                self.oldest_non_null_sample(newest_sample),
            ),
            // The velocity estimated using this expression is an approximation of the
            // scroll velocity of a macOS scroll view at the moment the user touch was
            // released.
            VelocityTrackerKind::MacOSScrollViewFling => (
                self.weighted_velocity(MAC_OS_SCROLL_VIEW_FLING_WEIGHTS),
                self.oldest_non_null_sample(newest_sample),
            ),
        };
        VelocityEstimate {
            pixels_per_second: pixels_per_second.map(|value| value as f32),
            offset: (newest_sample.point - oldest_sample.point).map(|value| value as f32),
        }
    }

    /// Computes the velocity of the pointer at the time of the last provided
    /// data point, in pixels per second (the estimate's `pixels_per_second`).
    pub(crate) fn get_velocity(&self, now: Instant) -> Point<f32> {
        self.get_velocity_estimate(now).pixels_per_second
    }

    /// The velocity and the oldest sample the fit used.
    fn least_squares_velocity(&self, newest_sample: PointAtTime) -> (Point<f64>, PointAtTime) {
        let mut x = Vec::with_capacity(HISTORY_SIZE);
        let mut y = Vec::with_capacity(HISTORY_SIZE);
        let mut w = Vec::with_capacity(HISTORY_SIZE);
        let mut time = Vec::with_capacity(HISTORY_SIZE);
        let mut sample_count = 0;
        let mut index = self.index;

        let mut previous_sample = newest_sample;
        let mut oldest_sample = newest_sample;

        // Starting with the most recent PointAtTime sample, iterate backwards while
        // the samples represent continuous motion.
        while let Some(sample) = self.samples[index] {
            let age = newest_sample.time.duration_since(sample.time).as_micros() as f64 / 1000.;
            // Dart's `.abs()` of `sample.time - previousSample.time`.
            let delta = sample
                .time
                .max(previous_sample.time)
                .duration_since(sample.time.min(previous_sample.time))
                .as_micros() as f64
                / 1000.;
            previous_sample = sample;
            if age > HORIZON_MILLISECONDS || delta > ASSUME_POINTER_MOVE_STOPPED_MILLISECONDS as f64
            {
                break;
            }

            oldest_sample = sample;
            x.push(sample.point.x);
            y.push(sample.point.y);
            w.push(1.0);
            time.push(-age);
            index = if index == 0 { HISTORY_SIZE } else { index } - 1;

            sample_count += 1;
            if sample_count >= HISTORY_SIZE {
                break;
            }
        }

        if sample_count >= MIN_SAMPLE_SIZE {
            let x_fit = LeastSquaresSolver {
                x: &time,
                y: &x,
                w: &w,
            }
            .solve(2);
            let y_fit = LeastSquaresSolver {
                x: &time,
                y: &y,
                w: &w,
            }
            .solve(2);

            if let (Some(x_fit), Some(y_fit)) = (x_fit, y_fit) {
                // convert from pixels/ms to pixels/s
                return (
                    point(x_fit.coefficients[1] * 1000., y_fit.coefficients[1] * 1000.),
                    oldest_sample,
                );
            }
        }

        // We're unable to make a velocity estimate but we did have at least one
        // valid pointer position.
        (Point::default(), oldest_sample)
    }

    /// The iOS and macOS trackers' `oldestNonNullSample`: the oldest of the
    /// 20 samples kept.
    fn oldest_non_null_sample(&self, newest_sample: PointAtTime) -> PointAtTime {
        (1..=HISTORY_SIZE)
            .find_map(|i| self.samples[(self.index + i) % HISTORY_SIZE])
            .unwrap_or(newest_sample)
    }

    fn weighted_velocity(&self, weights: [(isize, f64); 3]) -> Point<f64> {
        weights
            .into_iter()
            .fold(Point::default(), |estimated_velocity, (index, weight)| {
                estimated_velocity + self.previous_velocity_at(index) * weight
            })
    }

    // Computes the velocity using 2 adjacent points in history. When index = 0,
    // it uses the latest point recorded and the point recorded immediately before
    // it. The smaller index is, the earlier in history the points used are.
    fn previous_velocity_at(&self, index: isize) -> Point<f64> {
        let at = |offset: isize| {
            self.samples[(self.index as isize + offset).rem_euclid(HISTORY_SIZE as isize) as usize]
        };
        let (Some(end), Some(start)) = (at(index), at(index - 1)) else {
            return Point::default();
        };

        let dt = end.time.checked_duration_since(start.time);
        debug_assert!(dt.is_some(), "dt >= 0");
        let dt = dt.map_or(0, |dt| dt.as_micros());

        if dt > 0 {
            // Convert dt to milliseconds to preserve floating point precision.
            (end.point - start.point) * 1000. / (dt as f64 / 1000.)
        } else {
            Point::default()
        }
    }
}
