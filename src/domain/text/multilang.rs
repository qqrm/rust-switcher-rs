//! Conversion plans use the layouts currently installed in Windows.
//!
//! The worker produces data only. The caller must revalidate its input target
//! before applying a plan; a late result can never inject input by itself.

#[cfg(test)]
#[path = "multilang_tests.rs"]
mod integration_tests;

use std::{
    sync::OnceLock,
    thread,
    time::{Duration, Instant},
};

use super::ranking::{self, Candidate, RankingMode};
use crate::{
    input_journal::{InputRun, LayoutTag, RunKind, RunOrigin},
    platform::win::layouts::{self, LayoutId},
    utils::bounded_worker::BoundedWorker,
};

const PLAN_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Policy {
    Manual,
    Smart,
    Automatic,
}

#[derive(Clone, Debug)]
pub(super) struct Plan {
    pub text: String,
    pub target: LayoutId,
    pub runs: Vec<InputRun>,
}

enum Request {
    Text(String, Option<LayoutId>, Option<LayoutId>, Policy, Instant),
    Runs(Vec<InputRun>, Policy, Instant),
}

fn worker() -> Option<&'static BoundedWorker<Request, Option<Plan>>> {
    static WORKER: OnceLock<Option<BoundedWorker<Request, Option<Plan>>>> = OnceLock::new();
    WORKER
        .get_or_init(|| BoundedWorker::start("layout-planner", || process).ok())
        .as_ref()
}

/// Preload only the language models corresponding to the current input locales.
pub(crate) fn warm() {
    layouts::warm_layouts();
    let _ = thread::Builder::new()
        .name("layout-preload".to_owned())
        .spawn(|| {
            let languages: Vec<_> = layouts::installed_layouts()
                .into_iter()
                .filter_map(layouts::language_tag)
                .collect();
            ranking::warm_languages(&languages);
        });
}

pub(super) fn plan_text(text: &str, source: Option<LayoutId>, policy: Policy) -> Option<Plan> {
    let fallback = (source.is_none() && policy == Policy::Manual)
        .then(layouts::foreground_layout)
        .flatten();
    worker()?.request(
        Request::Text(
            text.to_owned(),
            source,
            fallback,
            policy,
            Instant::now() + PLAN_TIMEOUT,
        ),
        PLAN_TIMEOUT,
    )?
}

pub(super) fn plan_runs(runs: &[InputRun], policy: Policy) -> Option<Plan> {
    worker()?.request(
        Request::Runs(runs.to_vec(), policy, Instant::now() + PLAN_TIMEOUT),
        PLAN_TIMEOUT,
    )?
}

fn process(request: Request) -> Option<Plan> {
    // Query on every request: adding/removing a Windows layout needs no app setting
    // and no restart. A cached translation table never makes a removed layout eligible.
    let installed = layouts::installed_layouts();
    if installed.len() < 2 {
        return None;
    }
    match request {
        Request::Text(text, source, fallback, policy, deadline) => {
            plan_text_before(&text, source, policy, &installed, deadline).or_else(|| {
                // Source inference and its fallback share one deadline and one
                // caller wait, including when the native provider is stalled.
                plan_text_before(&text, Some(fallback?), policy, &installed, deadline)
            })
        }
        Request::Runs(runs, policy, deadline) => {
            plan_runs_before(&runs, policy, &installed, deadline)
        }
    }
}

#[cfg(test)]
fn plan_text_with(
    text: &str,
    source: Option<LayoutId>,
    policy: Policy,
    installed: &[LayoutId],
) -> Option<Plan> {
    plan_text_before(
        text,
        source,
        policy,
        installed,
        Instant::now() + PLAN_TIMEOUT,
    )
}

fn plan_text_before(
    text: &str,
    source: Option<LayoutId>,
    policy: Policy,
    installed: &[LayoutId],
    deadline: Instant,
) -> Option<Plan> {
    if text.is_empty() || installed.len() < 2 {
        return None;
    }
    let known_source = source.filter(|layout| installed.contains(layout));
    // A removed known layout must not be reinterpreted as some other source.
    if source.is_some() && known_source.is_none() {
        return None;
    }
    let sources = known_source.map_or_else(|| installed.to_vec(), |layout| vec![layout]);
    let mut plans = Vec::new();
    let mut candidates = Vec::new();
    for source in sources {
        let source_index = installed.iter().position(|layout| *layout == source)?;
        for offset in 1..installed.len() {
            // A cancelled request must not keep trying every source/target pair
            // after one slow native call has exhausted the caller's budget.
            if Instant::now() >= deadline {
                return None;
            }
            let target = installed[(source_index + offset) % installed.len()];
            let Some(converted) = layouts::convert_text(text, source, target) else {
                continue;
            };
            if plans
                .iter()
                .any(|plan: &Plan| plan.target == target && plan.text == converted)
            {
                continue;
            }
            candidates.push(Candidate {
                text: converted.clone(),
                language: layouts::language_tag(target),
            });
            plans.push(Plan {
                text: converted,
                target,
                runs: Vec::new(),
            });
        }
    }
    if plans.is_empty() {
        return None;
    }
    if policy == Policy::Manual
        && known_source.is_none()
        && plans.iter().all(|plan| plan.text == text)
    {
        return None;
    }
    if policy == Policy::Manual && plans.iter().any(|plan| plan.text != text) {
        let mut index = 0;
        plans.retain(|plan| {
            let keep = plan.text != text;
            if !keep {
                candidates.remove(index);
            } else {
                index += 1;
            }
            keep
        });
    }
    if policy == Policy::Manual && plans.len() == 1 {
        return plans.pop();
    }
    let converted_count = plans.len();
    if policy != Policy::Manual {
        // Correct words in any installed language can keep the original text.
        // Language and keyboard layout are deliberately separate here.
        for layout in installed {
            candidates.push(Candidate {
                text: text.to_owned(),
                language: layouts::language_tag(*layout),
            });
        }
    }
    let mode = if policy == Policy::Automatic {
        RankingMode::Automatic
    } else {
        RankingMode::Manual
    };
    match ranking::rank_candidates(text, &candidates, mode) {
        Some(index)
            if index < converted_count
                && (policy == Policy::Manual || plans[index].text != text) =>
        {
            Some(plans.swap_remove(index))
        }
        // Explicit commands have a deterministic fallback, including punctuation.
        // Ambiguous automatic/smart requests leave the original untouched.
        None if policy == Policy::Manual && known_source.is_some() => Some(plans.remove(0)),
        _ => None,
    }
}

