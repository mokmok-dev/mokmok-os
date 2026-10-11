mod marker;

pub use marker::*;

mod private {
    pub trait Sealed {}
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
#[serde(transparent)]
pub struct ID<M: private::Sealed> {
    value: uuid::Uuid,
    #[serde(skip)]
    _marker: core::marker::PhantomData<M>,
}

impl<M: private::Sealed> ID<M> {
    #[must_use]
    pub fn new(ts: uuid::timestamp::Timestamp) -> Self {
        Self {
            value: uuid::Uuid::new_v7(ts),
            _marker: core::marker::PhantomData,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ID;
    use core::marker::PhantomData;
    use proptest::prelude::*;
    use uuid::{Uuid, timestamp::Timestamp};

    #[derive(Debug, serde::Deserialize, serde::Serialize)]
    struct Marker;

    impl super::private::Sealed for Marker {}

    /// The highest whole second that keeps a version 7 UUID's millisecond
    /// timestamp inside its 48-bit field for every sub-second value.
    const MAX_SECONDS: u64 = (1 << 48) / 1_000 - 1;

    /// Builds a version 7 timestamp with no clock sequence, so that
    /// [`Uuid::new_v7`] fills the remaining bits with randomness.
    const fn timestamp(
        seconds: u64,
        subsec_nanos: u32,
    ) -> Timestamp {
        Timestamp::from_unix_time(seconds, subsec_nanos, 0, 0)
    }

    proptest! {
        #[test]
        fn new_builds_a_version_7_uuid(
            seconds in 0u64..=u64::MAX,
            subsec_nanos in 0u32..1_000_000_000,
        ) {
            let id = ID::<Marker>::new(timestamp(seconds, subsec_nanos));

            prop_assert_eq!(id.value.get_version_num(), 7);
        }

        #[test]
        fn new_round_trips_the_timestamp(
            seconds in 0u64..=MAX_SECONDS,
            subsec_nanos in 0u32..1_000_000_000,
        ) {
            let id = ID::<Marker>::new(timestamp(seconds, subsec_nanos));
            let (got_seconds, got_nanos) = id.value.get_timestamp().unwrap().to_unix();

            prop_assert_eq!(got_seconds, seconds);
            prop_assert_eq!(got_nanos, subsec_nanos / 1_000_000 * 1_000_000);
        }

        #[test]
        fn new_orders_ids_by_their_timestamp(
            seconds_a in 0u64..=MAX_SECONDS,
            subsec_nanos_a in 0u32..1_000_000_000,
            seconds_b in 0u64..=MAX_SECONDS,
            subsec_nanos_b in 0u32..1_000_000_000,
        ) {
            let millis_a = seconds_a * 1_000 + u64::from(subsec_nanos_a) / 1_000_000;
            let millis_b = seconds_b * 1_000 + u64::from(subsec_nanos_b) / 1_000_000;
            prop_assume!(millis_a != millis_b);

            let id_a = ID::<Marker>::new(timestamp(seconds_a, subsec_nanos_a));
            let id_b = ID::<Marker>::new(timestamp(seconds_b, subsec_nanos_b));

            prop_assert_eq!(
                id_a.value.as_bytes() < id_b.value.as_bytes(),
                millis_a < millis_b,
            );
        }

        #[test]
        fn serde_is_transparent_over_the_inner_uuid(bytes in any::<[u8; 16]>()) {
            let id = ID::<Marker> {
                value: Uuid::from_bytes(bytes),
                _marker: PhantomData,
            };

            let encoded = serde_json::to_string(&id).unwrap();
            let decoded: ID<Marker> = serde_json::from_str(&encoded).unwrap();

            prop_assert_eq!(decoded.value, id.value);
            prop_assert_eq!(encoded, serde_json::to_string(&id.value).unwrap());
        }
    }
}
