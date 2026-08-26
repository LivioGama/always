import AppKit

/// Sizing decisions for the normal (volume-HUD style) status overlay when it
/// renders text — most importantly the live provisional transcript, which can
/// exceed 100 characters with streaming previews. Pure functions so the
/// growth cap and head-truncation behavior are unit-testable without a window.
///
/// Contract:
/// - Width never changes; only height grows, from the classic fixed HUD
///   height (`StatusOverlayWindow.overlayHeight`) up to `maxHeight`.
/// - Past the cap, text is head-truncated (leading "…") so the NEWEST words
///   stay visible — live dictation feedback is about what was just said.
/// - Compact mode is untouched: it stays a fixed single-line pill.
enum OverlayHUDSizing {
    // Normal-mode metrics. `StatusOverlayView` reads these same constants so
    // measurement here and rendering there cannot drift apart.
    static let iconSize: CGFloat = 42
    static let iconLabelSpacing: CGFloat = 10
    static let verticalPadding: CGFloat = 14
    static let horizontalPadding: CGFloat = 20
    static let labelFontSize: CGFloat = 15
    /// Defense cap on rendered lines; `maxTextHeight` (120pt) fits at most 6
    /// wrapped lines of the 15pt label (~18pt each), never a clipped 7th.
    static let maxLines = 6

    static var labelFont: NSFont { .systemFont(ofSize: labelFontSize, weight: .medium) }

    /// Fixed vertical chrome around the label: padding above + icon + spacing
    /// + padding below.
    static var chromeHeight: CGFloat { verticalPadding * 2 + iconSize + iconLabelSpacing }

    /// The classic HUD height — short texts keep exactly this size.
    static var baseHeight: CGFloat { StatusOverlayWindow.overlayHeight }

    /// Growth cap: ~6 wrapped lines of transcript, then head-truncation.
    static let maxHeight: CGFloat = 200

    /// The wrapping width of the label column (panel width minus padding).
    static var textWidth: CGFloat { StatusOverlayWindow.overlayWidth - 2 * horizontalPadding }

    /// Tallest label the capped HUD can show.
    static var maxTextHeight: CGFloat { maxHeight - chromeHeight }

    /// Measured wrapped height of `text` at the label's font and column width.
    static func textHeight(of text: String) -> CGFloat {
        guard !text.isEmpty else { return 0 }
        let bounds = (text as NSString).boundingRect(
            with: NSSize(width: textWidth, height: .greatestFiniteMagnitude),
            options: [.usesLineFragmentOrigin, .usesFontLeading],
            attributes: [.font: labelFont]
        )
        return ceil(bounds.height)
    }

    /// Window height that fits `text`: never below the classic HUD height,
    /// never above the cap. Callers should pass already-fitted text (see
    /// `fittedTail`) so height and truncation agree.
    static func hudHeight(forText text: String) -> CGFloat {
        min(maxHeight, max(baseHeight, chromeHeight + textHeight(of: text)))
    }

    /// Longest suffix of `text` that fits within the capped text area,
    /// prefixed with "…" when anything was dropped. Head-truncation keeps the
    /// most recent words visible as the transcript streams in. Returns `text`
    /// unchanged when it already fits.
    static func fittedTail(of text: String) -> String {
        guard textHeight(of: text) > maxTextHeight else { return text }
        let chars = Array(text)
        // Binary search the smallest suffix start whose "…"-prefixed suffix
        // fits. Fitting is monotone in the start index; the empty suffix
        // (start == count) always fits, so `hi` maintains the invariant.
        var lo = 0
        var hi = chars.count
        while lo < hi {
            let mid = (lo + hi) / 2
            let candidate = "…" + String(chars[mid...])
            if textHeight(of: candidate) <= maxTextHeight {
                hi = mid
            } else {
                lo = mid + 1
            }
        }
        var start = hi
        // Snap forward to the next word boundary so the ellipsis never glues
        // onto half a word — unless doing so would eat the whole remainder.
        if start > 0, start < chars.count, !chars[start - 1].isWhitespace {
            if let next = (start..<chars.count).first(where: { chars[$0].isWhitespace }),
               next + 1 < chars.count {
                start = next + 1
            }
        }
        let suffix = String(chars[start...]).drop(while: { $0.isWhitespace })
        return "…" + suffix
    }

    /// Display text + panel height for a state's label in normal mode, in one
    /// decision so they can never disagree.
    static func layout(forText text: String) -> (display: String, height: CGFloat) {
        let display = fittedTail(of: text)
        return (display, hudHeight(forText: display))
    }
}
