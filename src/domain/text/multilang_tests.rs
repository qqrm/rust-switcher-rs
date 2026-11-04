//! Native planner integration with the catalog prepared by initialize-layouts.ps1.
//! The tests resolve real HKLs and exercise the public layout provider and models.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use windows::{
    Win32::UI::Input::KeyboardAndMouse::{KLF_NOTELLSHELL, LoadKeyboardLayoutW},
    core::PCWSTR,
};

use super::*;

struct Catalog {
    installed: Vec<LayoutId>,
    english: LayoutId,
    international: LayoutId,
    russian: LayoutId,
    ukrainian: LayoutId,
    german: LayoutId,
    french: LayoutId,
}

fn catalog() -> Catalog {
    // Capture availability before resolving handles. A successful native lookup
    // alone must not make an absent layout eligible for these integration tests.
    let installed = layouts::installed_layouts();
    for language in [0x0409, 0x0419, 0x0422, 0x0407, 0x040c, 0x0415] {
        assert!(
            installed.iter().any(|layout| layout.0 & 0xffff == language),
            "initialize-layouts.ps1 -Mode All must prepare language 0x{language:04x} in the hosted Windows job; observed {installed:?}"
        );
    }
    assert_eq!(
        installed.len(),
        7,
        "the isolated All fixture must include six languages and both English variants"
    );
    let english = loaded_layout(&installed, "00000409");
    let international = loaded_layout(&installed, "00020409");
    let russian = loaded_layout(&installed, "00000419");
    let ukrainian = loaded_layout(&installed, "00000422");
    let german = loaded_layout(&installed, "00000407");
    let french = loaded_layout(&installed, "0000040C");
    let _polish = loaded_layout(&installed, "00000415");
    assert_ne!(english, international);

    // Separate cold-start timing from language-selection assertions. This waits
    // through the public provider's bounded request, without sleeps or retries.
    assert_eq!(
        layouts::convert_text(".", english, russian),
        Some("ю".to_owned())
    );
    let languages = installed
        .iter()
        .copied()
        .filter_map(layouts::language_tag)
        .collect::<Vec<_>>();
    ranking::warm_languages(&languages);

    Catalog {
        installed,
        english,
        international,
        russian,
        ukrainian,
        german,
        french,
    }
}

fn loaded_layout(installed: &[LayoutId], name: &str) -> LayoutId {
    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let handle = unsafe { LoadKeyboardLayoutW(PCWSTR(wide.as_ptr()), KLF_NOTELLSHELL) }
        .expect("the configured native layout must resolve");
    let layout = LayoutId(handle.0 as usize);
    assert!(
        installed.contains(&layout),
        "layout {name} must already be visible in GetKeyboardLayoutList, not merely return an HKL"
    );
    layout
}

fn physical(text: &str, layout: LayoutId, kind: RunKind) -> InputRun {
    InputRun {
        text: text.to_owned(),
        layout: LayoutTag::Native(layout.0),
        origin: RunOrigin::Physical,
        kind,
    }
}

#[test]
#[ignore = "requires the seven-layout fixture on a disposable hosted Windows desktop"]
fn native_planner_full_catalog_selects_ukrainian_french_and_german() {
    let _guard = layouts::test_guard();
    let catalog = catalog();
    for (source, expected, target) in [
        ("ghbdsn", "привіт", catalog.ukrainian),
        ("sqlut", "salut", catalog.french),
        ("ywei", "zwei", catalog.german),
        ("ghbdsn zr cghfdb", "привіт як справи", catalog.ukrainian),
    ] {
        // Use the public planner entry: it must discover the complete live
        // catalog itself, including candidates from the other five layouts.
        let plan = plan_text(source, Some(catalog.english), Policy::Manual)
            .unwrap_or_else(|| panic!("the warmed planner must convert {source:?}"));
        assert_eq!(plan.text, expected, "source {source:?}");
        assert_eq!(plan.target, target, "source {source:?}");
    }
}

