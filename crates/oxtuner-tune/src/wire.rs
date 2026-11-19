//! Page images: bytes on the wire, values in the tune.
//!
//! A read gives a page as `pageSize` bytes; the tune holds one value per
//! constant, in user-facing storage. This module is the one place that converts
//! between them, using the layout's own addresses, types and byte order.
//!
//! ```text
//! page bytes ──decode_page──▶ Tune (ecu.ram)
//! page bytes ◀──encode_page── Tune (working, for a write)
//! ```
//!
//! # What it refuses
//!
//! A variable whose address or shape the definition did not resolve is skipped
//! and counted in [`DecodeReport::skipped`]. It is never placed at offset zero
//! and never invented — a wrong value in the ECU image is worse than a missing
//! one (ADR-0010).
//!
//! A bit field is refused the same way when it reaches past its own storage
//! word (`[40:47]` on a U16), when its word is wider than 32 bits, or when the
//! word is not an integer. Signed words are read and written as their
//! two's-complement pattern. Text is Latin-1 on the wire, like the `.msq` layer,
//! so a text constant with a character above U+00FF is refused on write with
//! [`TuneError::TextNotLatin1`]; the page image is never half-encoded. Decode
//! skips a field it refuses.

use oxtuner_ini::{DataType, Endianness};

use crate::layout::bit_field_can_round_trip;
use crate::{EntryShape, IniPage, Tune, TuneError, TuneLayout, VariableSpec};

/// Largest page image the codec will build.
///
/// Real pages top out at 16 724 bytes in the reference definitions. The wire
/// offsets are 16-bit, so a larger page could never be read back in full. A
/// hostile definition could declare billions and abort while encoding.
const MAX_PAGE_BYTES: usize = 65_536;

/// What one page decode did.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DecodeReport {
    /// Variables whose values were taken from the page.
    pub decoded: usize,
    /// Variables that were not decoded: no address, no shape, too small, a bit
    /// field outside its storage word, or a word that is not an integer of at
    /// most 32 bits.
    pub skipped: usize,
    /// Page bytes consumed by the variables that were decoded.
    pub bytes: usize,
}

impl DecodeReport {
    /// Whether every variable on the page was decoded.
    pub fn is_complete(&self) -> bool {
        self.skipped == 0
    }
}

/// Reads one page's bytes into `tune`.
///
/// The image is expected to be a snapshot of the tune; values are overwritten
/// in place, so a caller can decode several pages into one image. Values are
/// stored **raw**, exactly as the ECU sent them.
///
/// # Errors
///
/// Returns [`TuneError::UnknownPage`] for a page the layout does not declare
/// and [`TuneError::ShortPage`] when the image is shorter than the page.
pub fn decode_page(
    layout: &TuneLayout,
    page: IniPage,
    bytes: &[u8],
    tune: &mut Tune,
) -> Result<DecodeReport, TuneError> {
    let Some(page_layout) = layout.page(page) else {
        return Err(TuneError::UnknownPage { page });
    };
    if bytes.len() < usize::try_from(page_layout.size).unwrap_or(0) {
        return Err(TuneError::ShortPage {
            page,
            got: bytes.len(),
            expected: usize::try_from(page_layout.size).unwrap_or(0),
        });
    }

    let mut report = DecodeReport::default();
    for index in &page_layout.variables {
        let variable = &layout.variables()[*index];
        let Some(start) = offset_of(variable, page_layout.start_offset) else {
            report.skipped += 1;
            continue;
        };
        match variable.shape {
            EntryShape::Text { len } => {
                let Some(slice) = bytes.get(start..start.saturating_add(len)) else {
                    report.skipped += 1;
                    continue;
                };
                let text: String = slice.iter().copied().map(char::from).collect();
                if tune
                    .set_image_text(&variable.name, text.trim_end_matches('\0'))
                    .is_ok()
                {
                    report.decoded += 1;
                    report.bytes += slice.len();
                } else {
                    report.skipped += 1;
                }
            }
            EntryShape::Scalar | EntryShape::Bits { .. } => {
                let width = type_width(variable.data_type);
                let Some(raw) = read_value(bytes, start, variable.data_type, layout.endianness())
                else {
                    report.skipped += 1;
                    continue;
                };
                let raw = match variable.shape {
                    EntryShape::Bits { low, high, .. } => {
                        if bit_field_can_round_trip(variable.data_type, low, high) {
                            extract_bits(raw, low, high).map(f64::from)
                        } else {
                            None
                        }
                    }
                    _ => Some(raw),
                };
                let Some(raw) = raw else {
                    report.skipped += 1;
                    continue;
                };
                if tune.set_image_raw(&variable.name, 0, raw).is_ok() {
                    report.decoded += 1;
                    report.bytes += width;
                } else {
                    report.skipped += 1;
                }
            }
            EntryShape::Array { cols, rows } => {
                let count = cols.saturating_mul(rows);
                let width = type_width(variable.data_type);
                let mut decoded = 0usize;
                for element in 0..count {
                    let at = start.saturating_add(element.saturating_mul(width));
                    let Some(raw) = read_value(bytes, at, variable.data_type, layout.endianness())
                    else {
                        break;
                    };
                    if tune.set_image_raw(&variable.name, element, raw).is_err() {
                        break;
                    }
                    decoded += 1;
                }
                if decoded == count && count > 0 {
                    report.decoded += 1;
                    report.bytes += count.saturating_mul(width);
                } else {
                    report.skipped += 1;
                }
            }
        }
    }

    Ok(report)
}

