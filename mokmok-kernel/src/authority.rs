//! The one place authority is decided, and the adapter that hands the sandbox what it covers.
//!
//! The operator states the reach: `--allow-tool` names the programs the agent principal may
//! invoke, `--allow-path` the directories the workload may read and execute in, and
//! `--allow-write` the ones it may also write. Each statement becomes a root grant, so the agent
//! holds exactly what it was given and nothing else, and an operation the operator did not name is
//! denied by the policy before any grant is consulted.
//!
//! The kernel is the adapter, not the decider. It asks the capability crate for a decision and
//! translates what was granted into the sandbox's own narrow types, so the sandbox enforces
//! mechanically what the decision allowed.

use std::path::{Path, PathBuf};

use mokmok_capability::{
    Action, ActionSet, Authority as Decisions, Context, Decision, Denial, DenialReason, Name,
    Permission, Policy, Principal, PrincipalKind, Request, Resource, Rule,
};
use mokmok_protocol::{Moment, PrincipalID};
use mokmok_sandbox::Access;
use uuid::Uuid;

use crate::config::Config;
use crate::error::Error;

/// The one principal this round runs as.
///
/// Fixed rather than minted at startup, because a restart has to reach the decisions the log
/// recorded: a principal whose identity changed between runs would decide differently over the
/// same history.
pub const AGENT: PrincipalID =
    PrincipalID::from_uuid(Uuid::from_u128(0x0199_0000_0000_7000_8000_0000_0000_0001));

/// The name the agent principal is bound to in the policy.
const AGENT_NAME: &str = "mokmok-agent";

/// The path the sandbox opens as the workload's standard input.
///
/// The confiner opens it after it has applied the ruleset, so the read has to be inside that
/// ruleset. This is the sandbox's own need rather than the operator's grant, and leaving it out
/// confines every workload away from its own standard input.
pub const DEV_NULL: &str = "/dev/null";

/// Authority as this round's configuration states it.
pub struct Authority {
    decisions: Decisions,
    write_roots: Vec<PathBuf>,
}

impl Authority {
    /// Builds the policy and the root grants one configuration states.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidName`] when a granted path or program is not a name, and
    /// [`Error::NotAName`] when one is not a UTF-8 path.
    pub fn new(
        config: &Config,
        at: Moment,
    ) -> Result<Self, Error> {
        let mut policy = Policy::new();
        let mut roots = Vec::new();
        let mut write_roots = Vec::new();

        for path in &config.allow_write {
            write_roots.push(path.clone());
            let resource = Resource::Path(path_of(path)?);
            policy = policy
                .with(rule(Action::Read, resource.clone()))
                .with(rule(Action::Write, resource.clone()))
                .with(rule(Action::Execute, resource.clone()));
            roots.push((
                resource,
                ActionSet::READ
                    .union(ActionSet::WRITE)
                    .union(ActionSet::EXECUTE),
            ));
        }

        for path in &config.allow_path {
            let resource = Resource::Path(path_of(path)?);
            policy = policy
                .with(rule(Action::Read, resource.clone()))
                .with(rule(Action::Execute, resource.clone()));
            roots.push((resource, ActionSet::READ.union(ActionSet::EXECUTE)));
        }

        for tool in &config.allow_tool {
            let resource = Resource::Tool(name_of(&tool.to_string_lossy())?);
            policy = policy.with(rule(Action::Invoke, resource.clone()));
            roots.push((resource, ActionSet::INVOKE));
        }

        let stdio = Resource::Path(DEV_NULL.to_owned());
        policy = policy
            .with(rule(Action::Read, stdio.clone()))
            .with(rule(Action::Write, stdio.clone()));
        roots.push((stdio, ActionSet::READ.union(ActionSet::WRITE)));

        let mut decisions = Decisions::new(policy, at);
        for (resource, actions) in roots {
            // The grant is issued for its effect on the authority's own record; a later attenuation
            // would be the thing that holds the value.
            let _ = decisions.root(
                Principal::new(AGENT, PrincipalKind::Agent, name_of(AGENT_NAME)?),
                Permission::new(resource, actions),
                at,
            );
        }

        Ok(Self {
            decisions,
            write_roots,
        })
    }

