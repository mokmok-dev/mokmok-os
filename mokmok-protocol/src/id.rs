#[derive(Debug, serde::Deserialize, serde::Serialize)]
#[serde(transparent)]
pub struct ID<M> {
    value: uuid::Uuid,
    #[serde(skip)]
    _marker: core::marker::PhantomData<M>,
}

impl<M> ID<M> {
    #[must_use]
    pub fn new(ts: uuid::timestamp::Timestamp) -> Self {
        Self {
            value: uuid::Uuid::new_v7(ts),
            _marker: core::marker::PhantomData,
        }
    }
}