/// Writes one page's values into a fresh byte image.
///
/// Non-bits values are written first, then each `bits` constant merges into its
/// word, so every other bit of that word keeps what another constant declared.
/// A value whose address is unknown or a bit field the codec refuses is
/// skipped; a page image is never shifted to make one fit.
///
/// # Errors
///
/// Returns [`TuneError::UnknownPage`] for a page the layout does not declare,
/// [`TuneError::PageTooLarge`] when the declared size is over 64 KiB (65 536
/// bytes), and [`TuneError::TextNotLatin1`] when a text value contains a
/// character the wire format cannot carry.
pub fn encode_page(layout: &TuneLayout, page: IniPage, tune: &Tune) -> Result<Vec<u8>, TuneError> {
    let Some(page_layout) = layout.page(page) else {
        return Err(TuneError::UnknownPage { page });
    };
    let size = usize::try_from(page_layout.size).unwrap_or(usize::MAX);
    if size > MAX_PAGE_BYTES {
        return Err(TuneError::PageTooLarge {
            page,
            size,
            limit: MAX_PAGE_BYTES,
        });
    }

    let mut bytes = vec![0u8; size];

    // Non-bits values fill their words first, then bits fields merge into the
    // result, so declaration order never decides which value survives. A
    // variable another one covers is a view of the same bytes and must not
    // overwrite the value that owns them.
    for bits_only in [false, true] {
        for index in &page_layout.variables {
            if layout.is_alias_at(*index) {
                continue;
            }
            let variable = &layout.variables()[*index];
            let is_bits = matches!(variable.shape, EntryShape::Bits { .. });
            if is_bits != bits_only {
                continue;
            }

            let Some(start) = offset_of(variable, page_layout.start_offset) else {
                continue;
            };
            match variable.shape {
                EntryShape::Text { len } => {
                    // A text constant whose address did not land inside the page is
                    // skipped, like an unresolved numeric one. `start == len` is a
                    // valid empty write; beyond it is a definition bug.
                    if start >= bytes.len() {
                        continue;
                    }
                    let Ok(text) = tune.text(&variable.name) else {
                        continue;
                    };
                    let source = oxtuner_encoding::encode_latin1(text).map_err(|error| {
                        TuneError::TextNotLatin1 {
                            name: variable.name.clone(),
                            character: error.character,
                        }
                    })?;
                    let end = start.saturating_add(len).min(bytes.len());
                    let take = source.len().min(end - start);
                    bytes[start..end].fill(0);
                    bytes[start..start + take].copy_from_slice(&source[..take]);
                }
                EntryShape::Scalar => {
                    let Ok(raw) = tune.raw_at(&variable.name, 0) else {
                        continue;
                    };
                    write_value(
                        &mut bytes,
                        start,
                        variable.data_type,
                        layout.endianness(),
                        raw,
                    );
                }
                EntryShape::Bits { low, high, .. } => {
                    // Refuse before reading the word: rewriting an unchanged wide
                    // word through f64 would still lose low bits.
                    if !bit_field_can_round_trip(variable.data_type, low, high) {
                        continue;
                    }

                    let Ok(value) = tune.raw_at(&variable.name, 0) else {
                        continue;
                    };
                    let Some(existing) =
                        read_value(&bytes, start, variable.data_type, layout.endianness())
                    else {
                        continue;
                    };
                    let merged = merge_bits(existing, value, variable.data_type, low, high);
                    write_value(
                        &mut bytes,
                        start,
                        variable.data_type,
                        layout.endianness(),
                        merged,
                    );
                }
                EntryShape::Array { cols, rows } => {
                    let count = cols.saturating_mul(rows);
                    let width = type_width(variable.data_type);
                    for element in 0..count {
                        let Ok(raw) = tune.raw_at(&variable.name, element) else {
                            break;
                        };
                        let at = start.saturating_add(element.saturating_mul(width));
                        write_value(&mut bytes, at, variable.data_type, layout.endianness(), raw);
                    }
                }
            }
        }
    }

    Ok(bytes)
}

