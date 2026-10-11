use std::num::NonZeroUsize;
use std::time::Duration;

#[derive(Clone, Copy, Debug)]
pub struct Retention {
    pub events: NonZeroUsize,
    pub bytes: NonZeroUsize,
    pub idle: Duration,
}
