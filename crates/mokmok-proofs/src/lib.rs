//! Kani harnesses over the control core's prompt kernels.
//!
//! The module below is the production file. Kani compiles this crate as one
//! dependency-free unit, so the proof does not depend on the core crate's serde,
//! uuid, or event-store edges.
//!
//! These harnesses prove `room`, `clip_len`, and `clip_to_char_boundary` only, over
//! bounded byte slices. `compose_prompt` itself, the `String`-building validation
//! paths, and the domain/store/actor code are outside any proof; proptest covers the
//! prompt budget and validation panics.

#[path = "../../mokmok-control-core/src/prompt.rs"]
pub mod prompt;

#[cfg(kani)]
mod proofs {
    use super::prompt::{clip_len, clip_to_char_boundary, is_continuation, room};

    #[kani::proof]
    fn room_never_exceeds_the_budget() {
        let max: usize = kani::any();
        let reserved: usize = kani::any();
        assert!(room(max, reserved) <= max);
    }

    #[kani::proof]
    #[kani::unwind(5)]
    fn clip_len_is_bounded_and_lands_on_a_boundary() {
        let bytes: [u8; 4] = kani::any();
        let max: usize = kani::any();
        kani::assume(max <= 4);
        let n = clip_len(&bytes, max);
        assert!(n <= max);
        assert!(n <= 4);
        assert!(n == 4 || n == 0 || !is_continuation(bytes[n]));
    }

    #[kani::proof]
    #[kani::unwind(5)]
    fn clip_to_char_boundary_stays_on_a_boundary() {
        let text = "aé日z";
        let max: usize = kani::any();
        kani::assume(max <= text.len());
        let out = clip_to_char_boundary(text, max);
        assert!(out.len() <= max);
        assert!(out.len() <= text.len());
        assert!(text.is_char_boundary(out.len()));
    }
}
