//! Panic helpers for unit tests (`ok` / `err` / `some` and extensions).

use std::fmt::Display;

#[track_caller]
pub(crate) fn ok<T, E>(result: Result<T, E>, context: impl Display) -> T
where
    E: Display,
{
    result.unwrap_or_else(|err| panic!("{context}: {err}"))
}

#[track_caller]
pub(crate) fn err<T, E>(result: Result<T, E>, context: impl Display) -> E {
    match result {
        Ok(_) => panic!("{context}"),
        Err(err) => err,
    }
}

#[track_caller]
pub(crate) fn some<T>(value: Option<T>, context: impl Display) -> T {
    value.unwrap_or_else(|| panic!("{context}"))
}

/// Extension helpers for `Result` in tests (same semantics as [`ok`] /
/// [`err`]).
pub(crate) trait TestResultExt<T, E> {
    fn or_panic(self, context: impl Display) -> T
    where
        E: Display;
    fn err_or_panic(self, context: impl Display) -> E;
}

impl<T, E> TestResultExt<T, E> for Result<T, E> {
    #[track_caller]
    fn or_panic(self, context: impl Display) -> T
    where
        E: Display,
    {
        ok(self, context)
    }

    #[track_caller]
    fn err_or_panic(self, context: impl Display) -> E {
        err(self, context)
    }
}

/// Extension helper for `Option` in tests (same semantics as [`some`]).
pub(crate) trait TestOptionExt<T> {
    fn or_panic(self, context: impl Display) -> T;
}

impl<T> TestOptionExt<T> for Option<T> {
    #[track_caller]
    fn or_panic(self, context: impl Display) -> T {
        some(self, context)
    }
}
