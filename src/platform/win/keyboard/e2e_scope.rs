use std::sync::OnceLock;

use windows::Win32::{Foundation::HWND, UI::WindowsAndMessaging::GetWindowThreadProcessId};

const REQUIRED_ENVIRONMENT: [(&str, &str); 6] = [
    ("GITHUB_ACTIONS", "true"),
    ("RUNNER_ENVIRONMENT", "github-hosted"),
    ("RUNNER_OS", "Windows"),
    ("RUST_SWITCHER_HOSTED_E2E", "1"),
    ("RUST_SWITCHER_E2E_ALLOW_INJECTED", "1"),
    ("RUST_SWITCHER_E2E_PLAYGROUND", "1"),
];

fn fixture_pid(read: impl Fn(&str) -> Option<String>) -> Option<u32> {
    for (name, expected) in REQUIRED_ENVIRONMENT {
        if read(name).as_deref() != Some(expected) {
            return None;
        }
    }
    read("RUST_SWITCHER_E2E_TARGET_PID")?
        .parse::<u32>()
        .ok()
        .filter(|pid| *pid != 0)
}

/// Opt in to one unowned fixture process on the isolated hosted runner.
/// This module is absent from release builds. Keeping the fixture unowned avoids
/// implicitly attaching its input queue to the application's keyboard state.
pub(super) fn allows_foreground(window: HWND) -> bool {
    static TARGET_PID: OnceLock<Option<u32>> = OnceLock::new();
    let Some(expected) = *TARGET_PID.get_or_init(|| fixture_pid(|name| std::env::var(name).ok()))
    else {
        return false;
    };
    if window.0.is_null() {
        return false;
    }
    let mut actual = 0;
    let thread = unsafe { GetWindowThreadProcessId(window, Some(&mut actual)) };
    thread != 0 && actual == expected
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn hosted_environment() -> HashMap<&'static str, &'static str> {
        REQUIRED_ENVIRONMENT
            .into_iter()
            .chain([("RUST_SWITCHER_E2E_TARGET_PID", "1234")])
            .collect()
    }

    fn read_pid(environment: &HashMap<&str, &str>) -> Option<u32> {
        fixture_pid(|name| environment.get(name).map(|value| (*value).to_string()))
    }

    #[test]
    fn hosted_fixture_scope_requires_every_explicit_opt_in() {
        assert_eq!(read_pid(&hosted_environment()), Some(1234));
        for (name, _) in REQUIRED_ENVIRONMENT {
            let mut missing = hosted_environment();
            missing.remove(name);
            assert_eq!(read_pid(&missing), None, "missing {name}");
            let mut wrong = hosted_environment();
            wrong.insert(name, "disabled");
            assert_eq!(read_pid(&wrong), None, "wrong {name}");
        }
    }

    #[test]
    fn hosted_fixture_scope_rejects_missing_zero_or_invalid_pid() {
        let mut environment = hosted_environment();
        environment.remove("RUST_SWITCHER_E2E_TARGET_PID");
        assert_eq!(read_pid(&environment), None);
        for invalid in ["", "0", "-1", "all", "1234,5678", "4294967296"] {
            environment.insert("RUST_SWITCHER_E2E_TARGET_PID", invalid);
            assert_eq!(read_pid(&environment), None, "invalid PID {invalid:?}");
        }
    }
}
