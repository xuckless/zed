// Ported from flutter/flutter engine/src/flutter/lib/web_ui/lib/src/engine/text_editing/ @ 3.50.1
// (ae9b586f34a8443573f82ef57c3bf914a908cbf7): input_type.dart (`inputmodeAttribute`, `createDomElement`),
// input_action.dart (`configureInputAction`), text_capitalization.dart (`setAutocapitalizeAttribute`),
// autofill_hint.dart (`BrowserAutofillHints`) and text_editing.dart (`DefaultTextEditingStrategy.applyConfiguration`,
// `obscureText`); the test rows
// from engine/src/flutter/lib/web_ui/test/engine/text_editing_test.dart @ 3.50.1. Copyright 2014 The Flutter
// Authors. BSD-3-Clause, see third_party/flutter/LICENSE.

//! What the IME mirror's attributes are for a [`TextInputConfiguration`], and what edit an `input` event made to
//! it. Plain values in, plain values out, so Flutter's web text-editing cases run as tables on any host; the DOM
//! side is `ime_mirror.rs` and `events.rs`.
//!
//! Flutter gives each field its own `<input>` or `<textarea>`; GPUI keeps one mirror, a `<textarea>` that becomes
//! an `<input type=password>` while the focused field is secure ([`element_kind`]).

use std::ops::Range;

use gpui::{Autocapitalize, AutofillHint, KeyboardType, TextInputAction, TextInputConfiguration};

/// The mirror's text-assistance attributes for `configuration`, as (name, value); `None` removes the attribute.
/// `mobile` is Flutter's `operatingSystem == iOs || android` test, which gates `enterkeyhint`
/// (`configureInputAction`: "Only apply `enterkeyhint` in mobile browsers"). To add an attribute: a row.
pub(crate) fn attributes(
    configuration: &TextInputConfiguration,
    mobile: bool,
) -> [(&'static str, Option<&'static str>); 5] {
    let on_off = |enabled: bool| Some(if enabled { "on" } else { "off" });
    [
        // `applyConfiguration`: the autofill hint's token, or "off" when the field has none.
        (
            "autocomplete",
            Some(configuration.autofill.map_or("off", autocomplete_token)),
        ),
        ("autocorrect", on_off(configuration.autocorrect)),
        // `setAutocapitalizeAttribute`.
        (
            "autocapitalize",
            Some(match configuration.autocapitalize {
                Autocapitalize::None => "off",
                Autocapitalize::Words => "words",
                Autocapitalize::Sentences => "sentences",
                Autocapitalize::Characters => "characters",
            }),
        ),
        (
            "enterkeyhint",
            enter_key_hint(configuration.input_action).filter(|_| mobile),
        ),
        // GPUI's own: Flutter's framework sends no spellcheck flag to the web engine.
        (
            "spellcheck",
            Some(if configuration.suggestions {
                "true"
            } else {
                "false"
            }),
        ),
    ]
}

/// `EngineInputAction.enterkeyhintAttribute`; `Unspecified` sets none.
fn enter_key_hint(action: TextInputAction) -> Option<&'static str> {
    match action {
        TextInputAction::Unspecified => None,
        TextInputAction::Enter => Some("enter"),
        TextInputAction::Done => Some("done"),
        TextInputAction::Go => Some("go"),
        TextInputAction::Next => Some("next"),
        TextInputAction::Previous => Some("previous"),
        TextInputAction::Search => Some("search"),
        TextInputAction::Send => Some("send"),
    }
}

/// `BrowserAutofillHints`' `_flutterToEngineMap`, for the hints GPUI has.
fn autocomplete_token(hint: AutofillHint) -> &'static str {
    match hint {
        AutofillHint::Username => "username",
        AutofillHint::Password => "current-password",
        AutofillHint::NewPassword => "new-password",
        AutofillHint::OneTimeCode => "one-time-code",
        AutofillHint::Email => "email",
        AutofillHint::Phone => "tel",
        AutofillHint::Name => "name",
        AutofillHint::PostalAddress => "street-address",
        AutofillHint::PostalCode => "postal-code",
        AutofillHint::CreditCardNumber => "cc-number",
    }
}

/// `inputmodeAttribute` of each `EngineInputType`: the software keyboard layout the mirror asks for while the
/// keyboard is enabled (`inputmode=none` disables it). Flutter's plain text sets no attribute; here "text" is
/// written, since the mirror switches back from "none". `Search` is GPUI's own (Flutter has no search type).
pub(crate) fn input_mode(keyboard_type: KeyboardType) -> &'static str {
    match keyboard_type {
        KeyboardType::Text => "text",
        KeyboardType::Number => "numeric",
        KeyboardType::Decimal => "decimal",
        KeyboardType::Phone => "tel",
        KeyboardType::Email => "email",
        KeyboardType::Url => "url",
        KeyboardType::Search => "search",
    }
}