#[cfg(test)]
fn plan_runs_with(runs: &[InputRun], policy: Policy, installed: &[LayoutId]) -> Option<Plan> {
    plan_runs_before(runs, policy, installed, Instant::now() + PLAN_TIMEOUT)
}

fn plan_runs_before(
    runs: &[InputRun],
    policy: Policy,
    installed: &[LayoutId],
    deadline: Instant,
) -> Option<Plan> {
    let mut result = Vec::with_capacity(runs.len());
    let mut planned = false;
    let mut start = 0;
    while start < runs.len() {
        let source = match runs[start].layout {
            LayoutTag::Native(raw) => LayoutId(raw),
            // Production capture always retains the complete layout handle.
            // Do not guess the physical source of pasted/unknown text.
            _ => return None,
        };
        let end = runs[start..]
            .iter()
            .position(|run| run.layout != runs[start].layout)
            .map_or(runs.len(), |offset| start + offset);
        let text: String = runs[start..end]
            .iter()
            .map(|run| run.text.as_str())
            .collect();
        if let Some(plan) = plan_text_before(&text, Some(source), policy, installed, deadline) {
            result.extend(segment_result(&plan.text, plan.target));
            planned = true;
        } else {
            if policy == Policy::Manual {
                return None;
            }
            result.extend_from_slice(&runs[start..end]);
        }
        start = end;
    }
    if !planned {
        return None;
    }
    // If a smart conversion preserves a correct final group, subsequent typing
    // must keep that group's layout rather than an earlier corrected group's.
    let LayoutTag::Native(target) = result.last()?.layout else {
        return None;
    };
    Some(Plan {
        text: result.iter().map(|run| run.text.as_str()).collect(),
        target: LayoutId(target),
        runs: result,
    })
}

fn segment_result(text: &str, layout: LayoutId) -> Vec<InputRun> {
    let mut runs: Vec<InputRun> = Vec::new();
    for ch in text.chars() {
        let kind = if ch.is_whitespace() {
            RunKind::Whitespace
        } else {
            RunKind::Text
        };
        if let Some(run) = runs.last_mut().filter(|run| run.kind == kind) {
            run.text.push(ch);
        } else {
            runs.push(InputRun {
                text: ch.to_string(),
                layout: LayoutTag::Native(layout.0),
                origin: RunOrigin::Programmatic,
                kind,
            });
        }
    }
    runs
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use windows::{
        Win32::UI::Input::KeyboardAndMouse::{HKL, KLF_NOTELLSHELL, LoadKeyboardLayoutW},
        core::w,
    };

    use super::*;

    fn test_layouts() -> [LayoutId; 2] {
        let en = unsafe { LoadKeyboardLayoutW(w!("00000409"), KLF_NOTELLSHELL) }.unwrap();
        let ru = unsafe { LoadKeyboardLayoutW(w!("00000419"), KLF_NOTELLSHELL) }.unwrap();
        assert_ne!(en, HKL::default());
        assert_ne!(ru, HKL::default());
        [LayoutId(en.0 as usize), LayoutId(ru.0 as usize)]
    }

    #[test]
    fn explicit_conversion_preserves_leading_punctuation_and_exact_target() {
        let _guard = layouts::test_guard();
        let installed = test_layouts();
        let plan =
            plan_text_with(".ghbdtn", Some(installed[0]), Policy::Manual, &installed).unwrap();
        assert_eq!(plan.text, "юпривет");
        assert_eq!(plan.target, installed[1]);
        let reverse =
            plan_text_with(&plan.text, Some(plan.target), Policy::Manual, &installed).unwrap();
        assert_eq!(reverse.text, ".ghbdtn");
    }

    #[test]
    fn changed_layout_set_is_respected_without_cached_target_fallback() {
        let _guard = layouts::test_guard();
        let installed = test_layouts();
        assert!(
            plan_text_with(
                "ghbdtn",
                Some(installed[0]),
                Policy::Manual,
                &installed[1..]
            )
            .is_none()
        );
        let absent = LayoutId(usize::MAX);
        assert!(plan_text_with("ghbdtn", Some(absent), Policy::Manual, &installed).is_none());
    }

    #[test]
    fn converted_runs_keep_exact_target_for_the_next_command() {
        let _guard = layouts::test_guard();
        let installed = test_layouts();
        let runs = vec![InputRun {
            text: "ghbdtn".to_owned(),
            layout: LayoutTag::Native(installed[0].0),
            origin: RunOrigin::Physical,
            kind: RunKind::Text,
        }];
        let first = plan_runs_with(&runs, Policy::Manual, &installed).unwrap();
        assert_eq!(first.text, "привет");
        assert_eq!(first.runs[0].layout, LayoutTag::Native(installed[1].0));
        let second = plan_runs_with(&first.runs, Policy::Manual, &installed).unwrap();
        assert_eq!(second.text, "ghbdtn");
    }
}
