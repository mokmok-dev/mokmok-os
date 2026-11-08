//! Composition of the agent prompt, with a hard byte budget.
//!
//! This module imports only from the standard library: `mokmok-proofs` includes this
//! exact file with `#[path]`, and a crate-internal import would break that build.

/// The budget a caller uses when it has no reason to pick another.
pub const DEFAULT_PROMPT_BYTES: usize = 16_384;

/// The largest budget a submission's instruction can need.
///
/// The 64 KiB instruction limit plus 8 KiB for the header, the title line, and the
/// workspace line, so a maximal valid instruction reaches the agent whole.
pub const MAX_PROMPT_BYTES: usize = 65_536 + 8 * 1024;

const UNTITLED: &str = "(untitled)";
const CURRENT_DIRECTORY: &str = "(the agent's current directory)";

const HEADER: &str = "You are an agent running inside mokmok OS.\n\n";
const INSTRUCTION_LABEL: &str = "Instruction:\n";
const FOOTER: &str = "\n\nStop when the instruction is done, and report what you changed.\n";

/// The task text [`compose_prompt`] turns into an agent prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PromptInput<'a> {
    pub title: &'a str,
    pub instruction: &'a str,
    pub workspace: Option<&'a str>,
}

/// Whether `byte` continues a UTF-8 sequence.
#[must_use]
pub const fn is_continuation(byte: u8) -> bool {
    byte & 0b1100_0000 == 0b1000_0000
}

/// The byte length of the longest prefix of `bytes` of at most `max_bytes` bytes
/// that ends on a UTF-8 character boundary.
#[must_use]
pub const fn clip_len(
    bytes: &[u8],
    max_bytes: usize,
) -> usize {
    let len = bytes.len();
    let mut index = if max_bytes < len { max_bytes } else { len };
    while index > 0 {
        if index == len || !is_continuation(bytes[index]) {
            return index;
        }
        index -= 1;
    }
    0
}

/// The room left for a variable part after `reserved` fixed bytes.
#[must_use]
pub const fn room(
    max_bytes: usize,
    reserved: usize,
) -> usize {
    max_bytes.saturating_sub(reserved)
}

/// The longest prefix of `text` of at most `max_bytes` bytes that ends on a character
/// boundary.
#[must_use]
pub fn clip_to_char_boundary(
    text: &str,
    max_bytes: usize,
) -> &str {
    &text[..clip_len(text.as_bytes(), max_bytes)]
}

