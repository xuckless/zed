// Ported from flutter/flutter engine/src/flutter/shell/platform/darwin/ios/framework/Source/FlutterTextInputPluginTest.mm
// @ 3.50.1 (ae9b586f34a8443573f82ef57c3bf914a908cbf7): its UITextInput behaviour, text trait and update-count cases.
// Copyright 2014 The Flutter Authors. BSD-3-Clause, see third_party/flutter/LICENSE.
//
// Each Flutter test is one row: steps (UITextInput calls on the view, or what the app does) and the checks Flutter
// asserts, in Flutter's order. Flutter's `FlutterTextInputView` holds the text itself; in GPUI the app does, so a fake
// app (`Field`, upstream's `examples/input.rs` editing rules on UTF-16) answers the view's `PlatformInputHandler`.
// Flutter's `setSelectionRects:` input becomes the app's geometry answers (`Answer`): in GPUI the app lays text out
// (`bounds_for_range` is `firstRect`), so those rows check that the view passes the app's geometry on unchanged.
// Flutter's checks on its own model (`inputView.text`) read the fake app; delta, affinity and OCMock checks are dropped.
// Cases left out, and why: docs/ios-text-input.md in Parallax. To add a case: add a row; a new kind of call or check
// is a `Step` variant and one arm in `run_step`.

use super::*;
use gpui::{
    App, Bounds, InputHandler, Pixels, Point, TestAppContext, UTF16Selection, Window, point, size,
};
use objc2::AllocAnyThread;
use objc2_foundation::NSAttributedString;
use std::any::Any;

/// The app's answer to one geometry query, as a Flutter row's `setSelectionRects:` implies it.
#[derive(Clone)]
enum Answer {
    /// `None`: the app has no geometry for the range.
    FirstRect(Range<usize>, Option<CGRect>),
    SelectionRects(Range<usize>, Vec<CGRect>),
    Index(CGPoint, usize),
}

/// Upstream's `examples/input.rs` text field, on UTF-16 so a test can hold any NSString.
#[derive(Default)]
struct Field {
    text: Vec<u16>,
    selection: Range<usize>,
    marked: Option<Range<usize>>,
    answers: Vec<Answer>,
    /// Calls that change the app's text, selection or composition: Flutter's `updateEditingClient` count.
    edits: usize,
    /// The view's input delegate once a row watches it, and what it heard: each notice with the app's selection then.
    watcher: Option<Retained<SelectionWatcher>>,
    notices: Vec<(&'static str, Range<usize>)>,
}

impl Field {
    fn clamp(&self, range: Range<usize>) -> Range<usize> {
        range.start.min(self.text.len())..range.end.min(self.text.len())
    }

    fn splice(&mut self, range: Option<Range<usize>>, text: &str) -> Range<usize> {
        let range = range
            .or(self.marked.clone())
            .unwrap_or(self.selection.clone());
        let range = self.clamp(range);
        let units: Vec<u16> = text.encode_utf16().collect();
        let inserted = range.start..range.start + units.len();
        self.text.splice(range, units);
        self.edits += 1;
        inserted
    }

