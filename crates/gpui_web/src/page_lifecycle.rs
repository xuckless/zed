// Ported from GoogleChromeLabs/page-lifecycle src/Lifecycle.mjs @ a32563d198a7e4f5c2338fe6a53a02fd5857992b
// (0.1.2): its states, `LEGAL_STATE_TRANSITIONS`, `getLegalStateTransitionPath`, `getCurrentState` and the event
// rules of `_handleEvents`. Copyright 2018 Google Inc. Apache-2.0.
// The phases follow flutter/flutter engine/src/flutter/lib/web_ui/lib/src/engine/platform_dispatcher/
// app_lifecycle_state.dart @ 3.50.1 (ae9b586f34a8443573f82ef57c3bf914a908cbf7): focus is resumed, blur inactive,
// a hidden page hidden. Copyright 2014 The Flutter Authors. BSD-3-Clause, see third_party/flutter/LICENSE.

//! The page's lifecycle (Chrome's Page Lifecycle states) and the [`AppLifecyclePhase`]s GPUI hears for it. Pure,
//! so it is tested on any host; `platform.rs` feeds it DOM events and passes the phases on.
//!
//! Flutter's web engine listens to `focus`, `blur` and `visibilitychange` only. Page Lifecycle adds `freeze`,
//! `resume`, `pagehide` and `pageshow`, so a page entering the back/forward cache or unloading is heard too, and it
//! reports every state a browser skips (a tab closed while active still hears `Inactive` and `Background`).

use gpui::AppLifecyclePhase;

/// Page Lifecycle's states (`discarded` is never observed by a running page).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PageState {
    /// Visible and focused.
    Active,
    /// Visible, not focused.
    Passive,
    /// Not visible.
    Hidden,
    /// Suspended by the browser, or in the back/forward cache.
    Frozen,
    /// Unloading.
    Terminated,
}

/// The DOM events Page Lifecycle listens to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PageEvent {
    Focus,
    Blur,
    VisibilityChange,
    Freeze,
    Resume,
    PageShow,
    /// `pagehide`; `persisted` when the page goes into the back/forward cache.
    PageHide {
        persisted: bool,
    },
}

/// What the document says when an event arrives: `getCurrentState`'s inputs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Document {
    /// `document.visibilityState == "hidden"`.
    pub(crate) hidden: bool,
    /// `document.hasFocus()`.
    pub(crate) has_focus: bool,
}

/// `LEGAL_STATE_TRANSITIONS`: the orders a browser moves through. A change from one state to a later one in an
/// order passes every state between; browsers skip events, so the skipped states are reported too.
const LEGAL_STATE_TRANSITIONS: [&[PageState]; 6] = {
    use PageState::*;
    [
        // The normal unload process.
        &[Active, Passive, Hidden, Terminated],
        // An active page transitioning to frozen, or an unloading page going into the bfcache.
        &[Active, Passive, Hidden, Frozen],
        // A hidden page transitioning back to active.
        &[Hidden, Passive, Active],
        // A frozen page being resumed.
        &[Frozen, Hidden],
        // A frozen (bfcached) page navigated back to.
        &[Frozen, Active],
        &[Frozen, Passive],
    ]
};

/// The phases GPUI hears for each step of a legal path, Flutter's mapping: `Active` is resumed, `Passive` inactive,
/// `Hidden` hidden. Frozen is still hidden to GPUI. Every step of [`LEGAL_STATE_TRANSITIONS`] has a row (the
/// `every_legal_step_has_phases` test checks); to add a state, add its steps here.
const STEP_PHASES: [(PageState, PageState, &[AppLifecyclePhase]); 9] = {
    use AppLifecyclePhase as Phase;
    use PageState::*;
    [
        (Active, Passive, &[Phase::Inactive]),
        (Passive, Hidden, &[Phase::Background]),
        (Hidden, Terminated, &[Phase::Disconnected]),
        (Hidden, Frozen, &[]),
        (Hidden, Passive, &[Phase::Foreground]),
        (Passive, Active, &[Phase::Active]),
        (Frozen, Hidden, &[]),
        (Frozen, Active, &[Phase::Foreground, Phase::Active]),
        (Frozen, Passive, &[Phase::Foreground]),
    ]
};

impl PageState {
    /// `getCurrentState`.
    pub(crate) fn current(document: Document) -> Self {
        if document.hidden {
            Self::Hidden
        } else if document.has_focus {
            Self::Active
        } else {
            Self::Passive
        }
    }

    /// `_handleEvents` then `_dispatchChangesIfNeeded`: the state after `event`, and the phases GPUI hears on the
    /// way. A change with no legal path changes nothing.
    pub(crate) fn on(self, event: PageEvent, document: Document) -> (Self, Vec<AppLifecyclePhase>) {
        let target = match event {
            PageEvent::PageShow | PageEvent::Resume => Some(Self::current(document)),
            PageEvent::Focus => Some(Self::Active),
            // The `blur` event can fire while the page is being unloaded, so the state only changes from active.
            PageEvent::Blur => (self == Self::Active).then(|| Self::current(document)),
            PageEvent::PageHide { persisted: true } => Some(Self::Frozen),
            PageEvent::PageHide { persisted: false } => Some(Self::Terminated),
            // `visibilityState` turns hidden as the page unloads; that changes nothing.
            PageEvent::VisibilityChange => {
                (!matches!(self, Self::Frozen | Self::Terminated)).then(|| Self::current(document))
            }
            PageEvent::Freeze => Some(Self::Frozen),
        };
        let Some(path) = target.and_then(|target| legal_path(self, target)) else {
            return (self, Vec::new());
        };
        let phases = path
            .windows(2)
            .flat_map(|step| step_phases(step[0], step[1]))
            .copied()
            .collect();
        (path[path.len() - 1], phases)
    }
}

