use std::fmt;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct Payload(Box<serde_json::value::RawValue>);

#[derive(Debug)]
pub struct PayloadError {
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

impl fmt::Display for PayloadError {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        formatter.write_str("the payload is not JSON the requested type can read")
    }
}

impl std::error::Error for PayloadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.inner)
    }
}

#[cfg(test)]
mod tests {
    use super::Payload;
    use std::error::Error as _;

    struct Unserializable;

    impl serde::Serialize for Unserializable {
        fn serialize<S: serde::Serializer>(
            &self,
            _: S,
        ) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("no"))
        }
    }

    #[test]
    fn json_round_trips_a_value_with_its_bytes_intact() {
        let payload = Payload::json(&serde_json::json!({"turn": 7})).unwrap();

        assert_eq!(payload.as_str(), r#"{"turn":7}"#);
        assert_eq!(payload.decode::<serde_json::Value>().unwrap()["turn"], 7);
    }

    #[test]
    fn decode_reports_the_value_it_could_not_read() {
        let payload = Payload::json(&1).unwrap();

        let error = payload.decode::<String>().unwrap_err();

        assert_eq!(
            error.to_string(),
            "the payload is not JSON the requested type can read",
        );
        assert!(error.source().unwrap().is::<serde_json::Error>());
    }

    #[test]
    fn json_reports_a_value_it_could_not_encode() {
        let error = Payload::json(&Unserializable).unwrap_err();

        assert_eq!(
            error.to_string(),
            "the payload is not JSON the requested type can read",
        );
    }
}