    // Default geometry for queries no row answers: 10×20 pt cells, one per UTF-16 unit, a new line after '\n'.
    fn cell(&self, offset: usize) -> (f64, f64) {
        let before = &self.text[..offset.min(self.text.len())];
        let line = before
            .iter()
            .filter(|unit| **unit == u16::from(b'\n'))
            .count();
        let column = before
            .iter()
            .rev()
            .take_while(|unit| **unit != u16::from(b'\n'))
            .count();
        (column as f64 * 10., line as f64 * 20.)
    }
}

struct FakeApp(Rc<RefCell<Field>>);

define_class!(
    /// The view's input delegate in a row that watches it, standing in for UIKit's keyboard, which acts on the
    /// selection it knows when it hears `selectionWillChange:`.
    #[unsafe(super = objc2_foundation::NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "GPUITextInputOracleSelectionWatcher"]
    #[ivars = std::rc::Weak<RefCell<Field>>]
    struct SelectionWatcher;

    unsafe impl NSObjectProtocol for SelectionWatcher {}
    unsafe impl UITextInputDelegate for SelectionWatcher {
        #[unsafe(method(selectionWillChange:))]
        fn selection_will_change(&self, _input: Option<&ProtocolObject<dyn UITextInput>>) {
            self.heard("selection-will");
        }
        #[unsafe(method(selectionDidChange:))]
        fn selection_did_change(&self, _input: Option<&ProtocolObject<dyn UITextInput>>) {
            self.heard("selection-did");
        }
        #[unsafe(method(textWillChange:))]
        fn text_will_change(&self, _input: Option<&ProtocolObject<dyn UITextInput>>) {
            self.heard("text-will");
        }
        #[unsafe(method(textDidChange:))]
        fn text_did_change(&self, _input: Option<&ProtocolObject<dyn UITextInput>>) {
            self.heard("text-did");
        }
        #[unsafe(method(conversationContext:didChange:))]
        fn conversation_context_did_change(
            &self,
            _context: Option<&UIConversationContext>,
            _input: Option<&ProtocolObject<dyn UITextInput>>,
        ) {
        }
    }
);

impl SelectionWatcher {
    fn heard(&self, notice: &'static str) {
        if let Some(field) = self.ivars().upgrade() {
            let mut field = field.borrow_mut();
            let selection = field.selection.clone();
            field.notices.push((notice, selection));
        }
    }
}

fn bounds(rect: CGRect) -> Bounds<Pixels> {
    Bounds::new(
        point(px(rect.origin.x as f32), px(rect.origin.y as f32)),
        size(px(rect.size.width as f32), px(rect.size.height as f32)),
    )
}

impl InputHandler for FakeApp {
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut App,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.0.borrow().selection.clone(),
            reversed: false,
        })
    }

    fn marked_text_range(&mut self, _: &mut Window, _: &mut App) -> Option<Range<usize>> {
        self.0.borrow().marked.clone()
    }

    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut App,
    ) -> Option<String> {
        let field = self.0.borrow();
        let range = field.clamp(range);
        *adjusted = Some(range.clone());
        Some(String::from_utf16_lossy(&field.text[range]))
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        _: &mut App,
    ) {
        let mut field = self.0.borrow_mut();
        let inserted = field.splice(range, text);
        field.selection = inserted.end..inserted.end;
        field.marked = None;
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        _: &mut Window,
        _: &mut App,
    ) {
        let mut field = self.0.borrow_mut();
        let inserted = field.splice(range, text);
        field.marked = (!inserted.is_empty()).then_some(inserted.clone());
        let length = inserted.len();
        field.selection = selected.map_or(inserted.end..inserted.end, |selected| {
            inserted.start + selected.start.min(length)..inserted.start + selected.end.min(length)
        });
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut App) {
        let mut field = self.0.borrow_mut();
        field.marked = None;
        field.edits += 1;
    }

    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        _: &mut Window,
        _: &mut App,
    ) -> Option<Bounds<Pixels>> {
        let field = self.0.borrow();
        let answer = field.answers.iter().find_map(|answer| match answer {
            Answer::FirstRect(answered, rect) if *answered == range => Some(*rect),
            _ => None,
        });
        let (x, y) = field.cell(range.start);
        let (end_x, end_y) = field.cell(range.end);
        let width = if end_y == y { end_x - x } else { 0. };
        answer
            .unwrap_or(Some(CGRect::new(
                CGPoint::new(x, y),
                CGSize::new(width, 20.),
            )))
            .map(bounds)
    }

    fn selection_bounds_for_range(
        &mut self,
        range: Range<usize>,
        _: &mut Window,
        _: &mut App,
    ) -> Vec<Bounds<Pixels>> {
        self.0
            .borrow()
            .answers
            .iter()
            .find_map(|answer| match answer {
                Answer::SelectionRects(answered, rects) if *answered == range => {
                    Some(rects.iter().copied().map(bounds).collect())
                }
                _ => None,
            })
            .unwrap_or_default()
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut App,
    ) -> Option<usize> {
        let field = self.0.borrow();
        let point = CGPoint::new(f64::from(point.x), f64::from(point.y));
        let answer = field.answers.iter().find_map(|answer| match answer {
            Answer::Index(answered, index) if *answered == point => Some(*index),
            _ => None,
        });
        answer.or_else(|| {
            (0..=field.text.len()).min_by(|a, b| {
                let distance = |offset| {
                    let (x, y) = field.cell(offset);
                    (y + 10. - point.y).abs() * 1000. + (x - point.x).abs()
                };
                distance(*a).total_cmp(&distance(*b))
            })
        })
    }

    fn set_selected_text_range(&mut self, range: Range<usize>, _: &mut Window, _: &mut App) {
        let mut field = self.0.borrow_mut();
        field.selection = field.clamp(range);
        field.edits += 1;
    }

    fn text_length_utf16(&mut self, _: &mut Window, _: &mut App) -> Option<usize> {
        Some(self.0.borrow().text.len())
    }
}