#[test]
#[ignore = "requires the seven-layout fixture on a disposable hosted Windows desktop"]
fn native_planner_automatic_abstains_between_valid_russian_and_ukrainian_words() {
    let _guard = layouts::test_guard();
    let catalog = catalog();
    // Both interpretations are valid words: Russian "вырыть" and Ukrainian
    // "віріть". Verify the physical premise through the native provider before
    // testing the ranking policy, rather than assuming the two mappings.
    for (target, expected) in [(catalog.russian, "вырыть"), (catalog.ukrainian, "віріть")]
    {
        assert_eq!(
            layouts::convert_text("dshsnm", catalog.english, target).as_deref(),
            Some(expected)
        );
    }
    assert!(plan_text("dshsnm", Some(catalog.english), Policy::Automatic).is_none());
}

#[test]
#[ignore = "requires the seven-layout fixture on a disposable hosted Windows desktop"]
fn native_planner_automatic_recovers_ukrainian_when_russian_keys_produce_gibberish() {
    let _guard = layouts::test_guard();
    let catalog = catalog();
    // US s maps to Russian ы, so this does not compete with Russian "привит".
    for (target, expected) in [(catalog.russian, "привыт"), (catalog.ukrainian, "привіт")]
    {
        assert_eq!(
            layouts::convert_text("ghbdsn", catalog.english, target).as_deref(),
            Some(expected)
        );
    }
    let plan = plan_text("ghbdsn", Some(catalog.english), Policy::Automatic)
        .expect("unambiguous Ukrainian input must retain automatic recovery");
    assert_eq!(plan.text, "привіт");
    assert_eq!(plan.target, catalog.ukrainian);
}

#[test]
#[ignore = "requires the seven-layout fixture on a disposable hosted Windows desktop"]
fn native_planner_smart_keeps_valid_originals_and_does_not_select_an_identity_variant() {
    let _guard = layouts::test_guard();
    let catalog = catalog();
    for (text, source) in [
        ("hello world", catalog.english),
        ("привет как дела", catalog.russian),
        ("привіт як справи", catalog.ukrainian),
        ("die katze spielt draussen", catalog.german),
        ("salut tout le monde", catalog.french),
    ] {
        assert!(
            plan_text(text, Some(source), Policy::Smart).is_none(),
            "a correct {source:?} original must not produce a text or layout change: {text:?}"
        );
    }
}

#[test]
#[ignore = "requires the seven-layout fixture on a disposable hosted Windows desktop"]
fn native_planner_pair_converts_the_first_character_and_round_trips_punctuation() {
    let _guard = layouts::test_guard();
    let catalog = catalog();
    let pair = [catalog.english, catalog.russian];
    for (source, expected) in [(".", "ю"), (".ghbdtn", "юпривет"), (".Ghbdtn", "юПривет")]
    {
        let plan = plan_text_with(source, Some(catalog.english), Policy::Manual, &pair)
            .expect("an explicit two-layout command must not require language evidence");
        assert_eq!(plan.text, expected);
        assert_eq!(plan.target, catalog.russian);
        let reverse = plan_text_with(&plan.text, Some(plan.target), Policy::Manual, &pair)
            .expect("the exact returned target must be usable as the next source");
        assert_eq!(reverse.text, source);
        assert_eq!(reverse.target, catalog.english);
    }
}

