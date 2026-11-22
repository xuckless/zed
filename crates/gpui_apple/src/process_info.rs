use block2::RcBlock;
use futures::{StreamExt, channel::mpsc};
use gpui::{ForegroundExecutor, PowerMode, Task, ThermalState};
use objc2::{
    rc::Retained,
    runtime::{AnyObject, ProtocolObject},
};
use objc2_foundation::{
    NSNotification, NSNotificationCenter, NSNotificationName, NSObjectProtocol, NSProcessInfo,
    NSProcessInfoPowerStateDidChangeNotification, NSProcessInfoThermalState,
    NSProcessInfoThermalStateDidChangeNotification,
};
use std::time::{Duration, Instant};

// Ported from flutter/flutter engine/src/flutter/shell/platform/darwin/ios/framework/Source/FlutterViewController.mm
// (`dispatchTouches:`, its `pointer_data.time_stamp = touch.timestamp * kMicrosecondsPerSecond`) and
// engine/src/flutter/shell/platform/darwin/macos/framework/Source/FlutterViewController.mm (its pointer events'
// `.timestamp = static_cast<size_t>(event.timestamp * USEC_PER_SEC)`) @ 3.50.1
// (ae9b586f34a8443573f82ef57c3bf914a908cbf7). Copyright 2014 The Flutter Authors. BSD-3-Clause, see
// third_party/flutter/LICENSE.
/// An event time on the uptime clock moved onto GPUI's clock, as Flutter's Apple embedders send `UITouch.timestamp`
/// and `NSEvent.timestamp` as `PointerEvent.timeStamp`. Both count seconds since boot, as
/// `NSProcessInfo.systemUptime` does, so the event's age is taken off the current instant; a stamp later than now
/// counts as now.
pub fn uptime_instant(timestamp: f64) -> Option<Instant> {
    let age = NSProcessInfo::processInfo().systemUptime() - timestamp;
    Instant::now().checked_sub(Duration::try_from_secs_f64(age.max(0.)).ok()?)
}

pub fn thermal_state() -> ThermalState {
    match NSProcessInfo::processInfo().thermalState() {
        NSProcessInfoThermalState::Fair => ThermalState::Fair,
        NSProcessInfoThermalState::Serious => ThermalState::Serious,
        NSProcessInfoThermalState::Critical => ThermalState::Critical,
        _ => ThermalState::Nominal,
    }
}

pub fn power_mode() -> PowerMode {
    if NSProcessInfo::processInfo().isLowPowerModeEnabled() {
        PowerMode::LowPower
    } else {
        // This API does not distinguish Automatic from High Power Mode on macOS.
        PowerMode::Unknown
    }
}

pub fn observe_thermal_state(
    executor: &ForegroundExecutor,
    callback: Box<dyn FnMut()>,
) -> ProcessInfoObserver {
    // Foundation requires reading thermalState before registering for changes.
    // https://developer.apple.com/documentation/foundation/processinfo/thermalstatedidchangenotification
    thermal_state();
    ProcessInfoObserver::new(
        unsafe { NSProcessInfoThermalStateDidChangeNotification },
        executor,
        callback,
    )
}

pub fn observe_power_mode(
    executor: &ForegroundExecutor,
    callback: Box<dyn FnMut()>,
) -> ProcessInfoObserver {
    ProcessInfoObserver::new(
        unsafe { NSProcessInfoPowerStateDidChangeNotification },
        executor,
        callback,
    )
}

/// Runs `callback` on `executor` after each `name` notification `center` posts, from any sender: UIKit's and
/// AppKit's accessibility settings notifications. Deferred and removed on drop, as the process info observers.
pub fn observe_notification(
    center: Retained<NSNotificationCenter>,
    name: &NSNotificationName,
    executor: &ForegroundExecutor,
    callback: Box<dyn FnMut()>,
) -> ProcessInfoObserver {
    ProcessInfoObserver::with_center(center, name, None, executor, callback)
}

pub struct ProcessInfoObserver {
    center: Retained<NSNotificationCenter>,
    observer: Retained<ProtocolObject<dyn NSObjectProtocol>>,
    _task: Task<()>,
}

impl ProcessInfoObserver {
    fn new(
        name: &NSNotificationName,
        executor: &ForegroundExecutor,
        callback: Box<dyn FnMut()>,
    ) -> Self {
        Self::with_center(
            NSNotificationCenter::defaultCenter(),
            name,
            Some(&NSProcessInfo::processInfo()),
            executor,
            callback,
        )
    }

    /// `object`: the sender to observe, `None` for any.
    fn with_center(
        center: Retained<NSNotificationCenter>,
        name: &NSNotificationName,
        object: Option<&AnyObject>,
        executor: &ForegroundExecutor,
        mut callback: Box<dyn FnMut()>,
    ) -> Self {
        let (sender, mut receiver) = mpsc::unbounded();
        let block = RcBlock::new(move |_: std::ptr::NonNull<NSNotification>| {
            // Notifications can arrive synchronously or off-thread. Defer application
            // callbacks so they cannot reenter GPUI while it is already borrowed.
            if sender.unbounded_send(()).is_err() {
                log::debug!("process info observer stopped before notification delivery");
            }
        });
        let observer = unsafe {
            center.addObserverForName_object_queue_usingBlock(Some(name), object, None, &block)
        };
        let task = executor.spawn(async move {
            while receiver.next().await.is_some() {
                callback();
            }
        });
        Self {
            center,
            observer,
            _task: task,
        }
    }
}

impl Drop for ProcessInfoObserver {
    fn drop(&mut self) {
        unsafe {
            self.center.removeObserver(self.observer.as_ref());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};

    fn post_notification(center: &NSNotificationCenter, name: &NSNotificationName) {
        unsafe {
            center.postNotificationName_object(name, Some(&NSProcessInfo::processInfo()));
        }
    }

    #[gpui::test]
    fn notifications_are_deferred_and_cancelled_on_drop(cx: &mut gpui::TestAppContext) {
        for name in unsafe {
            [
                NSProcessInfoPowerStateDidChangeNotification,
                NSProcessInfoThermalStateDidChangeNotification,
            ]
        } {
            check_notification_lifecycle(cx, name);
        }
    }

    fn check_notification_lifecycle(
        cx: &mut gpui::TestAppContext,
        name: &'static NSNotificationName,
    ) {
        // Tests cannot share the default center because they run concurrently.
        let center = NSNotificationCenter::new();
        let calls = Rc::new(Cell::new(0));
        let observer = ProcessInfoObserver::with_center(
            center.clone(),
            name,
            Some(&NSProcessInfo::processInfo()),
            &cx.foreground_executor,
            Box::new({
                let calls = calls.clone();
                move || calls.set(calls.get() + 1)
            }),
        );
        post_notification(&center, name);
        assert_eq!(calls.get(), 0);
        cx.run_until_parked();
        assert_eq!(calls.get(), 1);

        cx.background_executor
            .spawn({
                let center = center.clone();
                let name = name.to_string();
                async move {
                    post_notification(&center, &objc2_foundation::NSString::from_str(&name))
                }
            })
            .detach();
        assert_eq!(calls.get(), 1);
        cx.run_until_parked();
        assert_eq!(calls.get(), 2);

        post_notification(&center, name);
        drop(observer);
        cx.run_until_parked();
        assert_eq!(calls.get(), 2);
        post_notification(&center, name);
        cx.run_until_parked();
        assert_eq!(calls.get(), 2);
    }
}
