//! Snapshot and restore of the macOS general pasteboard, item by item and type
//! by type.
//!
//! `arboard` covers plain text and images. Putting the user's clipboard back
//! after we borrowed it needs what it hides: every type of every item, and
//! `changeCount` to tell whether anyone wrote since. Two flows borrow the
//! clipboard and each keeps its own ordering and failure policy:
//! `selection`'s copy fallback refuses outright when anything cannot be
//! captured ([`Unrestorable::Refuse`]); the dictation injector has to paste
//! regardless, so it keeps what it can ([`Unrestorable::Skip`]).

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{
    NSPasteboard, NSPasteboardContentsOptions, NSPasteboardItem, NSPasteboardWriting,
};
use objc2_foundation::{NSArray, NSData, NSString};

/// Types whose payload is not the data on the pasteboard — a promise to be
/// fulfilled later. Capturing the bytes does not capture the promise, so we
/// cannot honestly restore them.
const PROMISE_TYPE_PREFIXES: [&str; 2] = [
    "com.apple.pasteboard.promised",
    "Apple files promise pasteboard type",
];

pub(crate) fn is_promise_type(ty: &str) -> bool {
    PROMISE_TYPE_PREFIXES
        .iter()
        .any(|prefix| ty.starts_with(prefix))
}

/// What the capture does with a type it cannot put back faithfully: a promised
/// type, or lazily-provided data whose owner refused or went away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Unrestorable {
    /// Fail the whole capture; the caller leaves the clipboard alone.
    Refuse,
    /// Leave that type out and keep the rest of the item.
    Skip,
}

/// Add one type's payload to the running snapshot size, refusing past
/// `max_bytes`. Saturating on purpose — an overflow that wrapped would read as
/// "plenty of room left".
pub(crate) fn accumulate_snapshot_bytes(
    total: usize,
    added: usize,
    max_bytes: usize,
) -> Result<usize, String> {
    let total = total.saturating_add(added);
    if total > max_bytes {
        return Err(format!("clipboard exceeds {max_bytes} bytes"));
    }
    Ok(total)
}

/// A captured pasteboard, item by item and type by type.
pub(crate) struct PasteboardSnapshot {
    items: Vec<Vec<(String, Vec<u8>)>>,
    skipped: Vec<String>,
}

impl PasteboardSnapshot {
    /// Read every type of every item. Lazily-provided data is rendered on the
    /// spot, which is the cost of capturing it at all.
    pub(crate) fn capture(
        pasteboard: &NSPasteboard,
        max_bytes: usize,
        unrestorable: Unrestorable,
    ) -> Result<Self, String> {
        let mut items = Vec::new();
        let mut skipped = Vec::new();
        let mut total_bytes = 0usize;

        let Some(pasteboard_items) = pasteboard.pasteboardItems() else {
            // No items at all: nothing to restore, and nothing that can fail.
            return Ok(Self { items, skipped });
        };

        for item in pasteboard_items.iter() {
            let mut captured = Vec::new();
            for ty in item.types().iter() {
                let type_name = ty.to_string();
                let data = if is_promise_type(&type_name) {
                    None
                } else {
                    item.dataForType(&ty)
                };
                let Some(data) = data else {
                    match unrestorable {
                        Unrestorable::Refuse => {
                            return Err(if is_promise_type(&type_name) {
                                format!("promised type {type_name}")
                            } else {
                                format!("unreadable type {type_name}")
                            });
                        }
                        Unrestorable::Skip => {
                            skipped.push(type_name);
                            continue;
                        }
                    }
                };

                total_bytes = accumulate_snapshot_bytes(total_bytes, data.len(), max_bytes)?;
                captured.push((type_name, data.to_vec()));
            }
            // An item reduced to nothing by skipping has nothing to put back.
            if !captured.is_empty() {
                items.push(captured);
            }
        }

        Ok(Self { items, skipped })
    }

    /// Types left out under [`Unrestorable::Skip`], for the log.
    pub(crate) fn skipped_types(&self) -> &[String] {
        &self.skipped
    }

    /// Put the captured contents back.
    ///
    /// Every item is rebuilt *before* the pasteboard is cleared: clearing is
    /// the destructive step, so anything that can fail must fail while the
    /// user's clipboard is still intact. Clearing first and then hitting a
    /// failure mid-rebuild would leave them with an empty or half-restored
    /// clipboard.
    ///
    /// `current_host_only` keeps the write off Universal Clipboard: a restore
    /// is not something the user copied, so it should not reach their other
    /// devices.
    pub(crate) fn restore(
        &self,
        pasteboard: &NSPasteboard,
        current_host_only: bool,
    ) -> Result<(), String> {
        let mut rebuilt: Vec<Retained<NSPasteboardItem>> = Vec::with_capacity(self.items.len());
        for types in &self.items {
            let item = NSPasteboardItem::new();
            for (type_name, bytes) in types {
                let ns_type = NSString::from_str(type_name);
                let ns_data = NSData::with_bytes(bytes);
                if !item.setData_forType(&ns_data, &ns_type) {
                    return Err(format!("setData failed for {type_name}"));
                }
            }
            rebuilt.push(item);
        }

        if current_host_only {
            pasteboard
                .prepareForNewContentsWithOptions(NSPasteboardContentsOptions::CurrentHostOnly);
        } else {
            pasteboard.clearContents();
        }
        if rebuilt.is_empty() {
            return Ok(());
        }

        let writable: Vec<&ProtocolObject<dyn NSPasteboardWriting>> = rebuilt
            .iter()
            .map(|item| ProtocolObject::from_ref(&**item))
            .collect();
        if !pasteboard.writeObjects(&NSArray::from_slice(&writable)) {
            return Err("writeObjects returned false".to_string());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUDGET: usize = 32 * 1024 * 1024;

    #[test]
    fn promise_types_are_recognised() {
        assert!(is_promise_type("com.apple.pasteboard.promised-file-url"));
        assert!(is_promise_type(
            "com.apple.pasteboard.promised-file-content-type"
        ));
        assert!(is_promise_type("Apple files promise pasteboard type"));
        assert!(!is_promise_type("public.utf8-plain-text"));
        assert!(!is_promise_type("public.png"));
    }

    #[test]
    fn the_snapshot_budget_refuses_rather_than_holding_a_huge_clipboard() {
        assert_eq!(accumulate_snapshot_bytes(0, 1024, BUDGET).unwrap(), 1024);
        assert_eq!(
            accumulate_snapshot_bytes(BUDGET - 1, 1, BUDGET).unwrap(),
            BUDGET,
            "exactly at the budget still fits"
        );
        assert!(accumulate_snapshot_bytes(BUDGET, 1, BUDGET).is_err());
    }

    #[test]
    fn the_budget_is_a_running_total_across_types_not_a_per_type_limit() {
        // One item can carry the same content in a dozen flavours; each under
        // the cap while the whole is far over it.
        let half = BUDGET / 2;
        let after_first = accumulate_snapshot_bytes(0, half, BUDGET).unwrap();
        assert!(accumulate_snapshot_bytes(after_first, half + 1, BUDGET).is_err());
    }

    #[test]
    fn an_overflowing_total_refuses_instead_of_wrapping_to_roomy() {
        assert!(accumulate_snapshot_bytes(usize::MAX, usize::MAX, BUDGET).is_err());
    }
}
