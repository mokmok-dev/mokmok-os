use mokmok_protocol::PrincipalID;

use crate::name::Name;

/// What kind of actor a principal is.
///
/// The kind is kept so a durable record of a decision can say what it decided about. It never
/// enters a rule or a grant: authority is issued to a [`PrincipalID`], not to a class of actor.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrincipalKind {
    Human,
    Agent,
    Service,
}

/// One actor an authority issues authority to.
///
/// [`Resource::Tool`](crate::Resource::Tool) is a name and a [`Principal`] is a name, id, and kind,
/// so the authority that issues a root grant is the one place an id is bound to a readable name.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Principal {
    id: PrincipalID,
    kind: PrincipalKind,
    name: Name,
}

impl Principal {
    #[must_use]
    pub const fn new(
        id: PrincipalID,
        kind: PrincipalKind,
        name: Name,
    ) -> Self {
        Self { id, kind, name }
    }

    #[must_use]
    pub const fn id(&self) -> PrincipalID {
        self.id
    }

    #[must_use]
    pub const fn kind(&self) -> PrincipalKind {
        self.kind
    }

    #[must_use]
    pub const fn name(&self) -> &Name {
        &self.name
    }
}