#[test]
#[ignore = "requires the seven-layout fixture on a disposable hosted Windows desktop"]
fn native_planner_mixed_source_runs_keep_each_physical_layout_on_repeated_conversion() {
    let _guard = layouts::test_guard();
    let catalog = catalog();
    let pair = [catalog.english, catalog.russian];
    let original = [
        physical(".ghbdtn", catalog.english, RunKind::Text),
        physical(" ", catalog.english, RunKind::Whitespace),
        physical("руддщ", catalog.russian, RunKind::Text),
    ];
    let first = plan_runs_with(&original, Policy::Manual, &pair)
        .expect("both captured source layouts must be converted independently");
    assert_eq!(first.text, "юпривет hello");
    assert_eq!(first.target, catalog.english);
    assert_eq!(first.runs.len(), 3);
    assert_eq!(first.runs[0].layout, LayoutTag::Native(catalog.russian.0));
    assert_eq!(first.runs[1].layout, LayoutTag::Native(catalog.russian.0));
    assert_eq!(first.runs[1].kind, RunKind::Whitespace);
    assert_eq!(first.runs[2].layout, LayoutTag::Native(catalog.english.0));
    let second = plan_runs_with(&first.runs, Policy::Manual, &pair)
        .expect("programmatic runs must retain enough layout identity for a second command");
    assert_eq!(second.text, ".ghbdtn руддщ");
    assert_eq!(second.target, catalog.russian);
    assert_eq!(second.runs[0].layout, original[0].layout);
    assert_eq!(second.runs[2].layout, original[2].layout);
}

#[test]
#[ignore = "requires the seven-layout fixture on a disposable hosted Windows desktop"]
fn native_planner_resegments_dead_key_output_when_composition_consumes_a_space() {
    let _guard = layouts::test_guard();
    let catalog = catalog();
    let pair = [catalog.english, catalog.german];
    let original = [
        physical("`", catalog.english, RunKind::Text),
        physical(" ", catalog.english, RunKind::Whitespace),
        physical("e", catalog.english, RunKind::Text),
    ];
    let plan = plan_runs_with(&original, Policy::Manual, &pair)
        .expect("dead-key translation must preserve composition across source run boundaries");
    assert_eq!(plan.text, "^e");
    assert_eq!(plan.target, catalog.german);
    assert_eq!(
        plan.runs,
        vec![InputRun {
            text: "^e".to_owned(),
            layout: LayoutTag::Native(catalog.german.0),
            origin: RunOrigin::Programmatic,
            kind: RunKind::Text,
        }]
    );
}

#[test]
#[ignore = "requires the seven-layout fixture on a disposable hosted Windows desktop"]
fn native_planner_preserves_full_hkl_between_standard_and_international_english() {
    let _guard = layouts::test_guard();
    let catalog = catalog();
    assert_eq!(
        layouts::language_tag(catalog.english),
        layouts::language_tag(catalog.international)
    );
    let pair = [catalog.english, catalog.international];
    let original = [physical("^e", catalog.english, RunKind::Text)];
    let plan = plan_runs_with(&original, Policy::Manual, &pair)
        .expect("same-language layout variants must remain distinct conversion targets");
    assert_eq!(plan.text, "ê");
    assert_eq!(plan.target, catalog.international);
    assert_eq!(
        plan.runs[0].layout,
        LayoutTag::Native(catalog.international.0)
    );
    let reverse = plan_runs_with(&plan.runs, Policy::Manual, &pair)
        .expect("the composed character must recover through its recorded variant");
    assert_eq!(reverse.text, "^e");
    assert_eq!(reverse.target, catalog.english);
}

#[test]
#[ignore = "requires the seven-layout fixture on a disposable hosted Windows desktop"]
fn native_planner_smart_keeps_the_layout_of_an_unchanged_tail_after_an_earlier_correction() {
    let _guard = layouts::test_guard();
    let catalog = catalog();
    let original = [
        physical("ghbdtn", catalog.english, RunKind::Text),
        physical(" ", catalog.english, RunKind::Whitespace),
        physical("zwei", catalog.german, RunKind::Text),
    ];
    let plan = plan_runs_with(&original, Policy::Smart, &catalog.installed)
        .expect("the wrong-layout prefix should be corrected without changing the valid tail");
    assert_eq!(plan.text, "привет zwei");
    assert_eq!(plan.runs.last(), original.last());
    assert_eq!(
        plan.target, catalog.german,
        "subsequent typing follows the final preserved German run, not an earlier correction"
    );
}
