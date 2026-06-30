//! A backend-neutral SQL value.
//!
//! Kept independent of any specific engine so both the rusqlite reference store
//! and the production Turso store consume the same parameter type.

/// A value bindable to a SQL parameter.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// SQL `NULL`.
    Null,
    /// 64-bit signed integer.
    Int(i64),
    /// UTF-8 text.
    Text(String),
    /// Binary blob.
    Blob(Vec<u8>),
}

impl Value {
    /// Convenience constructor for [`Value::Text`].
    #[must_use]
    pub fn text(value: impl Into<String>) -> Self {
        Self::Text(value.into())
    }
}

impl From<&Value> for rusqlite::types::Value {
    fn from(value: &Value) -> Self {
        match value {
            Value::Null => Self::Null,
            Value::Int(i) => Self::Integer(*i),
            Value::Text(s) => Self::Text(s.clone()),
            Value::Blob(b) => Self::Blob(b.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_to_rusqlite_value() {
        assert_eq!(
            rusqlite::types::Value::from(&Value::Int(7)),
            rusqlite::types::Value::Integer(7)
        );
        assert_eq!(
            rusqlite::types::Value::from(&Value::text("x")),
            rusqlite::types::Value::Text("x".to_owned())
        );
    }
}