/// One UITextInput call, app change or check, in a Flutter test's order.
enum Step {
    // UIKit → view (what the keyboard does).
    Insert(&'static str),
    /// `[@"<s>" substringWithRange:NSMakeRange(0, 1)]`: half of a surrogate pair.
    InsertFirstUnitOf(&'static str),
    DeleteBackward,
    /// `shouldChangeTextInRange:replacementText:@""`, sent before each keyboard edit.
    ShouldChange,
    SetMarked(Option<&'static str>, Range<usize>),
    SetAttributedMarked(Option<&'static str>, Range<usize>),
    Unmark,
    Replace(Range<usize>, &'static str),
    Select(Range<usize>),
    KeyboardVisible(bool),
    Configure(TextInputConfiguration),
    /// A standard edit action (`selectAll:`, `cut:`, `copy:`, `paste:`, `delete:`).
    Action(&'static str),
    /// `UIPasteboard.generalPasteboard.color = UIColor.redColor`: the pasteboard holds no string.
    PasteboardHoldsColor,
    /// The view gets an input delegate that records what it hears (checked by `Notices`).
    WatchDelegate,
    // The app changes its own state and tells the view, as Flutter's framework does with `setTextInputState:`.
    AppSets {
        text: &'static str,
        selection: Range<usize>,
        marked: Option<Range<usize>>,
    },
    Answers(Vec<Answer>),
    // Checks.
    Text(&'static str),
    Selection(Range<usize>),
    Marked(Option<Range<usize>>),
    Edits(usize),
    /// What the input delegate heard since the last check, each with the app's selection at that moment.
    Notices(Vec<(&'static str, Range<usize>)>),
    /// `textInRange:`'s `length`, nil counting as 0 as in Objective-C.
    TextInRangeLength(Range<usize>, usize),
    /// `textInRange:` from `beginningOfDocument` to `endOfDocument`.
    DocumentText(Option<&'static str>),
    /// `textInRange:` with a range some other object made (Flutter: an `NSNotFound` location).
    ForeignTextInRange(Option<&'static str>),
    TextRangeFromPositions(usize, usize, Range<usize>),
    FirstRect(Range<usize>, CGRect),
    CaretRect(usize, CGRect),
    SelectionRects(Range<usize>, Vec<CGRect>),
    Closest(CGPoint, Option<usize>),
    ClosestWithin(CGPoint, Range<usize>, Option<usize>),
    /// `rangeEnclosingPosition:withGranularity:UITextGranularityLine inDirection:`.
    Line(usize, UITextDirection, Option<Range<usize>>),
    CanPerform(&'static str, bool),
    RespondsTo(&'static str, bool),
    KeyboardType(UIKeyboardType),
    Secure(bool),
    Autocorrection(UITextAutocorrectionType),
    SpellChecking(UITextSpellCheckingType),
    InlinePrediction(UITextInlinePredictionType),
}

/// Whether a row must pass. `Open` rows run and report, and fail the run once they pass, so the row is promoted.
enum Status {
    Pass,
    Open(&'static str),
}

struct Row {
    flutter_test: &'static str,
    status: Status,
    steps: Vec<Step>,
}

fn row(flutter_test: &'static str, steps: Vec<Step>) -> Row {
    Row {
        flutter_test,
        status: Status::Pass,
        steps,
    }
}

fn open(flutter_test: &'static str, why: &'static str, steps: Vec<Step>) -> Row {
    Row {
        flutter_test,
        status: Status::Open(why),
        steps,
    }
}

fn rect(x: f64, y: f64, width: f64, height: f64) -> CGRect {
    CGRect::new(CGPoint::new(x, y), CGSize::new(width, height))
}

const RIGHT: UITextDirection = UITextLayoutDirection::Right.0;
const FORWARD: UITextDirection = UITextStorageDirection::Forward.0;
const BACKWARD: UITextDirection = UITextStorageDirection::Backward.0;

fn configuration(edit: impl FnOnce(&mut TextInputConfiguration)) -> Step {
    // Flutter's `mutableTemplateCopy`: autocorrect on, unspecified action, text keyboard.
    let mut configuration = TextInputConfiguration {
        autocorrect: true,
        suggestions: true,
        ..Default::default()
    };
    edit(&mut configuration);
    Step::Configure(configuration)
}

/// Flutter's first-rect rows: text "COMPOSING", the app's answer for each range (the rect the view must return),
/// then the row's own last steps.
fn first_rect_row(
    flutter_test: &'static str,
    answers: Vec<(Range<usize>, CGRect)>,
    last: Vec<Step>,
) -> Row {
    let checks = answers
        .iter()
        .map(|(range, rect)| Step::FirstRect(range.clone(), *rect));
    let mut steps = vec![
        Step::AppSets {
            text: "COMPOSING",
            selection: 0..0,
            marked: None,
        },
        Step::Answers(
            answers
                .iter()
                .map(|(range, rect)| Answer::FirstRect(range.clone(), Some(*rect)))
                .collect(),
        ),
    ];
    steps.extend(checks);
    steps.extend(last);
    row(flutter_test, steps)
}

/// The single-line first-rect tests end on "COM" with a range past its end.
fn range_outside_bounds() -> Vec<Step> {
    vec![
        Step::AppSets {
            text: "COM",
            selection: 0..0,
            marked: None,
        },
        Step::FirstRect(3..4, CGRect::ZERO),
    ]
}

// One row per line of a Flutter test keeps the table readable next to Flutter's source.
#[rustfmt::skip]
fn rows() -> Vec<Row> {
    use Step::*;
    vec![
        // --- UITextInput behaviour ---
        row("testTextRangeFromPositionMatchesUITextViewBehavior", vec![
            AppSets { text: "abc", selection: 0..0, marked: None },
            TextRangeFromPositions(2, 0, 0..2),
        ]),
        row("testTextInRange", vec![
            Insert("test"),
            TextInRangeLength(0..20, 4),
            TextInRangeLength(10..20, 0),
        ]),
        row("testInsertTextReplacesEntireMarkedRangeForDeadKeySequence", vec![
            SetMarked(Some("´"), 1..1),
            Insert("Á"),
            DocumentText(Some("Á")),
        ]),
        row("testTextInRangeAcceptsNSNotFoundLocationGracefully", vec![
            Insert("text"),
            ForeignTextInRange(None),
        ]),
        row("testStandardEditActions", vec![
            Insert("aaaa"),
            Action("selectAll:"), Action("cut:"),
            Insert("bbbb"),
            CanPerform("paste:", true),
            Action("paste:"), Action("selectAll:"), Action("copy:"), Action("paste:"),
            Action("selectAll:"), Action("delete:"), Action("paste:"), Action("paste:"),
            Text("bbbbaaaabbbbaaaa"),
        ]),
        // Not a Flutter test: UIKit's keyboard collapses the selection it knows on `selectionWillChange:`, so an edit
        // action sends it before the change. Sent afterwards, Select All in UIKit's edit menu left a caret at the end
        // (hello on the iOS simulator).
        row("Parallax: edit actions tell the input delegate before the selection moves", vec![
            Insert("aaaa"),
            WatchDelegate,
            Action("selectAll:"),
            Notices(vec![("selection-will", 4..4), ("selection-did", 0..4)]),
            Selection(0..4),
            Action("cut:"),
            Notices(vec![
                ("text-will", 0..4), ("selection-will", 0..4), ("selection-did", 0..0), ("text-did", 0..0),
            ]),
        ]),
        row("testCanPerformActionForSelectActions", vec![
            CanPerform("selectAll:", false),
            Insert("aaaa"),
            CanPerform("selectAll:", true),
        ]),
        row("testCanPerformActionCaptureTextFromCamera", vec![
            CanPerform("captureTextFromCamera:", true),
            Insert("test"),
            Action("selectAll:"),
            CanPerform("captureTextFromCamera:", true),
        ]),
        row("testPastingNonTextDisallowed", vec![
            PasteboardHoldsColor,
            CanPerform("paste:", false),
            Action("paste:"),
            Text(""),
        ]),
        row("testDeletingBackward", vec![
            Insert("ឹ😀 text 🥰👨‍👩‍👧‍👦🇺🇳ดี "),
            DeleteBackward, DeleteBackward,
            // Thai vowel is removed.
            Text("ឹ😀 text 🥰👨‍👩‍👧‍👦🇺🇳ด"),
            DeleteBackward, Text("ឹ😀 text 🥰👨‍👩‍👧‍👦🇺🇳"),
            DeleteBackward, Text("ឹ😀 text 🥰👨‍👩‍👧‍👦"),
            DeleteBackward, Text("ឹ😀 text 🥰"),
            DeleteBackward, Text("ឹ😀 text "),
            DeleteBackward, DeleteBackward, DeleteBackward, DeleteBackward, DeleteBackward, DeleteBackward,
            Text("ឹ😀"),
            DeleteBackward, Text("ឹ"),
            DeleteBackward, Text(""),
        ]),
        // This tests the workaround to fix an iOS 16 bug. See: https://github.com/flutter/flutter/issues/111494
        row("testSystemOnlyAddingPartialComposedCharacter", vec![
            Insert("👨‍👩‍👧‍👦"),
            DeleteBackward,
            // Insert the first unichar in the emoji.
            InsertFirstUnitOf("👨‍👩‍👧‍👦"),
            Insert("아"),
            Text("👨‍👩‍👧‍👦아"),
            // Deleting 아.
            DeleteBackward,
            // 👨‍👩‍👧‍👦 should be the current string.
            Insert("😀"),
            DeleteBackward,
            // Insert the first unichar in the emoji.
            InsertFirstUnitOf("😀"),
            Insert("아"),
            Text("👨‍👩‍👧‍👦😀아"),
            // Deleting 아.
            DeleteBackward,
            // 👨‍👩‍👧‍👦😀 should be the current string.
            DeleteBackward,
            // Insert the first unichar in the emoji.
            InsertFirstUnitOf("😀"),
            Insert("아"),
            Text("👨‍👩‍👧‍👦😀아"),
        ]),
        // GPUI's text is a Rust string: the lone half of a surrogate pair arrives as U+FFFD.
        row("testCachedComposedCharacterClearedAtKeyboardInteraction", vec![
            Insert("👨‍👩‍👧‍👦"),
            DeleteBackward,
            ShouldChange,
            // Insert the first unichar in the emoji.
            InsertFirstUnitOf("👨‍👩‍👧‍👦"),
            Insert("아"),
            Text("\u{FFFD}아"),
        ]),
        row("testReplaceTestLocalAdjustSelectionAndMarkedTextRange", vec![
            SetMarked(Some("test text"), 0..5),
            Selection(0..5),
            Marked(Some(0..9)),
            // Replaces space with space.
            Replace(4..5, " "),
            Selection(5..5),
            Marked(None),
        ]),
        // Regression test for https://github.com/flutter/flutter/issues/172250.
        row("testFlutterTextInputViewIsNotClearWhenKeyboardShowAndHide", vec![
            SetMarked(Some("test text"), 0..5),
            Text("test text"),
            // Showing keyboard does not trigger clearing of marked text.
            KeyboardVisible(true),
            Text("test text"),
            // Hiding keyboard does not trigger clearing of marked text.
            KeyboardVisible(false),
            Text("test text"),
        ]),
        row("testSetAttributedMarkedTextSelectedRange", vec![
            SetAttributedMarked(Some("inline prediction"), 0..7),
            Text("inline prediction"),
            Selection(0..7),
            // Marked range length must match the attributed string length (17 for "inline prediction").
            Marked(Some(0..17)),
            // Nil attributed string should behave like empty string.
            SetAttributedMarked(None, 0..0),
            Text(""),
            Marked(None),
        ]),
        first_rect_row("testFirstRectForRangeReturnsCorrectRectOnASingleLineLeftToRight", vec![
            (1..2, rect(100., 0., 100., 100.)),
            (1..4, rect(100., 0., 300., 100.)),
        ], range_outside_bounds()),
        first_rect_row("testFirstRectForRangeReturnsCorrectRectOnASingleLineRightToLeft", vec![
            (1..2, rect(200., 0., 100., 100.)),
            (1..4, rect(0., 0., 300., 100.)),
        ], range_outside_bounds()),
        first_rect_row("testFirstRectForRangeReturnsCorrectRectOnMultipleLinesLeftToRight", vec![
            (1..2, rect(100., 0., 100., 100.)),
            (1..5, rect(100., 0., 300., 100.)),
        ], vec![]),
        first_rect_row("testFirstRectForRangeReturnsCorrectRectOnMultipleLinesRightToLeft", vec![
            (1..2, rect(200., 0., 100., 100.)),
            (1..5, rect(0., 0., 300., 100.)),
        ], vec![]),
        first_rect_row("testFirstRectForRangeReturnsCorrectRectOnSingleLineWithVaryingMinYAndMaxYLeftToRight", vec![
            (1..4, rect(100., -10., 300., 120.)),
        ], vec![]),
        first_rect_row("testFirstRectForRangeReturnsCorrectRectOnSingleLineWithVaryingMinYAndMaxYRightToLeft", vec![
            (1..4, rect(0., -10., 300., 120.)),
        ], vec![]),
        first_rect_row("testFirstRectForRangeReturnsCorrectRectWithOverlappingRectsExceedingThresholdLeftToRight", vec![
            (1..5, rect(100., 0., 300., 100.)),
        ], vec![]),
        first_rect_row("testFirstRectForRangeReturnsCorrectRectWithOverlappingRectsExceedingThresholdRightToLeft", vec![
            (1..5, rect(0., 0., 300., 100.)),
        ], vec![]),
        first_rect_row("testFirstRectForRangeReturnsCorrectRectWithOverlappingRectsWithinThresholdLeftToRight", vec![
            (1..5, rect(100., 0., 400., 140.)),
        ], vec![]),
        first_rect_row("testFirstRectForRangeReturnsCorrectRectWithOverlappingRectsWithinThresholdRightToLeft", vec![
            (1..5, rect(0., 0., 400., 140.)),
        ], vec![]),
        row("testClosestPositionToPoint", vec![
            AppSets { text: "COMPOSING", selection: 0..0, marked: None },
            Answers(vec![
                Answer::Index(CGPoint::new(150., 150.), 2),
                Answer::Index(CGPoint::new(125., 150.), 2),
                Answer::Index(CGPoint::new(125., 201.), 4),
                Answer::Index(CGPoint::new(125., 250.), 4),
                Answer::Index(CGPoint::new(110., 50.), 2),
            ]),
            // Minimize the vertical distance from the center of the rects first
            Closest(CGPoint::new(150., 150.), Some(2)),
            // Then, if the point is above the bottom of the closest rects vertically, get the closest x origin
            Closest(CGPoint::new(125., 150.), Some(2)),
            // However, if the point is below the bottom of the closest rects vertically, get the position
            // farthest to the right
            Closest(CGPoint::new(125., 201.), Some(4)),
            // Also check a point at the right edge of the last selection rect
            Closest(CGPoint::new(125., 250.), Some(4)),
            // Minimize vertical distance if the difference is more than 1 point.
            Closest(CGPoint::new(110., 50.), Some(2)),
        ]),
        row("testClosestPositionToPointRTL", vec![
            AppSets { text: "COMPOSING", selection: 0..0, marked: None },
            Answers([(275., 0), (225., 1), (175., 1), (125., 2), (75., 2), (25., 3), (-25., 3)]
                .map(|(x, index)| Answer::Index(CGPoint::new(x, 50.), index)).to_vec()),
            Closest(CGPoint::new(275., 50.), Some(0)),
            Closest(CGPoint::new(225., 50.), Some(1)),
            Closest(CGPoint::new(175., 50.), Some(1)),
            Closest(CGPoint::new(125., 50.), Some(2)),
            Closest(CGPoint::new(75., 50.), Some(2)),
            Closest(CGPoint::new(25., 50.), Some(3)),
            Closest(CGPoint::new(-25., 50.), Some(3)),
        ]),
        row("testSelectionRectsForRange", vec![
            AppSets { text: "COMPOSING", selection: 0..0, marked: None },
            Answers(vec![
                Answer::SelectionRects(1..3, vec![rect(100., 100., 100., 100.), rect(200., 200., 100., 100.)]),
                Answer::SelectionRects(1..1, vec![rect(100., 100., 0., 100.)]),
            ]),
            // Returns the matching rects within a range
            SelectionRects(1..3, vec![rect(100., 100., 100., 100.), rect(200., 200., 100., 100.)]),
            // Returns a 0 width rect for a 0-length range
            SelectionRects(1..1, vec![rect(100., 100., 0., 100.)]),
        ]),
        row("testClosestPositionToPointWithinRange", vec![
            AppSets { text: "COMPOSING", selection: 0..0, marked: None },
            Answers(vec![Answer::Index(CGPoint::new(125., 150.), 2)]),
            // Do not return a position before the start of the range
            ClosestWithin(CGPoint::new(125., 150.), 3..5, Some(3)),
            // Do not return a position after the end of the range
            ClosestWithin(CGPoint::new(125., 150.), 0..1, Some(1)),
        ]),
        // GPUI widens a zero-width caret to 1 pt on purpose (`nonempty_caret_rect`).
        row("testClosestPositionToPointWithPartialSelectionRects", vec![
            AppSets { text: "COMPOSING", selection: 0..0, marked: None },
            Answers(vec![
                Answer::FirstRect(1..1, Some(rect(100., 0., 0., 100.))),
                Answer::FirstRect(2..2, None),
            ]),
            // Asking with a position at the end of selection rects should give you the trailing edge of the last
            // rect.
            CaretRect(1, rect(100., 0., 1., 100.)),
            // Asking with a position beyond the end of selection rects should return CGRectZero without crashing.
            CaretRect(2, CGRect::ZERO),
        ]),
        // The keyboard implementation may send unbalanced calls to the input view; the view takes none.
        row("testFloatingCursorDoesNotThrow", vec![
            RespondsTo("beginFloatingCursorAtPoint:", false),
            RespondsTo("endFloatingCursor", false),
        ]),
        row("testFlutterTokenizerCanParseLines", vec![
            // The tokenizer returns zero range When text is empty.
            Line(0, RIGHT, Some(0..0)),
            Insert("how are you\nI am fine, Thank you"),
            Line(0, RIGHT, Some(0..11)),
            Line(2, RIGHT, Some(0..11)),
            Line(11, RIGHT, Some(0..11)),
            Line(12, RIGHT, Some(12..32)),
            Line(15, RIGHT, Some(12..32)),
            Line(32, RIGHT, Some(12..32)),
        ]),
        row("testFlutterTokenizerLineEnclosingEndOfDocumentInBackwardDirectionShouldNotReturnNil", vec![
            Insert("0123456789\n012345"),
            Line(17, BACKWARD, Some(11..17)),
        ]),
        row("testFlutterTokenizerLineEnclosingEndOfDocumentInForwardDirectionShouldReturnNilOnIOS17", vec![
            Insert("0123456789\n012345"),
            Line(17, FORWARD, None),
        ]),
        row("testFlutterTokenizerLineEnclosingOutOfRangePositionShouldReturnNilOnIOS17", vec![
            Insert("0123456789\n012345"),
            Line(100, FORWARD, None),
        ]),
        // --- Text traits, from TextInputConfiguration ---
        row("testSecureInput", vec![
            configuration(|c| c.secure = true),
            // Verify secureTextEntry is set to the correct value.
            Secure(true),
            // Verify keyboardType is set to the default value.
            KeyboardType(UIKeyboardType::Default),
        ]),
        row("testKeyboardType", vec![
            configuration(|c| c.keyboard_type = gpui::KeyboardType::Url),
            KeyboardType(UIKeyboardType::URL),
        ]),
        row("testKeyboardTypeWebSearch", vec![
            configuration(|c| c.keyboard_type = gpui::KeyboardType::Search),
            KeyboardType(UIKeyboardType::WebSearch),
        ]),
        row("testUpdateSecureTextEntry", vec![
            configuration(|c| c.secure = true),
            Secure(true),
            configuration(|_| ()),
            Secure(false),
        ]),
        // GPUI has a separate `suggestions` switch for spell checking; Flutter's one switch is both fields here.
        row("testDisablingAutocorrectDisablesSpellChecking", vec![
            configuration(|_| ()),
            Autocorrection(UITextAutocorrectionType::Yes),
            SpellChecking(UITextSpellCheckingType::Yes),
            configuration(|c| { c.autocorrect = false; c.suggestions = false }),
            Autocorrection(UITextAutocorrectionType::No),
            SpellChecking(UITextSpellCheckingType::No),
        ]),
        row("testEnableInlinePredictionFromConfiguration", vec![
            configuration(|c| c.suggestions = false),
            InlinePrediction(UITextInlinePredictionType::No),
            configuration(|_| ()),
            InlinePrediction(UITextInlinePredictionType::Yes),
        ]),
        row("testFlutterTextInputViewOnlyRespondsToInsertionPointColorBelowIOS17", vec![
            RespondsTo("insertionPointColor", false),
        ]),
        // --- Update counts: each UITextInput call changes the app once; the app's own changes are not echoed ---
        row("testUITextInputCallsUpdateEditingStateOnce", vec![
            Insert("text to insert"),
            // Update the framework exactly once.
            Edits(1),
            DeleteBackward, Edits(2),
            Select(0..1), Edits(3),
            Replace(0..1, "replace text"), Edits(4),
            SetMarked(Some("marked text"), 0..1), Edits(5),
            Unmark, Edits(6),
        ]),
        row("testTextChangesDoNotTriggerUpdateEditingClient", vec![
            AppSets { text: "BEFORE", selection: 0..0, marked: None },
            Edits(0),
            // Text changes don't trigger an update.
            AppSets { text: "AFTER", selection: 0..0, marked: None },
            AppSets { text: "AFTER", selection: 0..0, marked: None },
            Edits(0),
            // Selection changes don't trigger an update.
            AppSets { text: "SELECTION", selection: 0..3, marked: None },
            AppSets { text: "SELECTION", selection: 1..3, marked: None },
            Edits(0),
            // Composing region changes don't trigger an update.
            AppSets { text: "COMPOSING", selection: 0..0, marked: Some(1..2) },
            AppSets { text: "COMPOSING", selection: 0..0, marked: Some(1..3) },
            Edits(0),
        ]),
        row("testUITextInputAvoidUnnecessaryUndateEditingClientCalls", vec![
            Unmark,
            // updateEditingClient shouldn't fire as the text is already unmarked.
            Edits(0),
            SetMarked(Some("marked text"), 0..1),
            // updateEditingClient fires in response to setMarkedText.
            Edits(1),
            Unmark,
            // updateEditingClient fires in response to unmarkText.
            Edits(2),
        ]),
    ]
}

fn ns_string(text: &str) -> Retained<NSString> {
    NSString::from_str(text)
}

fn first_unit_of(text: &str) -> Retained<NSString> {
    let unit = text.encode_utf16().next().unwrap_or_default();
    // SAFETY: `initWithCharacters:length:` copies `length` UTF-16 units from a valid pointer.
    unsafe { msg_send![NSString::alloc(), initWithCharacters: &unit as *const u16, length: 1usize] }
}

fn check<T: PartialEq + std::fmt::Debug>(what: &str, got: T, want: T) -> Result<(), String> {
    (got == want)
        .then_some(())
        .ok_or_else(|| format!("{what}: got {got:?}, want {want:?}"))
}

fn send_action(view: &TextInputView, name: &str) -> Result<(), String> {
    let selector =
        objc2::runtime::Sel::register(&std::ffi::CString::new(name).map_err(|e| e.to_string())?);
    if !view.respondsToSelector(selector) {
        return Err(format!("does not respond to {name}"));
    }
    // SAFETY: every standard edit action takes one `id sender` and returns nothing.
    let _: *mut AnyObject =
        unsafe { msg_send![view, performSelector: selector, withObject: None::<&AnyObject>] };
    Ok(())
}

fn run_step(view: &TextInputView, field: &Rc<RefCell<Field>>, step: &Step) -> Result<(), String> {
    let range = |range: &Range<usize>| view.new_range(range.clone());
    let offset = |position: Option<Retained<UITextPosition>>| {
        position.and_then(|position| view.validated_offset(&position))
    };
    match step {
        Step::Insert(text) => view.insertText(&ns_string(text)),
        Step::InsertFirstUnitOf(text) => view.insertText(&first_unit_of(text)),
        Step::DeleteBackward => view.deleteBackward(),
        Step::ShouldChange => {
            let selector = sel!(shouldChangeTextInRange:replacementText:);
            if view.respondsToSelector(selector) {
                let empty = ns_string("");
                let _: bool = unsafe {
                    msg_send![view, shouldChangeTextInRange: &*range(&(0..0)), replacementText: &*empty]
                };
            }
        }
        Step::SetMarked(text, selected) => view.setMarkedText_selectedRange(
            text.map(ns_string).as_deref(),
            NSRange::new(selected.start, selected.len()),
        ),
        Step::SetAttributedMarked(text, selected) => {
            let selector = sel!(setAttributedMarkedText:selectedRange:);
            let selected = NSRange::new(selected.start, selected.len());
            if view.respondsToSelector(selector) {
                let text = text.map(|text| NSAttributedString::from_nsstring(&ns_string(text)));
                let _: () = unsafe {
                    msg_send![view, setAttributedMarkedText: text.as_deref(), selectedRange: selected]
                };
            } else {
                // UIKit falls back to the plain-string call when the attributed one is missing.
                view.setMarkedText_selectedRange(text.map(ns_string).as_deref(), selected);
            }
        }
        Step::Unmark => view.unmarkText(),
        Step::Replace(replaced, text) => {
            view.replaceRange_withText(&range(replaced), &ns_string(text))
        }
        Step::Select(selected) => view.setSelectedTextRange(Some(&range(selected))),
        Step::KeyboardVisible(visible) => {
            view.set_keyboard_visible(*visible);
            view.reconcile_text_input(sel!(reconcileTextInput));
        }
        Step::Configure(configuration) => view.set_configuration(configuration),
        Step::Action(name) => send_action(view, name)?,
        Step::WatchDelegate => {
            let watcher = SelectionWatcher::alloc(view.mtm()).set_ivars(Rc::downgrade(field));
            // SAFETY: NSObject's `init` on a freshly allocated object.
            let watcher: Retained<SelectionWatcher> = unsafe { msg_send![super(watcher), init] };
            view.setInputDelegate(Some(ProtocolObject::from_ref(&*watcher)));
            field.borrow_mut().watcher = Some(watcher);
        }
        Step::Notices(want) => {
            check("input delegate notices", std::mem::take(&mut field.borrow_mut().notices), want.clone())?
        }
        Step::PasteboardHoldsColor => {
            // SAFETY: `redColor` returns a UIColor and `setColor:` takes one; both on the main thread.
            unsafe {
                let red: Retained<AnyObject> = msg_send![objc2::class!(UIColor), redColor];
                let _: () = msg_send![&pasteboard(), setColor: &*red];
            }
            check("pasteboard string", unsafe { pasteboard().string() }.is_some(), false)?
        }
        Step::AppSets {
            text,
            selection,
            marked,
        } => {
            {
                let mut field = field.borrow_mut();
                field.text = text.encode_utf16().collect();
                field.selection = selection.clone();
                field.marked = marked.clone();
            }
            view.state_changed(TextInputStateChange::ContentChanged);
            view.reconcile_text_input(sel!(reconcileTextInput));
        }
        Step::Answers(answers) => field.borrow_mut().answers = answers.clone(),
        Step::Text(text) => check(
            "text",
            String::from_utf16_lossy(&field.borrow().text),
            text.to_string(),
        )?,
        Step::Selection(selected) => check(
            "selectedTextRange",
            view.selectedTextRange()
                .and_then(|range| view.validated_range(&range)),
            Some(selected.clone()),
        )?,
        Step::Marked(marked) => check(
            "markedTextRange",
            view.markedTextRange()
                .and_then(|range| view.validated_range(&range)),
            marked.clone(),
        )?,
        Step::Edits(edits) => check("app edits", field.borrow().edits, *edits)?,
        Step::TextInRangeLength(read, length) => check(
            &format!("textInRange {read:?} length"),
            view.textInRange(&range(read))
                .map_or(0, |text| text.length()),
            *length,
        )?,
        Step::DocumentText(text) => {
            let document = view.textRangeFromPosition_toPosition(
                &view.beginningOfDocument(),
                &view.endOfDocument(),
            );
            let got = document
                .and_then(|document| view.textInRange(&document))
                .map(|text| text.to_string());
            check(
                "textInRange (whole document)",
                got,
                text.map(str::to_string),
            )?
        }
        Step::ForeignTextInRange(text) => {
            let foreign = UITextRange::new(view.mtm());
            check(
                "textInRange (foreign range)",
                view.textInRange(&foreign).map(|text| text.to_string()),
                text.map(str::to_string),
            )?
        }
        Step::TextRangeFromPositions(from, to, want) => check(
            "textRangeFromPosition",
            view.textRangeFromPosition_toPosition(
                &view.new_position(*from),
                &view.new_position(*to),
            )
            .and_then(|range| view.validated_range(&range)),
            Some(want.clone()),
        )?,
        Step::FirstRect(asked, want) => check(
            &format!("firstRectForRange {asked:?}"),
            view.firstRectForRange(&range(asked)),
            *want,
        )?,
        Step::CaretRect(at, want) => check(
            &format!("caretRectForPosition {at}"),
            view.caretRectForPosition(&view.new_position(*at)),
            *want,
        )?,
        Step::SelectionRects(asked, want) => {
            let rects = view.selectionRectsForRange(&range(asked));
            let got: Vec<CGRect> = rects.iter().map(|rect| rect.rect()).collect();
            check(
                &format!("selectionRectsForRange {asked:?}"),
                got,
                want.clone(),
            )?
        }
        Step::Closest(at, want) => check(
            &format!("closestPositionToPoint {at:?}"),
            offset(view.closestPositionToPoint(*at)),
            *want,
        )?,
        Step::ClosestWithin(at, within, want) => check(
            &format!("closestPositionToPoint {at:?} withinRange {within:?}"),
            offset(view.closestPositionToPoint_withinRange(*at, &range(within))),
            *want,
        )?,
        Step::Line(at, direction, want) => {
            let tokenizer = view.tokenizer();
            let got = tokenizer
                .rangeEnclosingPosition_withGranularity_inDirection(
                    &view.new_position(*at),
                    UITextGranularity::Line,
                    *direction,
                )
                .and_then(|range| view.validated_range(&range));
            check(
                &format!("line enclosing {at} in direction {direction}"),
                got,
                want.clone(),
            )?
        }
        Step::CanPerform(name, want) => {
            let selector = objc2::runtime::Sel::register(
                &std::ffi::CString::new(*name).map_err(|e| e.to_string())?,
            );
            check(
                &format!("canPerformAction {name}"),
                unsafe { view.canPerformAction_withSender(selector, None) },
                *want,
            )?
        }
        Step::RespondsTo(name, want) => {
            let selector = objc2::runtime::Sel::register(
                &std::ffi::CString::new(*name).map_err(|e| e.to_string())?,
            );
            check(
                &format!("respondsToSelector {name}"),
                view.respondsToSelector(selector),
                *want,
            )?
        }
        Step::KeyboardType(want) => check("keyboardType", view.keyboardType(), *want)?,
        Step::Secure(want) => check("isSecureTextEntry", view.isSecureTextEntry(), *want)?,
        Step::Autocorrection(want) => {
            check("autocorrectionType", view.autocorrectionType(), *want)?
        }
        Step::SpellChecking(want) => check("spellCheckingType", view.spellCheckingType(), *want)?,
        Step::InlinePrediction(want) => {
            check("inlinePredictionType", view.inlinePredictionType(), *want)?
        }
    }
    Ok(())
}

/// Runs every row whose Flutter test name contains `filter` and prints one line per row. Returns whether every
/// `Pass` row passed and no `Open` row did. `attach` makes `view`'s window answer with `handler` and returns what
/// must stay alive while the row runs.
pub(crate) fn run(
    main_thread: MainThreadMarker,
    filter: &str,
    attach: &dyn Fn(&TextInputView, PlatformInputHandler) -> Box<dyn Any>,
) -> bool {
    let mut cx = TestAppContext::single();
    let window = cx.add_empty_window();
    let mut fields = slotmap::SlotMap::<gpui::FocusId, ()>::with_key();
    let mut all_as_expected = true;
    for row in rows()
        .into_iter()
        .filter(|row| row.flutter_test.contains(filter))
    {
        println!("run  {}", row.flutter_test);
        let field = Rc::new(RefCell::new(Field::default()));
        let (_focus, handler) = window.update(|window, cx| {
            let focus = cx.focus_handle();
            window.focus(&focus, cx);
            let handler = PlatformInputHandler::for_focused_field(
                window.to_async(cx),
                Box::new(FakeApp(field.clone())),
                &focus,
            );
            (focus, handler)
        });
        let view = TextInputView::new(CGRect::ZERO, main_thread);
        let _attached = attach(&view, handler);
        view.set_input_field(Some(fields.insert(())));
        let result = objc2::rc::autoreleasepool(|_| {
            row.steps.iter().enumerate().try_for_each(|(index, step)| {
                run_step(&view, &field, step)
                    .map_err(|error| format!("step {}: {error}", index + 1))
            })
        });
        let (verdict, as_expected) = match (&row.status, &result) {
            (Status::Pass, Ok(())) => ("ok".to_string(), true),
            (Status::Pass, Err(error)) => (format!("FAIL {error}"), false),
            (Status::Open(why), Err(error)) => (format!("open ({why}): {error}"), true),
            (Status::Open(_), Ok(())) => ("PASSES (mark the row Pass)".to_string(), false),
        };
        println!("{verdict:<4} {}", row.flutter_test);
        all_as_expected &= as_expected;
    }
    all_as_expected
}