/// `getLegalStateTransitionPath`: the first order holding `old` before `new`, from `old` to `new` inclusive.
fn legal_path(old: PageState, new: PageState) -> Option<&'static [PageState]> {
    LEGAL_STATE_TRANSITIONS.iter().find_map(|order| {
        let old_index = order.iter().position(|&state| state == old)?;
        let new_index = order.iter().position(|&state| state == new)?;
        (new_index > old_index).then(|| &order[old_index..=new_index])
    })
}

fn step_phases(from: PageState, to: PageState) -> &'static [AppLifecyclePhase] {
    STEP_PHASES
        .iter()
        .find(|(step_from, step_to, _)| (*step_from, *step_to) == (from, to))
        .map_or(&[], |(_, _, phases)| phases)
}

#[cfg(test)]
mod tests {
    use super::*;
    use AppLifecyclePhase as Phase;
    use PageEvent::*;
    use PageState::*;

    const FOCUSED: Document = Document {
        hidden: false,
        has_focus: true,
    };
    const UNFOCUSED: Document = Document {
        hidden: false,
        has_focus: false,
    };
    const HIDDEN: Document = Document {
        hidden: true,
        has_focus: false,
    };

    /// One row per (state, event, document) → (next state, phases), rejected events included. Rows named after
    /// app_lifecycle_state.dart's listeners are Flutter's; the rest are Page Lifecycle's.
    #[test]
    fn transitions() {
        let rows: [(&str, PageState, PageEvent, Document, PageState, &[Phase]); 17] = [
            (
                "Flutter: focus is resumed",
                Passive,
                Focus,
                FOCUSED,
                Active,
                &[Phase::Active],
            ),
            (
                "Flutter: blur is inactive",
                Active,
                Blur,
                UNFOCUSED,
                Passive,
                &[Phase::Inactive],
            ),
            (
                "Flutter: hidden is hidden",
                Active,
                VisibilityChange,
                HIDDEN,
                Hidden,
                &[Phase::Inactive, Phase::Background],
            ),
            (
                "Flutter: visible is resumed",
                Hidden,
                VisibilityChange,
                FOCUSED,
                Active,
                &[Phase::Foreground, Phase::Active],
            ),
            (
                "visible without focus is passive",
                Hidden,
                VisibilityChange,
                UNFOCUSED,
                Passive,
                &[Phase::Foreground],
            ),
            (
                "blur only counts from active",
                Passive,
                Blur,
                UNFOCUSED,
                Passive,
                &[],
            ),
            (
                "an element's blur keeps a focused page active",
                Active,
                Blur,
                FOCUSED,
                Active,
                &[],
            ),
            (
                "freezing a hidden page",
                Hidden,
                Freeze,
                HIDDEN,
                Frozen,
                &[],
            ),
            (
                "resuming a frozen hidden page",
                Frozen,
                Resume,
                HIDDEN,
                Hidden,
                &[],
            ),
            (
                "a frozen page ignores visibility",
                Frozen,
                VisibilityChange,
                FOCUSED,
                Frozen,
                &[],
            ),
            (
                "into the back/forward cache from active",
                Active,
                PageHide { persisted: true },
                HIDDEN,
                Frozen,
                &[Phase::Inactive, Phase::Background],
            ),
            (
                "back from the back/forward cache",
                Frozen,
                PageShow,
                FOCUSED,
                Active,
                &[Phase::Foreground, Phase::Active],
            ),
            (
                "back from the cache without focus",
                Frozen,
                PageShow,
                UNFOCUSED,
                Passive,
                &[Phase::Foreground],
            ),
            (
                "unloading an active page",
                Active,
                PageHide { persisted: false },
                HIDDEN,
                Terminated,
                &[Phase::Inactive, Phase::Background, Phase::Disconnected],
            ),
            (
                "unloading a hidden page",
                Hidden,
                PageHide { persisted: false },
                HIDDEN,
                Terminated,
                &[Phase::Disconnected],
            ),
            (
                "a terminated page ignores focus",
                Terminated,
                Focus,
                FOCUSED,
                Terminated,
                &[],
            ),
            (
                "the first pageshow changes nothing",
                Active,
                PageShow,
                FOCUSED,
                Active,
                &[],
            ),
        ];
        for (name, state, event, document, next, phases) in rows {
            assert_eq!(state.on(event, document), (next, phases.to_vec()), "{name}");
        }
    }

    #[test]
    fn current_state() {
        assert_eq!(PageState::current(FOCUSED), Active);
        assert_eq!(PageState::current(UNFOCUSED), Passive);
        assert_eq!(PageState::current(HIDDEN), Hidden);
    }

    #[test]
    fn every_legal_step_has_phases() {
        for order in LEGAL_STATE_TRANSITIONS {
            for step in order.windows(2) {
                assert!(
                    STEP_PHASES
                        .iter()
                        .any(|(from, to, _)| (*from, *to) == (step[0], step[1])),
                    "no STEP_PHASES row for {:?} → {:?}",
                    step[0],
                    step[1]
                );
            }
        }
    }
}
