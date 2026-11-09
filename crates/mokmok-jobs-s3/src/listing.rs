//! Decoding of the `ListObjectsV2` response.

use mokmok_events::StoreError;
use quick_xml::Reader;
use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::Event;

pub struct Page {
    pub keys: Vec<String>,
    pub truncated: bool,
    pub next_token: Option<String>,
}

#[derive(Clone, Copy)]
enum Capture {
    Key,
    Truncated,
    Token,
}

pub fn parse_page(
    label: &str,
    body: &[u8],
) -> Result<Page, StoreError> {
    let mut reader = Reader::from_reader(body);
    let mut buffer = Vec::new();
    let mut capture = None;
    let mut text = String::new();
    let mut keys = Vec::new();
    let mut truncated = false;
    let mut next_token = None;
    loop {
        let event = reader
            .read_event_into(&mut buffer)
            .map_err(|error| malformed(label, &error))?;
        match event {
            Event::Start(element) => {
                capture = match element.name().as_ref() {
                    b"Key" => Some(Capture::Key),
                    b"IsTruncated" => Some(Capture::Truncated),
                    b"NextContinuationToken" => Some(Capture::Token),
                    _ => None,
                };
                text.clear();
            },
            Event::Text(value) => {
                if capture.is_some() {
                    text.push_str(&value.decode().map_err(|error| malformed(label, &error))?);
                }
            },
            Event::GeneralRef(reference) => {
                if capture.is_some() {
                    append_reference(label, &reference, &mut text)?;
                }
            },
            Event::End(_) => {
                match capture.take() {
                    Some(Capture::Key) => keys.push(std::mem::take(&mut text)),
                    Some(Capture::Truncated) => truncated = text.trim() == "true",
                    Some(Capture::Token) => next_token = Some(std::mem::take(&mut text)),
                    None => {},
                }
                text.clear();
            },
            Event::Eof => break,
            _ => {},
        }
        buffer.clear();
    }
    Ok(Page {
        keys,
        truncated,
        next_token,
    })
}

fn append_reference(
    label: &str,
    reference: &quick_xml::events::BytesRef<'_>,
    text: &mut String,
) -> Result<(), StoreError> {
    if let Some(character) = reference
        .resolve_char_ref()
        .map_err(|error| malformed(label, &error))?
    {
        text.push(character);
        return Ok(());
    }
    let name = reference
        .decode()
        .map_err(|error| malformed(label, &error))?;
    if let Some(value) = resolve_predefined_entity(&name) {
        text.push_str(value);
    } else {
        // An unknown entity stays literal, so a key decodes to something a writer did send.
        text.push('&');
        text.push_str(&name);
        text.push(';');
    }
    Ok(())
}

fn malformed(
    label: &str,
    error: &impl std::fmt::Display,
) -> StoreError {
    StoreError::Io(format!("{label}: malformed listing: {error}"))
}
