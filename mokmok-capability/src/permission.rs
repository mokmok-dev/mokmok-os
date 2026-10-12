use core::fmt;

use crate::resource::Resource;

/// One operation kind this crate names.
#[derive(
    Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Read,
    Write,
    Execute,
    Connect,
    Invoke,
}

impl fmt::Display for Action {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        formatter.write_str(match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Execute => "execute",
            Self::Connect => "connect",
            Self::Invoke => "invoke",
        })
    }
}

/// The set of [`Action`]s a permission or a rule names.
///
/// A set rather than an enum because a permission composes component-wise: a read-write grant
/// covers a read request and a write request, and neither alone.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ActionSet(u8);

impl ActionSet {
    pub const READ: Self = Self(0b0_0001);
    pub const WRITE: Self = Self(0b0_0010);
    pub const EXECUTE: Self = Self(0b0_0100);
    pub const CONNECT: Self = Self(0b0_1000);
    pub const INVOKE: Self = Self(0b1_0000);
    /// Every action this version names.
    pub const ALL: Self = Self(0b1_1111);

    #[must_use]
    pub const fn of(action: Action) -> Self {
        Self(match action {
            Action::Read => Self::READ.0,
            Action::Write => Self::WRITE.0,
            Action::Execute => Self::EXECUTE.0,
            Action::Connect => Self::CONNECT.0,
            Action::Invoke => Self::INVOKE.0,
        })
    }

    /// Whether every action in `needed` is in this set.
    #[must_use]
    pub const fn covers(
        self,
        needed: Self,
    ) -> bool {
        self.0 & needed.0 == needed.0
    }

    #[must_use]
    pub const fn union(
        self,
        other: Self,
    ) -> Self {
        Self(self.0 | other.0)
    }
}

// The wire form is the bit set. A set that names no action, or an action this version does not
// know, would let a parsed permission widen the grant it is attenuated against, so both are
// refused where the permission is parsed.
impl serde::Serialize for ActionSet {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_u8(self.0)
    }
}

impl<'de> serde::Deserialize<'de> for ActionSet {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let bits = u8::deserialize(deserializer)?;

        if bits == 0 || bits & !Self::ALL.0 != 0 {
            return Err(serde::de::Error::custom(
                "an action set is a nonempty subset of the five actions",
            ));
        }

        Ok(Self(bits))
    }
}

/// The actions one principal may take on one resource.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Permission {
    resource: Resource,
    actions: ActionSet,
}

impl Permission {
    #[must_use]
    pub const fn new(
        resource: Resource,
        actions: ActionSet,
    ) -> Self {
        Self { resource, actions }
    }

    #[must_use]
    pub const fn resource(&self) -> &Resource {
        &self.resource
    }

    #[must_use]
    pub const fn actions(&self) -> ActionSet {
        self.actions
    }

    /// Whether everything this permission admits, `other` admits too.
    ///
    /// The resource must be identical. Narrowing a path or a host to something beneath it would
    /// compare two names the way the layer that enforces the decision resolves them, and this
    /// crate reads no filesystem and no resolver, so a name it cannot resolve would be a
    /// comparison that silently widens.
    #[must_use]
    pub fn covers(
        &self,
        other: &Self,
    ) -> bool {
        self.resource == other.resource && self.actions.covers(other.actions)
    }
}

#[cfg(test)]
mod tests {
    use super::{Action, ActionSet, Permission};
    use crate::resource::Resource;
    use alloc::string::String;

    #[test]
    fn a_set_covers_exactly_its_own_actions() {
        let read_write = ActionSet::READ.union(ActionSet::WRITE);

        assert!(read_write.covers(ActionSet::READ));
        assert!(read_write.covers(read_write));
        assert!(!read_write.covers(ActionSet::EXECUTE));
        assert_eq!(ActionSet::of(Action::Connect), ActionSet::CONNECT);
        assert_eq!(ActionSet::of(Action::Invoke), ActionSet::INVOKE);
    }

    #[test]
    fn a_permission_covers_only_its_own_resource() {
        let path = Resource::Path(String::from("/data"));
        let other = Resource::Path(String::from("/other"));

        assert!(
            Permission::new(path.clone(), ActionSet::READ)
                .covers(&Permission::new(path, ActionSet::READ))
        );
        assert!(
            !Permission::new(other, ActionSet::READ).covers(&Permission::new(
                Resource::Path(String::from("/data")),
                ActionSet::READ
            ))
        );
    }
}
