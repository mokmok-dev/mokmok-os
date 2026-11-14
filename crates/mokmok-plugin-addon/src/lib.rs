//! The napi addon: the TypeScript contract, generated from the Rust types.
//!
//! Nothing loads this crate's `.node` at run time. Its only job is to make
//! `napi build --dts` emit `packages/plugin-sdk/types.gen.d.ts`, which the SDK re-exports
//! and the plugin host is typechecked against. The runtime wire is
//! [`mokmok_plugin_protocol`], and the daemon reads it in Rust.
//!
//! `napi_derive`'s macros expand to unsafe code, so this crate is the one place in the
//! workspace that opts out of the `unsafe_code = "deny"` lint. Every other member keeps
//! it. This file holds only safe functions; the unsafe is inside the macro expansions.
#![allow(unsafe_code)]

use mokmok_plugin_protocol::{PluginEvent, PluginOutcome, PluginResult, PluginTask};
use napi_derive::napi;

/// The contract types, in one object.
///
/// `napi build --dts` declares every type a function mentions, so one function that
/// returns all of them generates the whole contract. Never called at run time.
#[napi(object)]
#[derive(Clone, Debug)]
pub struct PluginSchema {
    pub task: PluginTask,
    pub event: PluginEvent,
    pub result: PluginResult,
    pub outcome: PluginOutcome,
}

// A plain comment rather than a doc comment: a `///` here would be copied into the
// author-facing `index.d.ts`.
#[napi]
#[must_use]
pub const fn schema() -> PluginSchema {
    PluginSchema {
        task: PluginTask {
            task_id: String::new(),
            title: String::new(),
            instruction: String::new(),
            workspace: None,
        },
        event: PluginEvent::Message {
            text: String::new(),
        },
        result: PluginResult::Cancelled,
        outcome: PluginOutcome::Cancelled,
    }
}

#[cfg(test)]
mod tests {
    //! The addon's only load-bearing property is that the types it emits are the ones
    //! the wire uses. The drift check in `flake.nix` compares the generated file with the
    //! committed one; this test pins the one thing that check cannot see, which is that
    //! the declared shapes and the serialized shapes agree.

    use super::*;
    use serde::Serialize;

    fn json<T: Serialize>(value: &T) -> serde_json::Value {
        serde_json::to_value(value).expect("the value encodes")
    }

    #[test]
    fn the_schema_serializes_to_the_wire_shape() {
        let schema = schema();
        assert_eq!(
            json(&schema.task),
            serde_json::json!({
                "task_id": "",
                "title": "",
                "instruction": ""
            })
        );
        assert_eq!(
            json(&schema.event),
            serde_json::json!({"kind": "message", "text": ""})
        );
        assert_eq!(
            json(&schema.result),
            serde_json::json!({"kind": "cancelled"})
        );
        assert_eq!(
            json(&schema.outcome),
            serde_json::json!({"kind": "cancelled"})
        );
    }

    #[test]
    fn the_author_result_is_narrower_than_the_wire_outcome() {
        // A plugin reports a failure by throwing, so the type it returns has no failure
        // variant. This is the invariant that keeps one way to say each thing.
        let result = json(&PluginResult::Completed {
            summary: "done".to_owned(),
        });
        assert_eq!(result["kind"], "completed");
        let outcome = json(&PluginOutcome::Failed {
            reason: "boom".to_owned(),
        });
        assert_eq!(outcome["kind"], "failed");
    }
}