/// Writes the named variables' values into an existing page image.
///
/// This is the delta write path: a page read from the ECU is the base, and only
/// variables whose working value changed are repainted into it. Every other
/// byte — controller-owned values, fields this build does not model, and the
/// untouched bits of a shared word — stays exactly as the ECU sent it. Writing
/// a full [`encode_page`] image would overwrite those with defaults.
///
/// Names are matched case-insensitively, as the layout is. A name that is not on
/// this page contributes nothing, and neither does a bit field the codec
/// refuses: the base bytes stay as they are.
///
/// # Errors
///
/// Returns [`TuneError::UnknownPage`] for an undeclared page,
/// [`TuneError::ShortPage`] when the base image is shorter than the page, and
/// [`TuneError::TextNotLatin1`] when a changed text value contains a character
/// the wire format cannot carry.
pub fn patch_page(
    layout: &TuneLayout,
    page: IniPage,
    base: &[u8],
    tune: &Tune,
    variables: &[impl AsRef<str>],
) -> Result<Vec<u8>, TuneError> {
    let Some(page_layout) = layout.page(page) else {
        return Err(TuneError::UnknownPage { page });
    };
    let size = usize::try_from(page_layout.size).unwrap_or(0);
    if base.len() < size {
        return Err(TuneError::ShortPage {
            page,
            got: base.len(),
            expected: size,
        });
    }

    let mut bytes = base[..size].to_vec();
    let changed: std::collections::HashSet<String> = variables
        .iter()
        .map(|name| name.as_ref().to_ascii_lowercase())
        .collect();

    // Apply changed scalars, arrays and text first, then merge changed bits
    // fields, so a shared word keeps every part that changed.
    for bits_only in [false, true] {
        for index in &page_layout.variables {
            let variable = &layout.variables()[*index];
            let is_bits = matches!(variable.shape, EntryShape::Bits { .. });
            if is_bits != bits_only {
                continue;
            }
            if !changed.contains(&variable.name.to_ascii_lowercase()) {
                continue;
            }

            let Some(start) = offset_of(variable, page_layout.start_offset) else {
                continue;
            };
            match variable.shape {
                EntryShape::Text { len } => {
                    if start >= bytes.len() {
                        continue;
                    }
                    let Ok(text) = tune.text(&variable.name) else {
                        continue;
                    };
                    let source = oxtuner_encoding::encode_latin1(text).map_err(|error| {
                        TuneError::TextNotLatin1 {
                            name: variable.name.clone(),
                            character: error.character,
                        }
                    })?;
                    let end = start.saturating_add(len).min(bytes.len());
                    let take = source.len().min(end - start);
                    bytes[start..end].fill(0);
                    bytes[start..start + take].copy_from_slice(&source[..take]);
                }
                EntryShape::Scalar => {
                    let Ok(raw) = tune.raw_at(&variable.name, 0) else {
                        continue;
                    };
                    write_value(
                        &mut bytes,
                        start,
                        variable.data_type,
                        layout.endianness(),
                        raw,
                    );
                }
                EntryShape::Bits { low, high, .. } => {
                    // Refuse before reading the word: rewriting an unchanged wide
                    // word through f64 would still lose low bits.
                    if !bit_field_can_round_trip(variable.data_type, low, high) {
                        continue;
                    }

                    let Ok(value) = tune.raw_at(&variable.name, 0) else {
                        continue;
                    };
                    let Some(existing) =
                        read_value(&bytes, start, variable.data_type, layout.endianness())
                    else {
                        continue;
                    };
                    let merged = merge_bits(existing, value, variable.data_type, low, high);
                    write_value(
                        &mut bytes,
                        start,
                        variable.data_type,
                        layout.endianness(),
                        merged,
                    );
                }
                EntryShape::Array { cols, rows } => {
                    let count = cols.saturating_mul(rows);
                    let width = type_width(variable.data_type);
                    for element in 0..count {
                        let Ok(raw) = tune.raw_at(&variable.name, element) else {
                            break;
                        };
                        let at = start.saturating_add(element.saturating_mul(width));
                        write_value(&mut bytes, at, variable.data_type, layout.endianness(), raw);
                    }
                }
            }
        }
    }

    Ok(bytes)
}

/// A variable's byte offset in its page, when it can be placed.
fn offset_of(variable: &VariableSpec, start_offset: u32) -> Option<usize> {
    let address = variable.address?;
    let offset = address.checked_sub(start_offset)?;
    usize::try_from(offset).ok()
}

fn type_width(data_type: DataType) -> usize {
    usize::try_from(data_type.size()).unwrap_or(usize::MAX)
}

/// Replaces a bit range in an existing word, keeping every other bit.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::as_conversions,
    reason = "bit constants live in the low 32 bits of a word and return to f64 storage"
)]
fn merge_bits(existing: f64, value: f64, data_type: DataType, low: u8, high: u8) -> f64 {
    if high >= 32 {
        return existing;
    }

    let mask = bit_mask(low, high);
    let merged = (existing as i64 & !i64::from(mask)) | (((value as i64) << low) & i64::from(mask));

    // A signed word is stored as its two's-complement pattern, so present the
    // merged bits with that width's sign before the write clamps it.
    let merged = if matches!(data_type, DataType::S08 | DataType::S16 | DataType::S32) {
        sign_extend(merged, type_width(data_type).saturating_mul(8))
    } else {
        merged
    };

    merged as f64
}

/// Reinterprets the low `bits` of `word` as a signed value.
fn sign_extend(word: i64, bits: usize) -> i64 {
    if bits >= 64 {
        return word;
    }

    let shift = 64 - bits;
    (word << shift) >> shift
}

/// The mask for `low..=high`.
///
/// The precondition is `high < 32`: callers refuse a field that reaches past
/// the word, so `low` is below 32 too. The clamp only keeps the shift below 32
/// so it cannot overflow, and it never makes a straddling range valid.
fn bit_mask(low: u8, high: u8) -> u32 {
    let low = u32::from(low);
    let high = u32::from(high);
    if low >= 32 {
        return 0;
    }

    let width = high.saturating_sub(low).saturating_add(1).min(32 - low);
    if width >= 32 {
        u32::MAX
    } else {
        ((1u32 << width) - 1) << low
    }
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::as_conversions,
    reason = "the value is checked finite before the bit-field cast, and negative words use their two's-complement pattern"
)]
fn extract_bits(raw: f64, low: u8, high: u8) -> Option<u32> {
    if high >= 32 {
        return None;
    }

    if !raw.is_finite() {
        return None;
    }

    // A signed word arrives sign-extended. Bit fields read the two's-complement
    // pattern of the low bits, so keep that pattern rather than refusing it.
    let word = if raw < 0.0 {
        (raw as i64) as u64
    } else {
        raw as u64
    };
    let shift = u32::from(low);
    let mask = u64::from(bit_mask(low, high)) >> shift;
    u32::try_from((word >> shift) & mask).ok()
}

