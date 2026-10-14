//! The `Engine` over the kernel's own driver.

use mokmok_agent_runtime::{Progress, Transcript};
use mokmok_kernel::agent::Driver;
use mokmok_kernel::{Config, Run};
use mokmok_protocol::{Moment, SessionID};

use crate::Error;
use crate::port::Engine;

/// The engine the conversation runs on: the kernel's own driver, one bounded step at a time.
///
/// Every value is the one `mokmokd agent run` builds for the same configuration, so a conversation
/// and a run differ in how often they step and in nothing else.
pub struct Session {
    driver: Driver,
}

impl Session {
    /// Opens the session's world: its log, its tool, and the provider the configuration named.
    ///
    /// # Errors
    ///
    /// Returns the errors of the kernel's driver: a refused grant, program, or credential, and the
    /// log, artifact store, sandbox, or provider that cannot be opened.
    pub fn open(
        config: &Config,
        run: &Run,
        at: Moment,
    ) -> Result<Self, Error> {
        let driver = Driver::open(config, run, at)?;

        Ok(Self { driver })
    }

    /// The session this engine works in.
    #[must_use]
    pub const fn session(&self) -> SessionID {
        self.driver.session()
    }
}

impl Engine for Session {
    fn fold(&self) -> Result<Transcript, Error> {
        self.driver.transcript().map_err(Error::from)
    }

    fn speak(
        &mut self,
        line: &str,
    ) -> Result<(), Error> {
        self.driver.speak(line)?;

        Ok(())
    }

    async fn step(&mut self) -> Result<Progress, Error> {
        self.driver.step().await.map_err(Error::from)
    }

    fn reopen(&mut self) -> Result<(), Error> {
        self.driver.reopen().map_err(Error::from)
    }
}
