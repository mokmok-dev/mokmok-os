//! The content identity blobs are named by.

use core::fmt;
use core::str::FromStr;
use sha2::{Digest, Sha256};

/// The SHA-256 of a blob's bytes, the name it is stored under.
///
/// A hash establishes identity, not authority: two blobs with equal bytes share one hash, and a
/// caller that reads by hash is trusted to have earned it. [`crate::Catalog::read`] is where the
/// right to read is checked.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ContentHash([u8; 32]);

impl ContentHash {
    /// Wraps 32 bytes a caller already has.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// The hash of `bytes`, the name [`crate::BlobStore::put`] stores them under.
    #[must_use]
    pub fn of(bytes: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let digest = hasher.finalize();

        let mut hash = [0u8; 32];
        hash.copy_from_slice(digest.as_slice());

        Self(hash)
    }
}

impl fmt::Display for ContentHash {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }

        Ok(())
    }
}

/// Why a string is not the hex form of a [`ContentHash`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum HexError {
    /// The string does not hold one two-digit byte per hash byte.
    #[error("a content hash is 64 hex characters, not {chars}")]
    Length {
        /// How many characters were found.
        chars: usize,
    },
    /// A character is not a hex digit.
    #[error("a content hash is hex, and {found:?} is not")]
    Digit {
        /// The first character that is not hex.
        found: char,
    },
}

impl FromStr for ContentHash {
    type Err = HexError;

    /// Parses 64 hex digits, either case, accepting nothing else.
    ///
    /// Uppercase is accepted on the way in while [`Display`](fmt::Display) writes lowercase, so a
    /// hash that has been copied through a tool remains one value.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let characters: Vec<char> = text.chars().collect();
        if characters.len() != 64 {
            return Err(HexError::Length {
                chars: characters.len(),
            });
        }

        let mut hash = [0u8; 32];
        for (index, [high, low]) in characters.as_chunks::<2>().0.iter().enumerate() {
            hash[index] = (nibble(*high)? << 4) | nibble(*low)?;
        }

        Ok(Self(hash))
    }
}

fn nibble(character: char) -> Result<u8, HexError> {
    character
        .to_digit(16)
        .and_then(|digit| u8::try_from(digit).ok())
        .ok_or(HexError::Digit { found: character })
}
