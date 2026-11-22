// Ported from flutter/flutter engine/src/flutter/third_party/spring_animation/spring_animation.mm and
// SpringAnimationTest.mm @ 3.50.1 (ae9b586f34a8443573f82ef57c3bf914a908cbf7), which Flutter derived from
// facebook/react-native Libraries/Animated/animations/SpringAnimation.js
// @ 494c47360f62761176033a4da359f43b53c2182f. Copyright (c) Meta Platforms, Inc. and affiliates. MIT, see
// LICENSE-REACT-NATIVE. The tests: Copyright 2014 The Flutter Authors. BSD-3-Clause, see LICENSE-FLUTTER.
//! The iOS keyboard's spring curve, as Flutter's keyboard inset manager computes it: the inset follows the
//! keyboard's `CASpringAnimation` frame-exact instead of sampling a hidden view's presentation layer.

/// Flutter's `SpringAnimation`: a spring from `from_value` to `to_value`, sampled by time since it started.
// Spring code adapted from React Native's Animation Library, see:
// https://github.com/facebook/react-native/blob/main/Libraries/Animated/animations/SpringAnimation.js
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpringAnimation {
    pub stiffness: f64,
    pub damping: f64,
    pub mass: f64,
    pub initial_velocity: f64,
    pub from_value: f64,
    pub to_value: f64,
    zeta: f64,
    omega0: f64,
    omega1: f64,
    v0: f64,
}

impl SpringAnimation {
    pub fn new(
        stiffness: f64,
        damping: f64,
        mass: f64,
        initial_velocity: f64,
        from_value: f64,
        to_value: f64,
    ) -> Self {
        let zeta = damping / (2. * (stiffness * mass).sqrt()); // Damping ratio.
        let omega0 = (stiffness / mass).sqrt(); // Undamped angular frequency of the oscillator.
        let omega1 = omega0 * (1.0 - zeta * zeta).sqrt(); // Exponential decay.
        Self {
            stiffness,
            damping,
            mass,
            initial_velocity,
            from_value,
            to_value,
            zeta,
            omega0,
            omega1,
            v0: -initial_velocity,
        }
    }

    /// The value `t` seconds after the spring started.
    pub fn curve_function(&self, t: f64) -> f64 {
        let x0 = self.to_value - self.from_value;

        if self.zeta < 1. {
            // Under damped.
            let envelope = (-self.zeta * self.omega0 * t).exp();
            self.to_value
                - envelope
                    * (((self.v0 + self.zeta * self.omega0 * x0) / self.omega1)
                        * (self.omega1 * t).sin()
                        + x0 * (self.omega1 * t).cos())
        } else {
            // Critically damped.
            let envelope = (-self.omega0 * t).exp();
            self.to_value - envelope * (x0 + (self.v0 + self.omega0 * x0) * t)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The keyboard curve config on iOS platform.
    fn keyboard(from_value: f64, to_value: f64) -> SpringAnimation {
        SpringAnimation::new(1000., 500., 3., 0., from_value, to_value)
    }

    #[test]
    fn can_initialize_correctly() {
        // SpringAnimationTest.CanInitializeCorrectly
        let animation = keyboard(0., 1000.);
        assert_eq!(
            (
                animation.stiffness,
                animation.damping,
                animation.mass,
                animation.initial_velocity,
                animation.from_value,
                animation.to_value
            ),
            (1000., 500., 3., 0., 0., 1000.)
        );
    }

    #[test]
    fn curve_function_rows() {
        // (Flutter test, from, to, time, expected value, accuracy)
        let rows = [
            (
                "CurveFunctionCanWorkCorrectly: starts at fromValue",
                0.,
                1000.,
                0.,
                0.,
                1.,
            ),
            (
                "CurveFunctionCanWorkCorrectly: reaches toValue by 0.6 s",
                0.,
                1000.,
                0.6,
                1000.,
                1.,
            ),
        ];
        for (name, from, to, time, expected, accuracy) in rows {
            let value = keyboard(from, to).curve_function(time);
            assert!((value - expected).abs() < accuracy, "{name}: {value}");
        }
    }

    #[test]
    fn can_update_position_values_and_calculate_correctly() {
        // SpringAnimationTest.CanUpdatePositionValuesAndCalculateCorrectly
        let animation = keyboard(0., 1000.);
        let (start_value1, to_value1) = (animation.curve_function(0.), animation.curve_function(0.6));
        let animation = SpringAnimation {
            from_value: 10.,
            to_value: 800.,
            ..animation
        };
        assert_eq!((animation.from_value, animation.to_value), (10., 800.));
        assert!(animation.curve_function(0.) > start_value1);
        assert!(animation.curve_function(0.6) < to_value1);
    }
}