/// The element the mirror is. Flutter's `createDomElement` makes an `<input>` for single-line fields and a
/// `<textarea>` for multiline ones, and `applyConfiguration` sets `type=password` when `obscureText`, so keyboards
/// neither show nor learn what is typed. GPUI's configuration has no multiline flag and any field may hold newlines
/// (which an `<input>` strips), so the mirror stays a `<textarea>` except for secure fields, which are single-line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ElementKind {
    TextArea,
    /// `<input type=password>`. Always `password` while secure: Flutter's `AutofillInfo.applyToDomElement` resets
    /// an `<input>` to `type=text` for a hint without "password" in it, which a secure field must not get.
    Password,
}

/// The element `configuration` needs; `ImeMirror::apply_configuration` swaps the mirror when it changes.
pub(crate) fn element_kind(configuration: &TextInputConfiguration) -> ElementKind {
    if configuration.secure {
        ElementKind::Password
    } else {
        ElementKind::TextArea
    }
}

/// An IME edit of the mirror: the UTF-16 range `replaced` of the old value became `inserted`.
#[derive(Debug, PartialEq)]
pub(crate) struct Edit {
    pub(crate) replaced: Range<usize>,
    pub(crate) inserted: String,
}

/// The edit that turned the mirror's `old` value into `new`, the browser having left the caret at
/// `post_edit_caret` (UTF-16). Flutter infers the same thing (`TextEditingDeltaState.inferDeltaState`); GPUI's
/// rule, which the `edits` table checks against Flutter's cases:
///
/// A prefix/suffix diff is ambiguous when the inserted text shares characters with what follows it (inserting
/// "pactor " before "pact" also reads as inserting "or pact" four units later). The edit's true position is not
/// ambiguous: the browser leaves the caret at the end of an IME edit, so the suffix is anchored as "everything
/// after the post-edit caret", and the prefix is capped to fit. Greedy matching is only a fallback for edits
/// where the anchored suffix doesn't verify.
pub(crate) fn infer_edit(old: &str, new: &str, post_edit_caret: Option<usize>) -> Edit {
    let old_units: Vec<u16> = old.encode_utf16().collect();
    let new_units: Vec<u16> = new.encode_utf16().collect();
    let anchored_suffix_length = post_edit_caret
        .map(|caret| new_units.len().saturating_sub(caret))
        .filter(|&suffix_length| {
            suffix_length <= old_units.len()
                && old_units[old_units.len() - suffix_length..]
                    == new_units[new_units.len() - suffix_length..]
        });
    let suffix_length = anchored_suffix_length.unwrap_or_else(|| {
        old_units
            .iter()
            .rev()
            .zip(new_units.iter().rev())
            .take_while(|(old_unit, new_unit)| old_unit == new_unit)
            .count()
    });
    let prefix_length = old_units
        .iter()
        .zip(&new_units)
        .take_while(|(old_unit, new_unit)| old_unit == new_unit)
        .count()
        .min(old_units.len() - suffix_length)
        .min(new_units.len() - suffix_length);
    Edit {
        replaced: prefix_length..old_units.len() - suffix_length,
        inserted: String::from_utf16_lossy(
            &new_units[prefix_length..new_units.len() - suffix_length],
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// text_editing_test.dart's `GloballyPositionedTextEditingStrategy` and `HybridTextEditing` configuration
    /// cases: (Flutter test, configuration, mobile, attribute, expected value). A row per checked attribute.
    #[test]
    fn configuration_attributes() {
        let default = TextInputConfiguration::default;
        let rows: [(&str, TextInputConfiguration, bool, &str, Option<&str>); 14] = [
            (
                "Knows how to create non-default text actions (mobile)",
                TextInputConfiguration {
                    input_action: TextInputAction::Send,
                    ..default()
                },
                true,
                "enterkeyhint",
                Some("send"),
            ),
            (
                "Knows how to create non-default text actions (desktop)",
                TextInputConfiguration {
                    input_action: TextInputAction::Send,
                    ..default()
                },
                false,
                "enterkeyhint",
                None,
            ),
            (
                "Knows to turn autocorrect off",
                TextInputConfiguration {
                    autocorrect: false,
                    ..default()
                },
                false,
                "autocorrect",
                Some("off"),
            ),
            (
                "Knows to turn autocorrect on",
                TextInputConfiguration {
                    autocorrect: true,
                    ..default()
                },
                false,
                "autocorrect",
                Some("on"),
            ),
            (
                "Knows to turn autofill off",
                default(),
                false,
                "autocomplete",
                Some("off"),
            ),
            (
                "input with autofill hint",
                TextInputConfiguration {
                    autofill: Some(AutofillHint::Email),
                    ..default()
                },
                false,
                "autocomplete",
                Some("email"),
            ),
            (
                "password autofill hint",
                TextInputConfiguration {
                    autofill: Some(AutofillHint::Password),
                    ..default()
                },
                false,
                "autocomplete",
                Some("current-password"),
            ),
            (
                "No capitalization: setClient, setEditingState, show",
                default(),
                true,
                "autocapitalize",
                Some("off"),
            ),
            (
                "All characters capitalization: setClient, setEditingState, show",
                TextInputConfiguration {
                    autocapitalize: Autocapitalize::Characters,
                    ..default()
                },
                true,
                "autocapitalize",
                Some("characters"),
            ),
            (
                "Sentences capitalization: setClient, setEditingState, show",
                TextInputConfiguration {
                    autocapitalize: Autocapitalize::Sentences,
                    ..default()
                },
                true,
                "autocapitalize",
                Some("sentences"),
            ),
            (
                "Words capitalization: setClient, setEditingState, show",
                TextInputConfiguration {
                    autocapitalize: Autocapitalize::Words,
                    ..default()
                },
                true,
                "autocapitalize",
                Some("words"),
            ),
            (
                "GPUI: suggestions off turn spellcheck off",
                default(),
                false,
                "spellcheck",
                Some("false"),
            ),
            (
                "GPUI: suggestions on turn spellcheck on",
                TextInputConfiguration {
                    suggestions: true,
                    ..default()
                },
                false,
                "spellcheck",
                Some("true"),
            ),
            (
                "GPUI: an unspecified action sets no enterkeyhint",
                default(),
                true,
                "enterkeyhint",
                None,
            ),
        ];
        for (name, configuration, mobile, attribute, expected) in rows {
            let value = attributes(&configuration, mobile)
                .into_iter()
                .find(|(key, _)| *key == attribute)
                .map(|(_, value)| value);
            assert_eq!(value, Some(expected), "{name}");
        }
    }

    /// "sets correct input type in Android" / "… in iOS" / "… for Firefox on Android": each keyboard type's
    /// `inputmode`.
    #[test]
    fn input_modes() {
        let rows = [
            (KeyboardType::Text, "text"),
            (KeyboardType::Number, "numeric"),
            (KeyboardType::Decimal, "decimal"),
            (KeyboardType::Phone, "tel"),
            (KeyboardType::Email, "email"),
            (KeyboardType::Url, "url"),
            (KeyboardType::Search, "search"),
        ];
        for (keyboard_type, expected) in rows {
            assert_eq!(input_mode(keyboard_type), expected, "{keyboard_type:?}");
        }
    }

    /// "Knows how to create password fields" and "password autofill hint" (`type=password`), plus GPUI's rows:
    /// (name, configuration, element).
    #[test]
    fn element_kinds() {
        let default = TextInputConfiguration::default;
        let rows = [
            (
                "Knows how to create password fields",
                TextInputConfiguration {
                    secure: true,
                    ..default()
                },
                ElementKind::Password,
            ),
            (
                "password autofill hint",
                TextInputConfiguration {
                    secure: true,
                    autofill: Some(AutofillHint::Password),
                    ..default()
                },
                ElementKind::Password,
            ),
            (
                "GPUI: a secure field with a hint without \"password\" stays a password field",
                TextInputConfiguration {
                    secure: true,
                    autofill: Some(AutofillHint::OneTimeCode),
                    ..default()
                },
                ElementKind::Password,
            ),
            (
                "GPUI: a password hint on a field that isn't secure keeps the textarea",
                TextInputConfiguration {
                    autofill: Some(AutofillHint::Password),
                    ..default()
                },
                ElementKind::TextArea,
            ),
            (
                "GPUI: a plain field is a textarea (it may hold newlines)",
                default(),
                ElementKind::TextArea,
            ),
        ];
        for (name, configuration, expected) in rows {
            assert_eq!(element_kind(&configuration), expected, "{name}");
        }
    }

    /// text_editing_test.dart's `TextEditingDeltaState` cases (Flutter's expected `deltaStart`..`deltaEnd` and
    /// `deltaText`), plus GPUI's own ambiguity row: (name, old value, new value, post-edit caret, replaced, inserted).
    #[test]
    fn edits() {
        let rows: [(&str, &str, &str, usize, Range<usize>, &str); 8] = [
            (
                "Verify correct delta is inferred - insertion",
                "worl",
                "world",
                5,
                4..4,
                "d",
            ),
            (
                "Verify correct delta is inferred - Backward deletion - Empty selection",
                "world",
                "worl",
                4,
                4..5,
                "",
            ),
            (
                "Verify correct delta is inferred - Forward deletion - Empty selection",
                "world",
                "worl",
                4,
                4..5,
                "",
            ),
            (
                "Verify correct delta is inferred - Deletion - Non-empty selection",
                "world",
                "w",
                1,
                1..5,
                "",
            ),
            (
                "Verify correct delta is inferred - composing region replacement",
                "ni hao ma",
                "你好吗",
                3,
                0..9,
                "你好吗",
            ),
            (
                "Verify correct delta is inferred for double space to insert a period",
                "hello ",
                "hello. ",
                7,
                5..6,
                ". ",
            ),
            (
                "Verify correct delta is inferred for accent menu",
                "a",
                "à",
                1,
                0..1,
                "à",
            ),
            (
                "GPUI: the caret anchors an ambiguous insertion",
                "pact",
                "pactor pact",
                7,
                0..0,
                "pactor ",
            ),
        ];
        for (name, old, new, caret, replaced, inserted) in rows {
            assert_eq!(
                infer_edit(old, new, Some(caret)),
                Edit {
                    replaced,
                    inserted: inserted.to_string()
                },
                "{name}"
            );
        }
    }
}
