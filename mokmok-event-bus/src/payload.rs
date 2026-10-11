#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct Payload(Box<serde_json::value::RawValue>);

#[derive(Debug, thiserror::Error)]
#[error("the value could not be converted to or from JSON")]
pub struct PayloadError {
    #[source]
    inner: serde_json::Error,
}

impl Payload {
    /// Encodes `value`, bytes intact, as the payload a publisher appends.
    ///
    /// # Errors
    ///
    /// Returns a [`PayloadError`] if `value` cannot be encoded as JSON.
    pub fn json<T: serde::Serialize + ?Sized>(value: &T) -> Result<Self, PayloadError> {
        serde_json::value::to_raw_value(value)
            .map(Self)
            .map_err(PayloadError::new)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.get()
    }

    /// Reads the payload back as `T`.
    ///
    /// # Errors
    ///
    /// Returns a [`PayloadError`] if the payload is not JSON that `T` can read.
    pub fn decode<T: serde::de::DeserializeOwned>(&self) -> Result<T, PayloadError> {
        serde_json::from_str(self.0.get()).map_err(PayloadError::new)
    }
}

impl PayloadError {
    const fn new(inner: serde_json::Error) -> Self {
        Self { inner }
    }
}

#[cfg(test)]
mod tests {
    use super::Payload;

    #[test]
    fn json_round_trips_a_value_with_its_bytes_intact() {
        let payload = Payload::json(&serde_json::json!({"turn": 7})).unwrap();

        assert_eq!(payload.as_str(), r#"{"turn":7}"#);
        assert_eq!(payload.decode::<serde_json::Value>().unwrap()["turn"], 7);
    }
}