/// Reads one value of `data_type` at `start`, in the layout's byte order.
#[expect(
    clippy::cast_precision_loss,
    clippy::as_conversions,
    reason = "64-bit raw values are stored as f64, the tune model's documented limit"
)]
fn read_value(
    bytes: &[u8],
    start: usize,
    data_type: DataType,
    endianness: Endianness,
) -> Option<f64> {
    let width = type_width(data_type);
    let slice = bytes.get(start..start.checked_add(width)?)?;

    // The value is assembled as if the wire were little-endian, so a big-endian
    // definition only has to reverse its significant bytes.
    let mut ordered = [0u8; 8];
    match endianness {
        Endianness::Little => ordered[..width].copy_from_slice(slice),
        Endianness::Big => {
            for index in 0..width {
                ordered[index] = slice[width - 1 - index];
            }
        }
    }

    Some(match data_type {
        DataType::U08 | DataType::M08 => f64::from(ordered[0]),
        DataType::S08 => f64::from(ordered[0].cast_signed()),
        DataType::U16 => f64::from(u16::from_le_bytes([ordered[0], ordered[1]])),
        DataType::S16 => f64::from(i16::from_le_bytes([ordered[0], ordered[1]])),
        DataType::U32 => f64::from(u32::from_le_bytes([
            ordered[0], ordered[1], ordered[2], ordered[3],
        ])),
        DataType::S32 => f64::from(i32::from_le_bytes([
            ordered[0], ordered[1], ordered[2], ordered[3],
        ])),
        DataType::F32 => f64::from(f32::from_le_bytes([
            ordered[0], ordered[1], ordered[2], ordered[3],
        ])),
        DataType::U64 => u64::from_le_bytes(ordered) as f64,
        DataType::S64 => i64::from_le_bytes(ordered) as f64,
        DataType::Ascii => return None,
    })
}

/// Writes one value at `start`, in the layout's byte order.
fn write_value(
    bytes: &mut [u8],
    start: usize,
    data_type: DataType,
    endianness: Endianness,
    raw: f64,
) {
    let width = type_width(data_type);
    let Some(end) = start.checked_add(width) else {
        return;
    };
    if end > bytes.len() {
        return;
    }

    if data_type == DataType::Ascii {
        return;
    }
    let little = wire_bytes(raw, data_type);
    match endianness {
        Endianness::Little => bytes[start..end].copy_from_slice(&little[..width]),
        Endianness::Big => {
            for index in 0..width {
                bytes[start + index] = little[width - 1 - index];
            }
        }
    }
}