    /// Whether the agent principal may write at one path, from the deepest write grant that covers it.
    ///
    /// The path is compared to the `--allow-write` roots the policy was built from, so a path
    /// outside every grant is denied before the policy is asked. The grant that covers it is then
    /// asked for `Action::Write`, and the answer is the one the policy gives.
    ///
    /// # Errors
    ///
    /// Returns a [`Denial`] when no write root covers the path, or when the policy denies write
    /// on the root that does.
    pub fn may_write_at(
        &self,
        path: &Path,
        at: Moment,
    ) -> Result<Access, Denial> {
        let root = self
            .write_roots
            .iter()
            .filter(|root| path.starts_with(root))
            .max_by_key(|root| root.as_os_str().len())
            .ok_or_else(|| Denial::new(DenialReason::NotGranted))?;
        let text = root
            .to_str()
            .ok_or_else(|| Denial::new(DenialReason::NotGranted))?
            .to_owned();
        let resource = Resource::Path(text);
        let request = Self::request(Action::Write, &resource, Self::agent());

        match self.decisions.decide(&request, at) {
            Decision::Allow(grant) if grant.permission().actions().covers(ActionSet::WRITE) => {
                Ok(Access::WRITE)
            },
            Decision::Allow(_) | Decision::Approve(_) => Err(Denial::new(DenialReason::NotGranted)),
            Decision::Deny(denial) => Err(denial),
        }
    }

    /// The sandbox authority for one path, from what the policy and a live grant allow there.
    ///
    /// The set is the truth, not a refusal: a path the operator opened for reading comes back with
    /// read and execute, and write simply is not in it. Only a path nothing allows is an error, and
    /// it carries the first real reason the authority reached rather than a generic one.
    ///
    /// # Errors
    ///
    /// Returns the [`Denial`] for the first action nothing allowed, when no action was allowed.
    pub fn access_at(
        &self,
        path: &Path,
        at: Moment,
    ) -> Result<Access, Denial> {
        let resource = Resource::Path(path.to_string_lossy().into_owned());
        let mut access: Option<Access> = None;
        let mut refusal = None;

        for (action, bit) in [
            (Action::Read, Access::READ),
            (Action::Write, Access::WRITE),
            (Action::Execute, Access::EXECUTE),
        ] {
            match self
                .decisions
                .decide(&Self::request(action, &resource, Self::agent()), at)
            {
                Decision::Allow(grant) => {
                    if grant.permission().actions().covers(ActionSet::of(action)) {
                        access = Some(access.map_or(bit, |known| known.union(bit)));
                    }
                },
                Decision::Deny(denial) => refusal = refusal.or(Some(denial)),
                Decision::Approve(_) => {
                    refusal = refusal.or(Some(Denial::new(DenialReason::NotGranted)));
                },
            }
        }

        access.ok_or_else(|| refusal.unwrap_or_else(|| Denial::new(DenialReason::NotGranted)))
    }

    /// Whether the agent principal may invoke one tool at `at`.
    ///
    /// # Errors
    ///
    /// Returns the [`Denial`] the authority reached, which ends the execution before the sandbox
    /// creates anything.
    pub fn may_invoke(
        &self,
        tool: &Name,
        at: Moment,
    ) -> Result<(), Denial> {
        let request = Self::request(Action::Invoke, &Resource::Tool(tool.clone()), Self::agent());

        match self.decisions.decide(&request, at) {
            Decision::Allow(_) => Ok(()),
            Decision::Deny(denial) => Err(denial),
            Decision::Approve(_) => Err(Denial::new(DenialReason::NotGranted)),
        }
    }

    /// The principal every request from this authority is made as.
    const fn agent() -> PrincipalID {
        AGENT
    }

    fn request(
        action: Action,
        resource: &Resource,
        principal: PrincipalID,
    ) -> Request {
        Request {
            principal,
            action,
            resource: resource.clone(),
            context: Context::none(),
        }
    }
}

fn rule(
    action: Action,
    resource: Resource,
) -> Rule {
    Rule::allow()
        .for_principal(AGENT)
        .for_action(action)
        .for_resource(resource)
}

fn name_of(value: &str) -> Result<Name, Error> {
    Name::new(value).map_err(Error::InvalidName)
}

fn path_of(path: &Path) -> Result<String, Error> {
    path.to_str()
        .map(str::to_owned)
        .ok_or(Error::NotAName("a granted path"))
}