/// Builds the prompt for one task, of at most `max_bytes` bytes.
///
/// The header, the title line, the workspace line, and the footer are fixed; the
/// instruction takes whatever budget is left. A budget too small for the fixed parts
/// truncates the whole prompt on a character boundary, so the bound holds for every
/// input and every budget.
#[must_use]
pub fn compose_prompt(
    input: &PromptInput<'_>,
    max_bytes: usize,
) -> String {
    let title = if input.title.is_empty() {
        UNTITLED
    } else {
        input.title
    };
    let workspace = input
        .workspace
        .filter(|path| !path.is_empty())
        .unwrap_or(CURRENT_DIRECTORY);
    let head = format!("{HEADER}Task: {title}\nWorkspace: {workspace}\n\n{INSTRUCTION_LABEL}");
    let reserved = head.len().saturating_add(FOOTER.len());
    let room = room(max_bytes, reserved);
    let instruction = clip_to_char_boundary(input.instruction, room);
    let capacity = head
        .len()
        .saturating_add(instruction.len())
        .saturating_add(FOOTER.len());
    let mut prompt = String::with_capacity(capacity);
    prompt.push_str(&head);
    prompt.push_str(instruction);
    prompt.push_str(FOOTER);
    if prompt.len() <= max_bytes {
        return prompt;
    }
    clip_to_char_boundary(&prompt, max_bytes).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>(
        title: &'a str,
        instruction: &'a str,
    ) -> PromptInput<'a> {
        PromptInput {
            title,
            instruction,
            workspace: None,
        }
    }

    fn fixed_bytes(title: &str) -> usize {
        compose_prompt(
            &PromptInput {
                title,
                instruction: "",
                workspace: None,
            },
            MAX_PROMPT_BYTES,
        )
        .len()
    }

    #[test]
    fn an_empty_input_still_names_the_task_the_place_and_the_footer() {
        let prompt = compose_prompt(&input("", ""), DEFAULT_PROMPT_BYTES);
        assert!(prompt.contains("mokmok OS"), "{prompt}");
        assert!(prompt.contains("Task: (untitled)"), "{prompt}");
        assert!(
            prompt.contains("Workspace: (the agent's current directory)"),
            "{prompt}"
        );
        assert!(prompt.contains("Instruction:"), "{prompt}");
        assert!(prompt.contains("report what you changed"), "{prompt}");
        assert!(
            prompt.contains("Stop when the instruction is done"),
            "{prompt}"
        );
        assert!(prompt.len() <= DEFAULT_PROMPT_BYTES);
    }

    #[test]
    fn the_prompt_carries_the_title_the_workspace_and_the_instruction() {
        let prompt = compose_prompt(
            &PromptInput {
                title: "Fix the build",
                instruction: "Make the tests pass.",
                workspace: Some("/work/mokmok"),
            },
            DEFAULT_PROMPT_BYTES,
        );
        assert!(prompt.contains("Task: Fix the build"), "{prompt}");
        assert!(prompt.contains("Workspace: /work/mokmok"), "{prompt}");
        assert!(prompt.contains("Make the tests pass."), "{prompt}");
    }

    #[test]
    fn an_empty_workspace_falls_back_to_the_current_directory_line() {
        let prompt = compose_prompt(
            &PromptInput {
                title: "T",
                instruction: "I",
                workspace: Some(""),
            },
            DEFAULT_PROMPT_BYTES,
        );
        assert!(
            prompt.contains("Workspace: (the agent's current directory)"),
            "{prompt}"
        );
    }

    #[test]
    fn an_exact_fit_keeps_the_whole_instruction() {
        let instruction = "abcdefghij";
        let budget = fixed_bytes("T").saturating_add(instruction.len());
        let prompt = compose_prompt(
            &PromptInput {
                title: "T",
                instruction,
                workspace: None,
            },
            budget,
        );
        assert_eq!(prompt.len(), budget);
        assert!(prompt.contains(instruction), "{prompt}");
        assert!(prompt.contains("report what you changed"), "{prompt}");
    }

    #[test]
    fn one_byte_over_the_exact_fit_drops_instruction_text_not_the_footer() {
        let instruction = "abcdefghij";
        let fixed = fixed_bytes("T");
        let budget = fixed.saturating_add(instruction.len()).saturating_sub(1);
        let prompt = compose_prompt(
            &PromptInput {
                title: "T",
                instruction,
                workspace: None,
            },
            budget,
        );
        assert_eq!(prompt.len(), budget);
        assert!(prompt.contains("abcdefghi"), "{prompt}");
        assert!(!prompt.contains(instruction), "{prompt}");
        assert!(prompt.contains("report what you changed"), "{prompt}");
    }

    #[test]
    fn a_multi_byte_character_is_never_split() {
        let instruction = "aé";
        let fixed = fixed_bytes("T");
        let prompt = compose_prompt(
            &PromptInput {
                title: "T",
                instruction,
                workspace: None,
            },
            fixed.saturating_add(1),
        );
        assert_eq!(prompt.len(), fixed.saturating_add(1));
        assert!(!prompt.contains("a\u{e9}"), "{prompt}");
        assert!(prompt.contains("Instruction:\na\n"), "{prompt}");

        let prompt = compose_prompt(
            &PromptInput {
                title: "T",
                instruction,
                workspace: None,
            },
            fixed.saturating_add(2),
        );
        assert_eq!(prompt.len(), fixed.saturating_add(1));
    }

    #[test]
    fn any_budget_yields_a_bounded_prompt() {
        let input = PromptInput {
            title: "Fix the build",
            instruction: "Make the tests pass, then stop.",
            workspace: Some("/work/mokmok"),
        };
        let head =
            format!("{HEADER}Task: Fix the build\nWorkspace: /work/mokmok\n\n{INSTRUCTION_LABEL}");
        let reserved = head.len().saturating_add(FOOTER.len());
        let without_instruction = format!("{head}{FOOTER}");
        let full = compose_prompt(&input, usize::MAX);
        for budget in 0..=full.len() + 4 {
            let prompt = compose_prompt(&input, budget);
            assert!(prompt.len() <= budget, "budget {budget}");
            if budget >= reserved {
                assert!(prompt.starts_with(&head), "budget {budget}: {prompt:?}");
                assert!(prompt.ends_with(FOOTER), "budget {budget}: {prompt:?}");
            } else {
                assert!(
                    without_instruction.starts_with(&prompt),
                    "budget {budget}: {prompt:?}"
                );
            }
        }
    }

    #[test]
    fn a_tiny_budget_truncates_the_whole_prompt() {
        let input = input("Fix the build", "Make the tests pass.");
        assert_eq!(compose_prompt(&input, 0), "");
        let prompt = compose_prompt(&input, 16);
        assert_eq!(prompt.len(), 16);
        assert!(prompt.starts_with("You are an agent"));
    }

    #[test]
    fn composition_is_deterministic() {
        let input = input("Fix the build", "Make the tests pass.");
        assert_eq!(
            compose_prompt(&input, DEFAULT_PROMPT_BYTES),
            compose_prompt(&input, DEFAULT_PROMPT_BYTES)
        );
    }

    #[test]
    fn clipping_stops_before_a_split_character() {
        let text = "aé";
        assert_eq!(clip_to_char_boundary(text, 3), text);
        assert_eq!(clip_to_char_boundary(text, 2), "a");
        assert_eq!(clip_to_char_boundary(text, 1), "a");
        assert_eq!(clip_to_char_boundary(text, 0), "");
        assert_eq!(clip_to_char_boundary("", 7), "");
    }

    #[test]
    fn only_trailing_bytes_of_a_character_count_as_continuation() {
        assert!(is_continuation(0b1000_0000));
        assert!(is_continuation(0b1011_1111));
        assert!(!is_continuation(0b1100_0011));
        assert!(!is_continuation(b'a'));
    }

    #[test]
    fn clip_len_stops_at_the_budget_on_a_boundary() {
        assert_eq!(clip_len(b"", 0), 0);
        assert_eq!(clip_len(b"", 4), 0);
        assert_eq!(clip_len(b"", usize::MAX), 0);

        assert_eq!(clip_len(b"abc", 3), 3);
        assert_eq!(clip_len(b"abc", 2), 2);
        assert_eq!(clip_len(b"abc", 1), 1);
        assert_eq!(clip_len(b"abc", 0), 0);
        assert_eq!(clip_len(b"abc", usize::MAX), 3);
    }

    #[test]
    fn the_budget_covers_the_maximal_instruction_and_the_fixed_lines() {
        // 64 KiB instruction + 8 KiB for the header, title, and workspace lines.
        assert_eq!(MAX_PROMPT_BYTES, 73_728);
    }

    #[test]
    fn a_maximal_workspace_still_carries_the_instruction_and_the_footer() {
        let workspace = format!("/{}", "w".repeat(4095));
        let title = "t".repeat(200);
        let instruction = "i".repeat(65_536);
        let prompt = compose_prompt(
            &PromptInput {
                title: &title,
                instruction: &instruction,
                workspace: Some(&workspace),
            },
            MAX_PROMPT_BYTES,
        );
        assert!(prompt.contains("Instruction:\n"), "{}", &prompt[..256]);
        assert!(prompt.contains(&instruction), "the instruction is complete");
        assert!(prompt.ends_with(FOOTER), "the footer is present");
        assert!(prompt.len() <= MAX_PROMPT_BYTES);
    }

    #[test]
    fn clip_len_at_the_exact_budget_keeps_everything() {
        assert_eq!(clip_len(b"abcd", 4), 4);
        assert_eq!(clip_len("aé".as_bytes(), 3), 3);
    }

    #[test]
    fn clip_len_at_zero_never_reads_the_first_byte() {
        assert_eq!(clip_len(b"abc", 0), 0);
        assert_eq!(clip_len(&[0x80], 0), 0);
        assert_eq!(clip_len(&[0x80, b'a'], 0), 0);
    }

    #[test]
    fn clip_len_walks_a_four_byte_character_back_to_its_start() {
        let emoji = "😀";
        assert_eq!(emoji.len(), 4);
        assert_eq!(clip_len(emoji.as_bytes(), 4), 4);
        assert_eq!(clip_len(emoji.as_bytes(), 3), 0);
        assert_eq!(clip_len(emoji.as_bytes(), 2), 0);
        assert_eq!(clip_len(emoji.as_bytes(), 1), 0);
    }

    #[test]
    fn clip_len_backs_off_a_continuation_byte() {
        let text = "aé";
        assert!(is_continuation(text.as_bytes()[2]));
        assert_eq!(clip_len(text.as_bytes(), 2), 1);
        assert_eq!(clip_len(text.as_bytes(), 0), 0);
    }

    #[test]
    fn the_room_never_exceeds_the_budget() {
        assert_eq!(room(0, 0), 0);
        assert_eq!(room(0, 1), 0);
        assert_eq!(room(10, 4), 6);
        assert_eq!(room(10, 10), 0);
        assert_eq!(room(10, 11), 0);
        assert_eq!(room(usize::MAX, 1), usize::MAX - 1);
    }
}
