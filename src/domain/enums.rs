use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

use super::error::AppError;

macro_rules! string_enum {
    ($name:ident, $field:literal, { $($variant:ident => $text:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        pub enum $name { $( #[serde(rename = $text)] $variant ),+ }

        impl $name {
            pub fn as_str(self) -> &'static str {
                match self { $( $name::$variant => $text ),+ }
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(self.as_str()) }
        }
        impl FromStr for $name {
            type Err = AppError;
            fn from_str(s: &str) -> Result<Self, AppError> {
                match s.trim().to_ascii_lowercase().as_str() {
                    $( $text => Ok($name::$variant), )+
                    other => Err(AppError::invalid($field, format!(
                        "'{other}' (expected one of: {})", [$($text),+].join(", ")))),
                }
            }
        }
    };
}

// Declared low -> high so the derived Ord sorts by importance.
string_enum!(Priority, "priority", {
    Low => "low", Medium => "medium", High => "high", Critical => "critical",
});
string_enum!(Stage, "stage", {
    Idea => "idea", Planning => "planning", InProgress => "in-progress",
    Review => "review", Done => "done", Paused => "paused",
});
string_enum!(TaskStatus, "status", {
    Todo => "todo", InProgress => "in-progress", Blocked => "blocked", Done => "done",
});
string_enum!(RefKind, "kind", { Commit => "commit", Pr => "pr", Issue => "issue" });

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_case_insensitively_and_round_trips() {
        for p in [
            Priority::Low,
            Priority::Medium,
            Priority::High,
            Priority::Critical,
        ] {
            assert_eq!(p.as_str().to_uppercase().parse::<Priority>().unwrap(), p);
        }
        assert_eq!("in-progress".parse::<Stage>().unwrap(), Stage::InProgress);
    }

    #[test]
    fn rejects_unknown_with_hint() {
        let e = "urgent".parse::<Priority>().unwrap_err();
        assert!(e.to_string().contains("low, medium, high, critical"), "{e}");
    }

    #[test]
    fn priority_orders_by_importance() {
        assert!(Priority::Critical > Priority::High && Priority::High > Priority::Low);
    }
}
