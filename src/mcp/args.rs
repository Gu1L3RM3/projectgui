//! Strict argument extraction: unknown keys, wrong types and malformed ids are rejected, never ignored.
use std::str::FromStr;

use serde_json::{Map, Value};

use crate::domain::{AppError, Result};

pub struct Args(Map<String, Value>);

fn type_err(key: &'static str, expected: &str) -> AppError {
    AppError::invalid(key, format!("expected {expected}"))
}

impl Args {
    pub fn new(raw: Option<Map<String, Value>>, allowed: &[&str]) -> Result<Self> {
        let map = raw.unwrap_or_default();
        if let Some(extra) = map.keys().find(|k| !allowed.contains(&k.as_str())) {
            return Err(AppError::Invalid {
                field: "arguments",
                reason: format!(
                    "unknown argument '{extra}' (allowed: {})",
                    allowed.join(", ")
                ),
            });
        }
        Ok(Self(map))
    }

    fn get(&self, key: &str) -> Option<&Value> {
        self.0.get(key).filter(|v| !v.is_null())
    }

    pub fn is_null(&self, key: &str) -> bool {
        self.0.get(key).is_some_and(Value::is_null)
    }

    pub fn str(&self, key: &'static str) -> Result<String> {
        self.opt_str(key)?
            .ok_or_else(|| AppError::invalid(key, "is required"))
    }

    pub fn opt_str(&self, key: &'static str) -> Result<Option<String>> {
        match self.get(key) {
            None => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(type_err(key, "a string")),
        }
    }

    pub fn opt_bool(&self, key: &'static str) -> Result<Option<bool>> {
        match self.get(key) {
            None => Ok(None),
            Some(Value::Bool(b)) => Ok(Some(*b)),
            Some(_) => Err(type_err(key, "a boolean")),
        }
    }

    pub fn opt_percent(&self, key: &'static str) -> Result<Option<u8>> {
        match self.get(key) {
            None => Ok(None),
            Some(v) => v
                .as_u64()
                .filter(|n| *n <= 100)
                .map(|n| Some(n as u8))
                .ok_or_else(|| type_err(key, "an integer from 0 to 100")),
        }
    }

    pub fn opt_uint(&self, key: &'static str, max: usize) -> Result<Option<usize>> {
        match self.get(key) {
            None => Ok(None),
            Some(v) => v
                .as_u64()
                .filter(|n| *n as usize <= max)
                .map(|n| Some(n as usize))
                .ok_or_else(|| type_err(key, &format!("an integer from 0 to {max}"))),
        }
    }

    pub fn parse<T: FromStr<Err = AppError>>(&self, key: &'static str) -> Result<T> {
        self.str(key)?.parse()
    }

    pub fn opt_parse<T: FromStr<Err = AppError>>(&self, key: &'static str) -> Result<Option<T>> {
        self.opt_str(key)?.map(|s| s.parse()).transpose()
    }

    pub fn opt_parse_list<T: FromStr<Err = AppError>>(&self, key: &'static str) -> Result<Vec<T>> {
        match self.get(key) {
            None => Ok(vec![]),
            Some(Value::Array(items)) => items
                .iter()
                .map(|v| {
                    v.as_str()
                        .ok_or_else(|| type_err(key, "an array of strings"))?
                        .parse()
                })
                .collect(),
            Some(_) => Err(type_err(key, "an array of strings")),
        }
    }

    pub fn str_list(&self, key: &'static str) -> Result<Vec<String>> {
        match self.get(key) {
            Some(Value::Array(items)) if !items.is_empty() => items
                .iter()
                .map(|v| {
                    v.as_str()
                        .map(String::from)
                        .ok_or_else(|| type_err(key, "an array of strings"))
                })
                .collect(),
            _ => Err(type_err(key, "a non-empty array of strings")),
        }
    }

    pub fn objects(&self, key: &'static str) -> Result<Vec<Map<String, Value>>> {
        match self.get(key) {
            None => Ok(vec![]),
            Some(Value::Array(items)) => items
                .iter()
                .map(|v| {
                    v.as_object()
                        .cloned()
                        .ok_or_else(|| type_err(key, "an array of objects"))
                })
                .collect(),
            Some(_) => Err(type_err(key, "an array of objects")),
        }
    }

    /// A full 26-character ULID. Mutations never accept names, slugs or abbreviations.
    pub fn id(&self, key: &'static str) -> Result<String> {
        let s = self.str(key)?;
        if s.len() == 26 && s.chars().all(|c| c.is_ascii_alphanumeric()) {
            Ok(s.to_ascii_uppercase())
        } else {
            Err(AppError::invalid(
                key,
                "must be the full 26-character id (use resolve_project to look one up)",
            ))
        }
    }

    pub fn id_list(&self, key: &'static str) -> Result<Vec<String>> {
        let list = self.str_list(key)?;
        list.into_iter()
            .map(|s| {
                let one = Args(Map::from_iter([(key.to_string(), Value::String(s))]));
                one.id(key)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn args(v: Value, allowed: &[&str]) -> Result<Args> {
        Args::new(v.as_object().cloned(), allowed)
    }

    #[test]
    fn rejects_unknown_keys_and_wrong_types() {
        assert!(args(json!({"nope": 1}), &["a"]).is_err());
        let a = args(json!({"a": 5, "b": "x"}), &["a", "b"]).unwrap();
        assert!(a.opt_str("a").is_err());
        assert!(a.opt_bool("b").is_err());
        assert!(a.str("missing").is_err());
    }

    #[test]
    fn ids_must_be_full_ulids() {
        let a = args(
            json!({"p": "01arz3ndektsv4rrffq69g5fav", "q": "my-slug"}),
            &["p", "q"],
        )
        .unwrap();
        assert_eq!(a.id("p").unwrap(), "01ARZ3NDEKTSV4RRFFQ69G5FAV");
        assert!(a.id("q").is_err());
    }

    #[test]
    fn percent_and_enums() {
        let a = args(
            json!({"n": 101, "m": 40, "prio": ["high", "low"], "bad": ["x"]}),
            &["n", "m", "prio", "bad"],
        )
        .unwrap();
        assert!(a.opt_percent("n").is_err());
        assert_eq!(a.opt_percent("m").unwrap(), Some(40));
        assert_eq!(
            a.opt_parse_list::<crate::domain::Priority>("prio")
                .unwrap()
                .len(),
            2
        );
        assert!(a.opt_parse_list::<crate::domain::Priority>("bad").is_err());
    }

    #[test]
    fn null_means_absent_but_is_detectable() {
        let a = args(json!({"path": null}), &["path"]).unwrap();
        assert_eq!(a.opt_str("path").unwrap(), None);
        assert!(a.is_null("path"));
    }
}