/// One raw value as the wire stores it, least significant byte first.
///
/// The encoder writes these bytes (reordering for a big-endian link) and the
/// value comparison reads them, so "same cell" and "what is sent" cannot drift.
/// An integer type rounds and clamps here exactly as it does on the wire.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::as_conversions,
    reason = "each cast is clamped to the storage type's range or is the documented f64 raw representation"
)]
pub(crate) fn wire_bytes(raw: f64, data_type: DataType) -> [u8; 8] {
    match data_type {
        DataType::U08 | DataType::M08 => [raw.round().clamp(0.0, 255.0) as u8, 0, 0, 0, 0, 0, 0, 0],
        DataType::S08 => {
            let value = raw.round().clamp(-128.0, 127.0) as i8;
            [value as u8, 0, 0, 0, 0, 0, 0, 0]
        }
        DataType::U16 => {
            let mut buffer = [0u8; 8];
            buffer[..2].copy_from_slice(&(raw.round().clamp(0.0, 65_535.0) as u16).to_le_bytes());
            buffer
        }
        DataType::S16 => {
            let mut buffer = [0u8; 8];
            buffer[..2]
                .copy_from_slice(&(raw.round().clamp(-32_768.0, 32_767.0) as i16).to_le_bytes());
            buffer
        }
        DataType::U32 => {
            let mut buffer = [0u8; 8];
            buffer[..4]
                .copy_from_slice(&(raw.round().clamp(0.0, 4_294_967_295.0) as u32).to_le_bytes());
            buffer
        }
        DataType::S32 => {
            let mut buffer = [0u8; 8];
            buffer[..4].copy_from_slice(
                &(raw.round().clamp(-2_147_483_648.0, 2_147_483_647.0) as i32).to_le_bytes(),
            );
            buffer
        }
        DataType::F32 => {
            let mut buffer = [0u8; 8];
            buffer[..4].copy_from_slice(&(raw as f32).to_le_bytes());
            buffer
        }
        DataType::U64 => (raw.max(0.0) as u64).to_le_bytes(),
        DataType::S64 => (raw as i64).to_le_bytes(),
        DataType::Ascii => [0u8; 8],
    }
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unwrap_used,
        reason = "unit tests may unwrap; a failed call is a failed test"
    )]
    use std::sync::Arc;

    use oxtuner_ini::SettingsContext;

    use super::*;

    fn definition() -> oxtuner_ini::Definition {
        oxtuner_ini::Definition::parse(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
endianness = little
nPages = 1
pageSize = 10
page = 1
rpm = scalar, U16, 0, \"rpm\", 1.0, 0.0
advance = scalar, S08, 2, \"deg\", 1.0, 0.0
boost = scalar, U08, 3, \"kpa\", 2.0, 0.0
name = string, ASCII, 4, 4
",
            &SettingsContext::new(),
        )
        .expect("parses")
    }

    #[test]
    fn a_page_decodes_into_raw_values() {
        let definition = definition();
        let layout = TuneLayout::build(&definition, &|_| None);
        let mut tune = Tune::new(Arc::new(layout.clone()));

        let report = decode_page(
            &layout,
            IniPage::FIRST,
            &[0x10, 0x27, 0xFB, 0x14, b'o', b'x', b' ', b' ', 0, 0],
            &mut tune,
        )
        .expect("decodes");

        assert!(report.is_complete(), "{report:?}");
        assert_eq!(tune.raw_at("rpm", 0).unwrap(), 10_000.0);
        assert_eq!(tune.raw_at("advance", 0).unwrap(), -5.0);
        assert_eq!(tune.raw_at("boost", 0).unwrap(), 20.0);
        assert_eq!(tune.text("name").unwrap().trim_end(), "ox");
    }

    #[test]
    fn a_page_round_trips_through_the_codec() {
        let definition = definition();
        let layout = TuneLayout::build(&definition, &|_| None);
        let mut tune = Tune::new(Arc::new(layout.clone()));
        let bytes = [0x10, 0x27, 0xFB, 0x14, b'o', b'x', b' ', b' ', 0, 0];

        decode_page(&layout, IniPage::FIRST, &bytes, &mut tune).expect("decodes");
        let encoded = encode_page(&layout, IniPage::FIRST, &tune).expect("encodes");

        assert_eq!(&encoded[..4], &bytes[..4]);
        assert_eq!(&encoded[4..6], b"ox");
    }

    #[test]
    fn a_big_endian_page_decodes_and_encodes_in_wire_order() {
        let definition = oxtuner_ini::Definition::parse(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
endianness = big
nPages = 1
pageSize = 4
page = 1
rpm = scalar, U16, 0, \"rpm\", 1.0, 0.0
",
            &SettingsContext::new(),
        )
        .expect("parses");
        let layout = TuneLayout::build(&definition, &|_| None);
        let mut tune = Tune::new(Arc::new(layout.clone()));
        let bytes = [0x27, 0x10, 0, 0];

        decode_page(&layout, IniPage::FIRST, &bytes, &mut tune).expect("decodes");
        assert_eq!(tune.raw_at("rpm", 0).unwrap(), 10_000.0);

        let encoded = encode_page(&layout, IniPage::FIRST, &tune).expect("encodes");
        assert_eq!(&encoded[..2], &[0x27, 0x10]);
    }

    #[test]
    fn a_mask_covers_only_the_bits_inside_the_word() {
        assert_eq!(bit_mask(0, 0), 1);
        assert_eq!(bit_mask(0, 15), 0xFFFF);
        assert_eq!(bit_mask(31, 31), 0x8000_0000);
        assert_eq!(bit_mask(0, 31), u32::MAX);
        assert_eq!(bit_mask(31, 39), 0x8000_0000, "clamped to the word");
        assert_eq!(bit_mask(40, 47), 0, "no bits exist past the word");
    }

    #[test]
    fn a_bit_field_past_the_word_is_skipped_not_a_panic() {
        let definition = oxtuner_ini::Definition::parse(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
endianness = little
nPages = 1
pageSize = 4
page = 1
wide = bits, U16, 0, [40:47]
",
            &SettingsContext::new(),
        )
        .expect("parses");
        let layout = TuneLayout::build(&definition, &|_| None);
        let mut tune = Tune::new(Arc::new(layout.clone()));
        tune.set_image_raw("wide", 0, 1234.0).expect("known");

        let report =
            decode_page(&layout, IniPage::FIRST, &[0xFF, 0xFF, 0, 0], &mut tune).expect("decodes");

        assert_eq!(report.skipped, 1);
        assert_eq!(
            tune.raw_at("wide", 0).expect("wide"),
            1234.0,
            "a refused field must not be written"
        );
    }

    #[test]
    fn latin1_text_round_trips_through_the_codec() {
        let definition = definition();
        let layout = TuneLayout::build(&definition, &|_| None);
        let mut tune = Tune::new(Arc::new(layout.clone()));
        let bytes = [0x10, 0x27, 0xFB, 0x14, 0xE9, b'x', 0, 0, 0, 0];

        decode_page(&layout, IniPage::FIRST, &bytes, &mut tune).expect("decodes");
        let encoded = encode_page(&layout, IniPage::FIRST, &tune).expect("encodes");

        assert_eq!(tune.text("name").expect("name"), "\u{e9}x");
        assert_eq!(
            &encoded[4..6],
            &[0xE9, b'x'],
            "the byte survives the round trip"
        );

        tune.set_text("name", "\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}")
            .expect("text is set");
        let truncated = encode_page(&layout, IniPage::FIRST, &tune).expect("encodes");
        assert_eq!(
            &truncated[4..8],
            &[0xE9, 0xE9, 0xE9, 0xE9],
            "the cut lands between characters, never inside one"
        );
    }

    #[test]
    fn text_that_cannot_be_latin1_is_refused_on_write() {
        let definition = definition();
        let layout = TuneLayout::build(&definition, &|_| None);
        let mut tune = Tune::new(Arc::new(layout.clone()));
        tune.set_text("name", "\u{2192}").expect("text is set");

        let encoded = encode_page(&layout, IniPage::FIRST, &tune)
            .expect_err("a text value the wire cannot carry must not be dropped");
        let base = [1u8; 10];
        let patched = patch_page(&layout, IniPage::FIRST, &base, &tune, &["name".to_owned()])
            .expect_err("the delta write must not succeed without the value");

        for error in [encoded, patched] {
            assert!(
                matches!(
                    &error,
                    TuneError::TextNotLatin1 { name, character }
                        if name == "name" && *character == '\u{2192}'
                ),
                "{error}"
            );
            assert!(error.to_string().contains("name"), "{error}");
        }
    }

    #[test]
    fn a_variable_without_an_address_is_skipped_not_placed_at_zero() {
        let definition = oxtuner_ini::Definition::parse(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
endianness = little
nPages = 1
pageSize = 4
page = 1
broken = scalar, U08, not-a-number, \"x\", 1.0, 0.0
value = scalar, U08, 2, \"x\", 1.0, 0.0
",
            &SettingsContext::new(),
        )
        .expect("parses");
        let layout = TuneLayout::build(&definition, &|_| None);
        let mut tune = Tune::new(Arc::new(layout.clone()));

        let report =
            decode_page(&layout, IniPage::FIRST, &[1, 2, 3, 4], &mut tune).expect("decodes");

        assert_eq!(report.skipped, 1);
        assert_eq!(tune.raw_at("broken", 0).unwrap(), 0.0);
        assert_eq!(tune.raw_at("value", 0).unwrap(), 3.0);
    }

    #[test]
    fn a_text_constant_past_the_page_end_is_skipped_not_a_panic() {
        // A mis-resolved string address must be ignored, not sliced out of
        // bounds. `encode_page` has no report, so the observable contract is:
        // it returns without panicking and writes nothing there.
        let definition = oxtuner_ini::Definition::parse(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
endianness = little
nPages = 1
pageSize = 4
page = 1
name = string, ASCII, 99, 4
value = scalar, U08, 0, \"x\", 1.0, 0.0
",
            &SettingsContext::new(),
        )
        .expect("parses");
        // The text address (99) lies past the 4-byte page: a definition bug the
        // encoder must skip instead of slicing out of bounds.
        let layout = TuneLayout::build(&definition, &|_| None);
        let tune = Tune::new(Arc::new(layout.clone()));

        let bytes = encode_page(&layout, IniPage::FIRST, &tune).expect("encodes");

        assert_eq!(bytes.len(), 4);
    }

    #[test]
    fn an_oversized_page_is_refused_before_allocating() {
        let definition = oxtuner_ini::Definition::parse(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
nPages = 1
pageSize = 2000000
page = 1
value = scalar, U08, 0, \"x\", 1, 0
",
            &SettingsContext::new(),
        )
        .expect("parses");
        let layout = TuneLayout::build(&definition, &|_| None);
        let tune = Tune::new(Arc::new(layout.clone()));

        let error = encode_page(&layout, IniPage::FIRST, &tune).expect_err("too large");

        assert!(
            matches!(
                error,
                TuneError::PageTooLarge {
                    limit: MAX_PAGE_BYTES,
                    ..
                }
            ),
            "{error}"
        );
    }

    #[test]
    fn a_page_at_the_limit_is_encoded_and_one_byte_more_is_refused() {
        let definition = |size: u32| {
            oxtuner_ini::Definition::parse(
                &format!(
                    "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
nPages = 1
pageSize = {size}
page = 1
value = scalar, U08, 0, \"x\", 1, 0
"
                ),
                &SettingsContext::new(),
            )
            .expect("parses")
        };

        let at_limit = TuneLayout::build(&definition(65_536), &|_| None);
        let tune = Tune::new(Arc::new(at_limit.clone()));
        let encoded = encode_page(&at_limit, IniPage::FIRST, &tune).expect("at the limit");
        assert_eq!(encoded.len(), 65_536);

        let over = TuneLayout::build(&definition(65_537), &|_| None);
        let tune = Tune::new(Arc::new(over.clone()));
        let error = encode_page(&over, IniPage::FIRST, &tune).expect_err("one byte over");

        assert!(matches!(error, TuneError::PageTooLarge { .. }), "{error}");
    }

    #[test]
    fn a_short_page_is_refused() {
        let definition = definition();
        let layout = TuneLayout::build(&definition, &|_| None);
        let mut tune = Tune::new(Arc::new(layout.clone()));

        let error =
            decode_page(&layout, IniPage::FIRST, &[0u8; 4], &mut tune).expect_err("too short");

        assert!(matches!(error, TuneError::ShortPage { .. }));
    }

    #[test]
    fn a_patch_writes_only_the_named_variable() {
        let definition = definition();
        let layout = TuneLayout::build(&definition, &|_| None);
        let mut tune = Tune::new(Arc::new(layout.clone()));
        tune.set_user("rpm", 4200.0).expect("known");
        let base = [0x10, 0x27, 0xFB, 0x14, b'o', b'x', b' ', b' ', 0, 0];

        let patched = patch_page(&layout, IniPage::FIRST, &base, &tune, &["rpm".to_owned()])
            .expect("patches");

        // 4200 raw = 0x1068.
        assert_eq!(&patched[..2], &[0x68, 0x10]);
        assert_eq!(&patched[2..], &base[2..]);
    }

    #[test]
    fn a_patch_clears_the_rest_of_a_short_text_field() {
        let definition = definition();
        let layout = TuneLayout::build(&definition, &|_| None);
        let mut tune = Tune::new(Arc::new(layout.clone()));
        tune.set_text("name", "ox").expect("known");

        let patched = patch_page(
            &layout,
            IniPage::FIRST,
            &[0, 0, 0, 0, b'A', b'B', b'C', b'D', 0, 0],
            &tune,
            &["name".to_owned()],
        )
        .expect("patches");

        assert_eq!(&patched[4..8], &[b'o', b'x', 0, 0]);
    }

    #[test]
    fn a_bit_field_that_straddles_the_word_is_skipped() {
        let definition = oxtuner_ini::Definition::parse(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
endianness = little
nPages = 1
pageSize = 4
page = 1
wide = bits, U16, 0, [8:23]
",
            &SettingsContext::new(),
        )
        .expect("parses");
        let layout = TuneLayout::build(&definition, &|_| None);
        let mut tune = Tune::new(Arc::new(layout.clone()));
        tune.set_image_raw("wide", 0, 1234.0).expect("known");

        let report =
            decode_page(&layout, IniPage::FIRST, &[0xFF, 0xFF, 0, 0], &mut tune).expect("decodes");

        assert_eq!(report.skipped, 1);
        assert_eq!(
            tune.raw_at("wide", 0).expect("wide"),
            1234.0,
            "a refused field must not be written"
        );
    }

    #[test]
    fn a_patch_of_a_bit_field_past_the_word_leaves_the_word_alone() {
        let definition = oxtuner_ini::Definition::parse(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
endianness = little
nPages = 1
pageSize = 4
page = 1
wide = bits, U16, 0, [40:47]
",
            &SettingsContext::new(),
        )
        .expect("parses");
        let layout = TuneLayout::build(&definition, &|_| None);
        let tune = Tune::new(Arc::new(layout.clone()));
        let base = [0xFF, 0xFF, 0, 0];

        let patched = patch_page(&layout, IniPage::FIRST, &base, &tune, &["wide".to_owned()])
            .expect("patches");

        assert_eq!(patched, base, "a skipped field must not rewrite the word");
    }

    #[test]
    fn a_patch_of_a_u64_bit_field_keeps_every_byte() {
        let definition = oxtuner_ini::Definition::parse(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
endianness = little
nPages = 1
pageSize = 8
page = 1
wide = bits, U64, 0, [0:7]
",
            &SettingsContext::new(),
        )
        .expect("parses");
        let layout = TuneLayout::build(&definition, &|_| None);
        let tune = Tune::new(Arc::new(layout.clone()));
        let base = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];

        let patched = patch_page(&layout, IniPage::FIRST, &base, &tune, &["wide".to_owned()])
            .expect("patches");

        assert_eq!(patched, base, "a skipped field must not rewrite the word");
    }

    #[test]
    fn a_u64_bit_field_is_skipped_on_decode() {
        let definition = oxtuner_ini::Definition::parse(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
endianness = little
nPages = 1
pageSize = 8
page = 1
wide = bits, U64, 0, [0:7]
",
            &SettingsContext::new(),
        )
        .expect("parses");
        let layout = TuneLayout::build(&definition, &|_| None);
        let mut tune = Tune::new(Arc::new(layout.clone()));
        tune.set_image_raw("wide", 0, 1234.0).expect("known");

        let report = decode_page(&layout, IniPage::FIRST, &[0xFF; 8], &mut tune).expect("decodes");

        assert_eq!(report.skipped, 1, "a wide word cannot survive a write-back");
        assert_eq!(tune.raw_at("wide", 0).expect("wide"), 1234.0);
    }

    #[test]
    fn an_f32_bit_field_is_skipped_on_patch() {
        let definition = oxtuner_ini::Definition::parse(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
endianness = little
nPages = 1
pageSize = 4
page = 1
wide = bits, F32, 0, [0:7]
",
            &SettingsContext::new(),
        )
        .expect("parses");
        let layout = TuneLayout::build(&definition, &|_| None);
        let tune = Tune::new(Arc::new(layout.clone()));
        let base = [0x00, 0x00, 0x80, 0x3F];

        let patched = patch_page(&layout, IniPage::FIRST, &base, &tune, &["wide".to_owned()])
            .expect("patches");

        assert_eq!(patched, base, "a float word must not be rewritten");
    }

    #[test]
    fn a_full_width_u32_bit_field_decodes() {
        let definition = oxtuner_ini::Definition::parse(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
endianness = little
nPages = 1
pageSize = 4
page = 1
wide = bits, U32, 0, [0:31]
",
            &SettingsContext::new(),
        )
        .expect("parses");
        let layout = TuneLayout::build(&definition, &|_| None);
        let mut tune = Tune::new(Arc::new(layout.clone()));

        let report = decode_page(
            &layout,
            IniPage::FIRST,
            &[0xEF, 0xBE, 0xAD, 0xDE],
            &mut tune,
        )
        .expect("decodes");

        assert!(report.is_complete(), "{report:?}");
        assert_eq!(
            tune.raw_at("wide", 0).expect("wide"),
            f64::from(0xDEAD_BEEFu32)
        );
    }

    #[test]
    fn a_signed_word_bit_field_round_trips() {
        let definition = oxtuner_ini::Definition::parse(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
endianness = little
nPages = 1
pageSize = 4
page = 1
wide = bits, S08, 0, [0:2]
",
            &SettingsContext::new(),
        )
        .expect("parses");
        let layout = TuneLayout::build(&definition, &|_| None);
        let mut tune = Tune::new(Arc::new(layout.clone()));
        let base = [0xFD, 0, 0, 0];

        let report = decode_page(&layout, IniPage::FIRST, &base, &mut tune).expect("decodes");
        assert!(report.is_complete(), "{report:?}");
        assert_eq!(tune.raw_at("wide", 0).expect("wide"), 5.0);

        tune.set_image_raw("wide", 0, 2.0).expect("known");
        let patched = patch_page(&layout, IniPage::FIRST, &base, &tune, &["wide".to_owned()])
            .expect("patches");

        assert_eq!(patched[0], 0xFA, "upper bits stay set, the field becomes 2");
    }

    #[test]
    fn a_signed_bit_field_can_set_the_sign_bit() {
        let definition = oxtuner_ini::Definition::parse(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
endianness = little
nPages = 1
pageSize = 4
page = 1
wide = bits, S08, 0, [7:7]
",
            &SettingsContext::new(),
        )
        .expect("parses");
        let layout = TuneLayout::build(&definition, &|_| None);
        let mut tune = Tune::new(Arc::new(layout.clone()));
        tune.set_image_raw("wide", 0, 1.0).expect("known");

        let encoded = encode_page(&layout, IniPage::FIRST, &tune).expect("encodes");

        assert_eq!(
            encoded[0], 0x80,
            "the sign bit is the two's-complement -128"
        );
    }

    #[test]
    fn an_m08_bit_field_decodes() {
        let definition = oxtuner_ini::Definition::parse(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
endianness = little
nPages = 1
pageSize = 1
page = 1
wide = bits, M08, 0, [0:7]
",
            &SettingsContext::new(),
        )
        .expect("parses");
        let layout = TuneLayout::build(&definition, &|_| None);
        let mut tune = Tune::new(Arc::new(layout.clone()));

        let report = decode_page(&layout, IniPage::FIRST, &[0x5A], &mut tune).expect("decodes");

        assert!(report.is_complete(), "{report:?}");
        assert_eq!(tune.raw_at("wide", 0).expect("wide"), f64::from(0x5Au8));
    }

    #[test]
    fn bits_merge_after_scalars_regardless_of_declaration_order() {
        let definition = oxtuner_ini::Definition::parse(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
endianness = little
nPages = 1
pageSize = 4
page = 1
fan = bits, U08, 0, [1:1]
status = scalar, U08, 0, \"bits\", 1, 0
",
            &SettingsContext::new(),
        )
        .expect("parses");
        let layout = TuneLayout::build(&definition, &|_| None);
        let mut tune = Tune::new(Arc::new(layout.clone()));
        tune.set_image_raw("status", 0, 1.0).expect("status");
        tune.set_image_raw("fan", 0, 1.0).expect("fan");

        let encoded = encode_page(&layout, IniPage::FIRST, &tune).expect("encodes");

        assert_eq!(
            encoded[0], 0b0000_0011,
            "both the scalar and the bit survive"
        );
    }

    #[test]
    fn a_full_width_u32_bit_field_encodes() {
        let definition = oxtuner_ini::Definition::parse(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
endianness = little
nPages = 1
pageSize = 4
page = 1
wide = bits, U32, 0, [0:31]
",
            &SettingsContext::new(),
        )
        .expect("parses");
        let layout = TuneLayout::build(&definition, &|_| None);
        let mut tune = Tune::new(Arc::new(layout.clone()));
        tune.set_image_raw("wide", 0, f64::from(0xDEAD_BEEFu32))
            .expect("wide");

        let encoded = encode_page(&layout, IniPage::FIRST, &tune).expect("encodes");

        assert_eq!(encoded, [0xEF, 0xBE, 0xAD, 0xDE]);
    }

    #[test]
    fn a_patch_keeps_the_other_bits_of_a_shared_word() {
        let definition = oxtuner_ini::Definition::parse(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
endianness = little
nPages = 1
pageSize = 4
page = 1
status = scalar, U08, 0, \"bits\", 1.0, 0.0
running = bits, U08, 0, [0:0]
fan = bits, U08, 0, [1:1]
",
            &SettingsContext::new(),
        )
        .expect("parses");
        let layout = TuneLayout::build(&definition, &|_| None);
        let mut tune = Tune::new(Arc::new(layout.clone()));
        tune.set_user("fan", 1.0).expect("known");

        let patched = patch_page(
            &layout,
            IniPage::FIRST,
            &[0b0000_0001, 0, 0, 0],
            &tune,
            &["fan".to_owned()],
        )
        .expect("patches");

        assert_eq!(patched[0], 0b0000_0011, "running keeps its bit");
    }
}
